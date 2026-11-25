//! The desktop owns the pending replies. Every operation uses the chat's send gate.
use super::*;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SendMode {
    #[default]
    Normal,
    Queue,
    Steer,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub(crate) enum QueueAction {
    Remove { id: String },
    Send { id: String },
}

#[derive(Default, PartialEq, Eq)]
pub(super) enum QueueState {
    #[default]
    Held,
    Running,
    Ready,
}

impl QueueState {
    pub(super) fn can_dispatch(&self, meta: &ChatMeta) -> bool {
        *self == Self::Ready && meta.goal.as_ref().is_none_or(|g| g.status == moonfish_core::chats::GoalStatus::Complete)
    }

    pub(super) fn observe(&mut self, event: &ChatEvent) -> bool {
        use moonfish_core::chats::GoalStatus;
        match event {
            ChatEvent::UserMessage { id, .. } if !id.starts_with("moonfish-native-goal-") => *self = Self::Running,
            ChatEvent::TurnStarted { .. } => *self = Self::Running,
            ChatEvent::TurnFinished { interrupted: false, .. } if *self != Self::Held => {
                *self = Self::Ready;
                return true;
            }
            ChatEvent::GoalChanged { goal: Some(goal) } if goal.status == GoalStatus::Complete => {
                *self = Self::Ready;
                return true;
            }
            ChatEvent::Error { .. } | ChatEvent::Exited { .. } | ChatEvent::TurnFinished { interrupted: true, .. } => *self = Self::Held,
            ChatEvent::GoalChanged { goal } if goal.as_ref().is_none_or(|g| g.status != GoalStatus::Active) => *self = Self::Held,
            _ => {}
        }
        false
    }
}

pub(super) fn goal_active(meta: &ChatMeta) -> bool {
    meta.goal.as_ref().is_some_and(|g| g.status == moonfish_core::chats::GoalStatus::Active)
}

/// The event can arrive on a driver thread, or during a send that already holds the gate.
pub(super) fn dispatch<R: Runtime>(app: &AppHandle<R>, id: &str) {
    if app.try_state::<crate::AppState>().is_none() {
        return;
    }
    let (app, id) = (app.clone(), id.to_owned());
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<crate::AppState>();
        let chats = &state.chats;
        let gate = chats.send_gate(&id);
        let _gate = gate.lock().unwrap();
        if chats.stopping.load(Ordering::Acquire) {
            return;
        }
        let Ok(meta) = chats.store.get(&id) else { return };
        let Some(item) = meta.queued.first() else { return };
        if !chats.log(&id).lock().unwrap().queue.can_dispatch(&meta) || meta.sleeping || chats.turn_active(&id) {
            return;
        }
        let settings = state.settings.lock().unwrap().clone();
        if let Err(error) = chats.send_queued_locked(&app, &settings, &id, item, false) {
            chats.hold_queue(&id);
            chats.publisher(&app, &id).publish(&ChatEvent::Notice {
                level: NoticeLevel::Warn,
                text: format!("The queued reply is still waiting. Couldn't send it: {error}"),
            });
        }
    });
}

impl Chats {
    pub(super) fn hold_queue(&self, id: &str) {
        self.log(id).lock().unwrap().queue = QueueState::Held;
    }

