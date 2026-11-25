use std::sync::atomic::Ordering;
use std::time::Duration;

use moonfish_agents::driver::GoalCommand;
use moonfish_agents::goal::{self, Outcome, Progress};
use moonfish_core::chats::{Goal, GoalStatus};

use super::*;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum GoalAction {
    Set { objective: String },
    Pause,
    Resume,
    Clear,
}

impl Chats {
    pub fn goal<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        settings: &Settings,
        chat_id: &str,
        action: GoalAction,
        permission: Option<PermissionMode>,
    ) -> Result<(), String> {
        let gate = self.send_gate(chat_id);
        let _gate = gate.lock().unwrap();
        self.goal_locked(app, settings, chat_id, action, permission)
    }

    pub(super) fn goal_locked<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        settings: &Settings,
        chat_id: &str,
        action: GoalAction,
        permission: Option<PermissionMode>,
    ) -> Result<(), String> {
        let meta = self.store.get(chat_id).map_err(crate::err)?;
        if !matches!(meta.agent, AgentKind::Claude | AgentKind::Codex | AgentKind::Antigravity) {
            return Err("Goals are available for Claude Code, Codex, and Antigravity.".into());
        }
        let previous = meta.goal;
        let active = previous.as_ref().is_some_and(|goal| goal.status == GoalStatus::Active);
        let action = match action {
            GoalAction::Set { objective } => {
                let objective = objective.trim().to_owned();
                if objective.is_empty() || objective.chars().count() > 4000 {
                    return Err("Enter a goal between 1 and 4,000 characters.".into());
                }
                if active {
                    return Err("Pause the active goal before replacing it.".into());
                }
                GoalAction::Set { objective }
            }
            GoalAction::Resume => {
                let goal = previous.as_ref().ok_or("This chat has no goal to resume.")?;
                if goal.status == GoalStatus::Complete {
                    return Err("This goal is complete. Start a new goal to continue.".into());
                }
                if active && (meta.agent != AgentKind::Codex || self.driver(chat_id).is_some()) {
                    return Ok(());
                }
                GoalAction::Resume
            }
            other => other,
        };
        let starting = matches!(action, GoalAction::Set { .. } | GoalAction::Resume);
        if starting && self.turn_active(chat_id) {
            return Err("Wait for the current turn to stop before starting a goal.".into());
        }
        if !starting {
            self.hold_queue(chat_id);
        }
        if meta.agent == AgentKind::Codex {
            if !starting && previous.is_none() && meta.agent_session_id.is_none() {
                return Ok(());
            }
            if starting {
                self.remember_permission(chat_id, permission)?;
            }
            let mut slot = self.driver(chat_id);
            let command = match &action {
                GoalAction::Set { objective } => GoalCommand::Set(objective),
                GoalAction::Pause => GoalCommand::Pause,
                GoalAction::Resume => GoalCommand::Resume,
                GoalAction::Clear => GoalCommand::Clear,
            };
            // Pausing or clearing a saved goal must not start its thread. Resuming the
            // thread with an active goal could otherwise start a turn before the mutation.
            if !starting && slot.is_none() {
                let Some(thread_id) = meta.agent_session_id.as_deref() else { return self.save_goal(app, chat_id, None) };
                let project = settings.projects.iter().find(|p| p.id == meta.project_id).ok_or("This chat's project was removed.")?;
                let provider = settings.providers.get(AgentKind::Codex);
                let binary = provider.binary_on(&project.location);
                let program = if binary.trim().is_empty() { AgentKind::Codex.default_binary() } else { binary.trim() };
                let env = crate::secrets::resolve(&crate::secrets::scope(AgentKind::Codex), &provider.env);
                let (method, params) = moonfish_agents::codex::goal_request(thread_id, command);
                let cwd = if meta.cwd.is_empty() { &project.path } else { &meta.cwd };
                let result = moonfish_agents::account::with_codex(program, &project.location, cwd, &env, |rpc| rpc(method, params))?;
                return self.save_goal(app, chat_id, moonfish_agents::codex::parse_goal(&result["goal"]));
            }
            if starting {
                if let (Some(slot), Some(permission)) = (&slot, permission.or(meta.permission)) {
                    slot.lock().unwrap().set_permission(permission)?;
                }
            }
            if starting && slot.as_ref().is_some_and(|d| d.lock().unwrap().goal_needs_restart()) {
                if slot.as_ref().is_some_and(|d| d.lock().unwrap().background() != 0) {
                    return Err("Wait for background tasks to finish before starting a goal with the new settings.".into());
                }
                self.silence(chat_id);
                self.stop_driver(chat_id);
                slot = None;
            }
            let slot = match slot {
                Some(slot) => slot,
                None => self.add_driver(chat_id, self.start(app, settings, chat_id)?),
            };
            let mut driver = slot.lock().unwrap();
            driver.goal(command)?;
            if let GoalCommand::Set(objective) = command {
                self.publisher(app, chat_id).publish(&ChatEvent::UserMessage {
                    id: format!("moonfish-native-goal-{}", uuid::Uuid::new_v4().simple()),
                    text: format!("/goal {objective}"),
                    images: vec![],
                    files: vec![],
                });
            }
            if active && !starting {
                driver.interrupt()?;
            }
            return Ok(());
        }

        if starting {
            let objective = match action {
                GoalAction::Set { objective } => objective,
                GoalAction::Resume => previous.ok_or("This chat has no goal to resume.")?.objective,
                _ => unreachable!(),
            };
            let generation = self.next_goal.fetch_add(1, Ordering::Relaxed);
            self.remember_permission(chat_id, permission)?;
            let mut goal = Goal { objective, status: GoalStatus::Active, tokens_used: None, token_budget: None, note: None };
            let mut events = self.feed();
            self.save_goal(app, chat_id, Some(goal.clone()))?;
            self.goal_runs.lock().unwrap().insert(chat_id.into(), generation);
            let app = app.clone();
            let chat_id = chat_id.to_owned();
            tauri::async_runtime::spawn_blocking(move || {
                let state = app.state::<crate::AppState>();
                let chats = &state.chats;
                let completion = format!("MOONFISH_GOAL_COMPLETE_{}", uuid::Uuid::new_v4().simple());
                let mut outcome = Outcome::Continue;
                for round in 0..goal::MAX_ROUNDS {
                    let settings = state.settings.lock().unwrap().clone();
                    let id = format!("{}{generation}-{round}", goal::PROMPT_PREFIX);
                    {
                        let gate = chats.send_gate(&chat_id);
                        let _gate = gate.lock().unwrap();
                        if !chats.current_goal(&chat_id, generation) {
                            return;
                        }
                        if chats.driver(&chat_id).is_some_and(|d| !d.lock().unwrap().idle()) {
                            outcome = Outcome::Pause("Another turn started before the goal could continue.");
                            break;
                        }
                        let prompt = Prompt {
                            id: id.clone(),
                            text: goal::prompt(&goal.objective, &completion, round),
                            images: vec![],
                            files: vec![],
                        };
                        if let Err(error) = chats.send_locked(&app, &settings, &chat_id, prompt) {
                            goal.note = Some(error);
                            outcome = Outcome::Pause("The agent could not start.");
                            break;
                        }
                        if !chats.current_goal(&chat_id, generation) {
                            chats.stop_driver(&chat_id);
                            return;
                        }
                    }
                    let mut progress = Progress::default();
                    let mut sent = false;
                    loop {
                        if !chats.current_goal(&chat_id, generation) {
                            return;
                        }
                        match events.try_recv() {
                            Ok(Feed::Chat { chat_id: from, event, .. }) if from == chat_id => {
                                let Ok(event) = serde_json::from_value::<ChatEvent>((*event).clone()) else { continue };
                                if matches!(&event, ChatEvent::UserMessage { id: echoed, .. } if echoed == &id) {
                                    sent = true;
                                }
                                if sent {
                                    if let Some(next) = progress.observe(&event, &completion) {
                                        if next == Outcome::Continue {
                                            break;
                                        }
                                        outcome = next;
                                        break;
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(broadcast::error::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(100)),
                            Err(_) => {
                                outcome = Outcome::Pause("Moonfish lost the agent's progress stream.");
                                break;
                            }
                        }
                    }
                    if outcome != Outcome::Continue {
                        break;
                    }
                }
                let gate = chats.send_gate(&chat_id);
                let _gate = gate.lock().unwrap();
                if !chats.current_goal(&chat_id, generation) {
                    return;
                }
                chats.goal_runs.lock().unwrap().remove(&chat_id);
                match outcome {
                    Outcome::Complete => goal.status = GoalStatus::Complete,
                    Outcome::Pause(reason) => {
                        goal.status = GoalStatus::Paused;
                        if goal.note.is_none() {
                            goal.note = Some(reason.into());
                        }
                    }
                    Outcome::Continue => {
                        goal.status = GoalStatus::Paused;
                        goal.note = Some(format!("The goal reached {} turns. Resume to continue.", goal::MAX_ROUNDS));
                    }
                }
                if let Err(error) = chats.save_goal(&app, &chat_id, Some(goal)) {
                    chats.publisher(&app, &chat_id).publish(&ChatEvent::Error { message: error });
                }
            });
            return Ok(());
        }
        let next = match action {
            GoalAction::Pause => previous.map(|mut goal| {
                if goal.status == GoalStatus::Active {
                    goal.status = GoalStatus::Paused;
                    goal.note = None;
                }
                goal
            }),
            GoalAction::Clear => None,
            _ => unreachable!(),
        };
        self.save_goal(app, chat_id, next)?;
        self.goal_runs.lock().unwrap().remove(chat_id);
        if active {
            self.interrupt(chat_id)?;
        }
        Ok(())
    }

    fn current_goal(&self, chat_id: &str, generation: u64) -> bool {
        self.goal_runs.lock().unwrap().get(chat_id).copied() == Some(generation)
    }

    fn save_goal<R: Runtime>(&self, app: &AppHandle<R>, chat_id: &str, goal: Option<Goal>) -> Result<(), String> {
        self.edit(chat_id, |m| {
            if let Some(goal) = &goal {
                if m.title.is_empty() {
                    m.title = title_from(&goal.objective);
                }
            }
            m.goal = goal.clone();
        })?;
        self.publisher(app, chat_id).publish(&ChatEvent::GoalChanged { goal });
        self.changed(app);
        Ok(())
    }

    pub fn stop_goal_or_turn<R: Runtime>(&self, app: &AppHandle<R>, settings: &Settings, chat_id: &str) -> Result<(), String> {
        let gate = self.send_gate(chat_id);
        let _gate = gate.lock().unwrap();
        self.stop_locked(app, settings, chat_id)
    }

    pub(super) fn stop_locked<R: Runtime>(&self, app: &AppHandle<R>, settings: &Settings, chat_id: &str) -> Result<(), String> {
        self.hold_queue(chat_id);
        if self.store.get(chat_id).is_ok_and(|m| m.goal.is_some_and(|goal| goal.status == GoalStatus::Active)) {
            self.goal_locked(app, settings, chat_id, GoalAction::Pause, None)
        } else {
            self.interrupt(chat_id)
        }
    }
}