    pub(super) fn enqueue_locked<R: Runtime>(&self, app: &AppHandle<R>, id: &str, prompt: Prompt) -> Result<(), String> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        crate::remote::check_images(&prompt)?;
        let mut attachments = Vec::with_capacity(prompt.images.len());
        for img in &prompt.images {
            let bytes = STANDARD.decode(&img.data).map_err(crate::err)?;
            if bytes.len() > 5 * 1024 * 1024 {
                return Err("Images must be 5 MB or smaller.".into());
            }
            let ext = img.mime.rsplit('/').next().unwrap_or("").replace("jpeg", "jpg");
            attachments.push((format!("queue-{}.{ext}", uuid::Uuid::new_v4().simple()), bytes));
        }
        let message = moonfish_core::chats::QueuedMessage {
            id: uuid::Uuid::new_v4().to_string(),
            text: prompt.text,
            images: attachments.iter().map(|(file, _)| file.clone()).collect(),
        };
        let result = (|| {
            if !attachments.is_empty() {
                let dir = self.store.images_dir(id).map_err(crate::err)?;
                for (file, bytes) in &attachments {
                    std::fs::write(dir.join(file), bytes).map_err(crate::err)?;
                }
            }
            self.edit(id, |meta| meta.queued.push(message.clone()))
        })();
        if result.is_err() {
            self.remove_queue_images(id, &message);
        }
        result?;
        self.changed(app);
        Ok(())
    }

    /// Identifying items by id keeps concurrent actions on the same reply from sending it twice.
    pub(crate) fn queue_action<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        settings: &Settings,
        id: &str,
        action: QueueAction,
    ) -> Result<bool, String> {
        let gate = self.send_gate(id);
        let _gate = gate.lock().unwrap();
        let meta = self.store.get(id).map_err(crate::err)?;
        let item_id = match &action {
            QueueAction::Remove { id } | QueueAction::Send { id } => id,
        };
        let Some(item) = meta.queued.iter().find(|item| &item.id == item_id) else { return Ok(false) };
        match action {
            QueueAction::Remove { .. } => {
                self.remove_queued_locked(app, id, item)?;
                self.remove_queue_images(id, item);
            }
            QueueAction::Send { .. } => self.send_queued_locked(app, settings, id, item, true)?,
        }
        Ok(true)
    }

    fn send_queued_locked<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        settings: &Settings,
        id: &str,
        item: &moonfish_core::chats::QueuedMessage,
        steer: bool,
    ) -> Result<(), String> {
        let images = item
            .images
            .iter()
            .map(|file| {
                let url = self.image(id, file)?;
                let Some((mime, data)) = url.strip_prefix("data:").and_then(|s| s.split_once(";base64,")) else {
                    return Err("Couldn't read a queued reply's attachment.".into());
                };
                Ok(driver::Image { mime: mime.into(), data: data.into() })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let meta = self.store.get(id).map_err(crate::err)?;
        if steer && meta.agent.is_acp() && self.turn_active(id) {
            self.stop_locked(app, settings, id)?;
        }
        let prompt = Prompt { id: item.id.clone(), text: item.text.clone(), images, files: item.images.clone() };
        self.send_locked(app, settings, id, prompt)?;
        self.remove_queued_locked(app, id, item)
    }

    fn remove_queued_locked<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        id: &str,
        item: &moonfish_core::chats::QueuedMessage,
    ) -> Result<(), String> {
        self.edit(id, |meta| meta.queued.retain(|queued| queued.id != item.id))?;
        self.changed(app);
        Ok(())
    }

    fn remove_queue_images(&self, id: &str, item: &moonfish_core::chats::QueuedMessage) {
        for file in &item.images {
            if let Ok(path) = self.store.image_path(id, file) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish(interrupted: bool) -> ChatEvent {
        ChatEvent::TurnFinished { turn_id: "turn".into(), usage: None, cost_usd: None, duration_ms: None, interrupted }
    }

    #[test]
    fn only_naturally_completed_work_releases_replies() {
        let mut state = QueueState::default();
        assert!(!state.observe(&finish(false)));
        state.observe(&ChatEvent::TurnStarted { turn_id: "turn".into() });
        assert!(state.observe(&finish(false)));
        state.observe(&ChatEvent::TurnStarted { turn_id: "next".into() });
        assert!(!state.observe(&finish(true)));
        assert!(!state.observe(&finish(false)));
        state.observe(&ChatEvent::TurnStarted { turn_id: "another".into() });
        state.observe(&ChatEvent::Error { message: "failed".into() });
        assert!(!state.observe(&finish(false)));
    }

    #[test]
    fn another_finished_turn_keeps_replies_queued_for_an_unfinished_goal() {
        use moonfish_core::chats::{Goal, GoalStatus};
        let mut queue = QueueState::Running;
        assert!(queue.observe(&finish(false)));
        for status in [GoalStatus::Active, GoalStatus::Paused, GoalStatus::Blocked, GoalStatus::UsageLimited, GoalStatus::BudgetLimited] {
            let goal = Goal { objective: "Run tests".into(), status, tokens_used: None, token_budget: None, note: None };
            assert!(!queue.can_dispatch(&ChatMeta { goal: Some(goal), ..Default::default() }));
        }
        assert!(queue.can_dispatch(&ChatMeta::default()));
    }

    #[test]
    fn goal_completion_can_release_replies_with_no_turn_in_progress() {
        let mut state = QueueState::default();
        state.observe(&ChatEvent::GoalChanged { goal: None });
        assert!(!state.observe(&finish(false)));
        let goal = moonfish_core::chats::Goal {
            objective: "Run tests".into(),
            status: moonfish_core::chats::GoalStatus::Complete,
            tokens_used: None,
            token_budget: None,
            note: None,
        };
        assert!(state.observe(&ChatEvent::GoalChanged { goal: Some(goal) }));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_host_keeps_attachments_and_drains_one_reply_after_the_goal() {
        use std::sync::atomic::AtomicBool;

        struct TestDriver {
            busy: Arc<AtomicBool>,
            reject: Arc<AtomicBool>,
            sent: Arc<Mutex<Vec<Prompt>>>,
            sink: driver::Sink,
        }

        impl Driver for TestDriver {
            fn send(&mut self, prompt: Prompt) -> Result<(), String> {
                if self.reject.load(Ordering::Acquire) {
                    return Err("not accepted".into());
                }
                self.busy.store(true, Ordering::Release);
                (self.sink)(ChatEvent::TurnStarted { turn_id: prompt.id.clone() });
                self.sent.lock().unwrap().push(prompt);
                Ok(())
            }
            fn interrupt(&mut self) -> Result<(), String> {
                Ok(())
            }
            fn answer(&mut self, _: &str, _: Decision) -> Result<(), String> {
                Ok(())
            }
            fn stop(&mut self) {}
            fn idle(&self) -> bool {
                !self.busy.load(Ordering::Acquire)
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        let app = tauri::test::mock_app();
        app.manage(crate::AppState::for_tests(settings.clone(), ChatStore::new(dir.path())));
        let handle = app.handle();
        let state = app.state::<crate::AppState>();
        let chats = &state.chats;
        let id = chats.store.create("project", AgentKind::Claude, "", "").unwrap().id;
        let publisher = chats.publisher(handle, &id);
        let busy = Arc::new(AtomicBool::new(true));
        let reject = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(Mutex::new(Vec::new()));
        chats.add_driver(
            &id,
            Box::new(TestDriver {
                busy: busy.clone(),
                reject: reject.clone(),
                sent: sent.clone(),
                sink: {
                    let publisher = publisher.clone();
                    Arc::new(move |event| publisher.publish(&event))
                },
            }),
        );
        let mut goal = moonfish_core::chats::Goal {
            objective: "Run tests".into(),
            status: moonfish_core::chats::GoalStatus::Active,
            tokens_used: None,
            token_budget: None,
            note: None,
        };
        chats.edit(&id, |meta| meta.goal = Some(goal.clone())).unwrap();
        let prompt = |text: &str, images| Prompt { id: "request".into(), text: text.into(), images, files: vec![] };
        chats
            .send(
                handle,
                &settings,
                &id,
                prompt("first", vec![driver::Image { mime: "image/png".into(), data: "aGVsbG8=".into() }]),
                None,
                SendMode::Queue,
            )
            .unwrap();
        chats.send(handle, &settings, &id, prompt("second", vec![]), None, SendMode::Normal).unwrap();
        let saved = ChatStore::new(dir.path()).get(&id).unwrap().queued;
        assert_eq!(saved.iter().map(|item| item.text.as_str()).collect::<Vec<_>>(), ["first", "second"]);
        assert_eq!(chats.image(&id, &saved[0].images[0]).unwrap(), "data:image/png;base64,aGVsbG8=");

        publisher.publish(&ChatEvent::TurnStarted { turn_id: "goal".into() });
        busy.store(false, Ordering::Release);
        publisher.publish(&finish(false));
        {
            let gate = chats.send_gate(&id);
            let _gate = gate.lock().unwrap();
            assert!(sent.lock().unwrap().is_empty());
            goal.status = moonfish_core::chats::GoalStatus::Complete;
            chats.edit(&id, |meta| meta.goal = Some(goal.clone())).unwrap();
            publisher.publish(&ChatEvent::GoalChanged { goal: Some(goal) });
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while chats.store.get(&id).unwrap().queued.len() != 1 {
            assert!(Instant::now() < deadline, "the host did not dispatch the first reply");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert_eq!(sent.lock().unwrap()[0].files, saved[0].images);
        assert!(!chats.queue_action(handle, &settings, &id, QueueAction::Send { id: saved[0].id.clone() }).unwrap());
        reject.store(true, Ordering::Release);
        assert_eq!(chats.queue_action(handle, &settings, &id, QueueAction::Send { id: saved[1].id.clone() }).unwrap_err(), "not accepted");
        assert_eq!(chats.store.get(&id).unwrap().queued, saved[1..]);
        assert!(chats.queue_action(handle, &settings, &id, QueueAction::Remove { id: saved[1].id.clone() }).unwrap());
        assert!(chats.store.get(&id).unwrap().queued.is_empty());
    }
}
