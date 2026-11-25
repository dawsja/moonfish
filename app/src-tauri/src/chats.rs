//! Chats at runtime: one agent driver per open chat. Every event a driver reports is appended to
//! the chat's log, folded into its meta (title, CLI session id, usage), emitted to the UI as
//! `chat-event` `{ chatId, event }` and fed, numbered, to paired phones ([`Chats::feed`]).
//! [`Chats::snapshot`] is a chat as it is right now, mid-turn included, lined up with that
//! numbering.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use moonfish_agents::driver::{self, Driver, Prompt, StartOptions};
use moonfish_agents::event::{ApprovalKind, ApprovalRequest, ChatEvent, Decision, NoticeLevel, QuestionRequest};
use moonfish_core::chats::{now_ms, title_from, ChatMeta, ChatStore};
use moonfish_core::settings::{AgentKind, Location, PermissionMode, Settings};
use moonfish_remote::server::Feed;
use serde::{Deserialize, Serialize};

#[path = "goals.rs"]
mod goals;
pub(crate) use goals::GoalAction;
#[path = "chat_queue.rs"]
mod chat_queue;
pub(crate) use chat_queue::{QueueAction, SendMode};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::broadcast;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope<'a> {
    chat_id: &'a str,
    event: &'a ChatEvent,
    /// The event's number in the chat (as in [`Snapshot::seq`]).
    seq: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubagentEnvelope<'a> {
    chat_id: &'a str,
    task_id: &'a str,
    event: &'a ChatEvent,
    seq: u64,
}

#[derive(Default)]
struct SubagentLog {
    events: Vec<ChatEvent>,
    busy: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentSnapshot {
    pub events: Vec<ChatEvent>,
    pub seq: u64,
    pub busy: bool,
}

type Slot = Arc<Mutex<Box<dyn Driver>>>;

/// What a chat waits on you for (see [`Chats::attention`]).
pub enum Asking {
    Approval(ApprovalRequest),
    Question(QuestionRequest),
}

struct Activity {
    last: Instant,
    busy: bool,
    stopped: bool,
    /// Background tasks running, as the last `Tasks` said.
    tasks: usize,
    /// What it asks you now, oldest first.
    approvals: Vec<ApprovalRequest>,
    questions: Vec<QuestionRequest>,
    /// How its last turn ended (`done`, `interrupted`, `error`), until the next starts.
    ended: Option<&'static str>,
    /// Questions asked as secrets (a password), by request: their answers are never shown or
    /// logged.
    secret: HashMap<String, Vec<String>>,
}

impl Activity {
    fn new() -> Self {
        Self {
            last: Instant::now(),
            busy: false,
            stopped: false,
            tasks: 0,
            approvals: Vec::new(),
            questions: Vec::new(),
            ended: None,
            secret: HashMap::new(),
        }
    }

    fn observe(&mut self, event: &ChatEvent) {
        self.last = Instant::now();
        match event {
            ChatEvent::UserMessage { id, .. } if !id.starts_with("moonfish-native-goal-") => {
                self.busy = true;
                self.ended = None;
            }
            ChatEvent::TurnStarted { .. } => {
                self.busy = true;
                self.ended = None;
            }
            ChatEvent::Approval { request } => {
                self.busy = true;
                self.approvals.push(request.clone());
            }
            ChatEvent::Question { request } => {
                self.busy = true;
                self.questions.push(request.clone());
            }
            ChatEvent::ApprovalResolved { request_id, .. } => self.approvals.retain(|r| r.request_id != *request_id),
            ChatEvent::QuestionResolved { request_id, .. } => self.questions.retain(|r| r.request_id != *request_id),
            ChatEvent::TurnFinished { .. } | ChatEvent::Exited { .. } | ChatEvent::Error { .. } => {
                self.ended = match event {
                    ChatEvent::TurnFinished { interrupted: false, .. } => Some("done"),
                    ChatEvent::TurnFinished { .. } => Some("interrupted"),
                    ChatEvent::Error { .. } => Some("error"),
                    // An agent that quits mid-turn stopped with an error; one put to sleep between
                    // turns keeps what its last turn said.
                    _ if self.busy => Some("error"),
                    _ => self.ended,
                };
                self.busy = false;
                self.approvals.clear();
                self.questions.clear();
            }
            ChatEvent::Tasks { tasks } => self.tasks = tasks.iter().filter(|t| t.is_running() && t.tracked && t.background).count(),
            _ => {}
        }
        if let ChatEvent::Question { request } = event {
            let secret: Vec<String> = request.questions.iter().filter(|q| q.secret).map(|q| q.id.clone()).collect();
            if !secret.is_empty() {
                self.secret.insert(request.request_id.clone(), secret);
            }
        }
    }

    /// `event`, with the answers to secret questions masked. The agent already has them; the
    /// log, the window, and phones get dots.
    fn redact(&mut self, event: ChatEvent) -> ChatEvent {
        match event {
            ChatEvent::QuestionResolved { request_id, mut answers } => {
                if let Some(ids) = self.secret.remove(&request_id) {
                    for id in ids {
                        if let Some(a) = answers.get_mut(&id) {
                            *a = a.iter().map(|_| "••••••".to_string()).collect();
                        }
                    }
                }
                ChatEvent::QuestionResolved { request_id, answers }
            }
            other => other,
        }
    }

    /// Asleep, the CLI would take its background work down with it.
    fn eligible(&self, minimum: Duration) -> bool {
        !self.stopped && !self.busy && self.tasks == 0 && self.last.elapsed() >= minimum
    }
}

pub struct Chats {
    pub store: ChatStore,
    /// Every chat's text, for search.
    pub index: moonfish_agents::search::Index,
    /// Each running chat's agent, behind its own lock: one chat waiting on its agent never
    /// holds up another. The map's lock is only ever held to look a chat up.
    live: Mutex<HashMap<String, Slot>>,
    goal_runs: Mutex<HashMap<String, u64>>,
    next_goal: AtomicU64,
    stopping: std::sync::atomic::AtomicBool,
    /// Per chat, held through a send (which may start its agent, for seconds): two sends to one
    /// chat go one after the other, while other chats carry on.
    sending: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    activity: Mutex<HashMap<String, Arc<Mutex<Activity>>>>,
    /// Metas of chats with a live driver, updated as events arrive.
    metas: Arc<Mutex<HashMap<String, ChatMeta>>>,
    /// Chats whose CLI restarts before the next message (an effort it only takes at start).
    restart: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Per chat, the log as it's being written (kept while the app runs).
    logs: Mutex<HashMap<String, Arc<Mutex<Log>>>>,
    subagents: Arc<Mutex<HashMap<(String, String), SubagentLog>>>,
    feed: broadcast::Sender<Feed>,
}

/// A chat's log as it's written: what the Compactor holds back, and how many events it has
/// seen since the app started (each event fed to phones carries its number).
#[derive(Default)]
struct Log {
    compactor: Compactor,
    seq: u64,
    queue: chat_queue::QueueState,
}

/// A chat as it is now: its log plus the text still streaming, whether its agent runs and is
/// mid-turn, and the number of the last event in it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub events: Vec<serde_json::Value>,
    pub seq: u64,
    pub live: bool,
    pub busy: bool,
}

/// Writes a chat's events: to its log, to the page, and to phones, in one order.
struct Publisher<R: Runtime> {
    app: AppHandle<R>,
    store: ChatStore,
    feed: broadcast::Sender<Feed>,
    log: Arc<Mutex<Log>>,
    id: String,
}

impl<R: Runtime> Clone for Publisher<R> {
    fn clone(&self) -> Self {
        Publisher { app: self.app.clone(), store: self.store.clone(), feed: self.feed.clone(), log: self.log.clone(), id: self.id.clone() }
    }
}

fn feed_chat(feed: &broadcast::Sender<Feed>, chat_id: &str, seq: u64, event: &ChatEvent) {
    if feed.receiver_count() > 0 {
        if let Ok(event) = serde_json::to_value(event) {
            let _ = feed.send(Feed::Chat { chat_id: chat_id.to_string(), seq, event: Arc::new(event) });
        }
    }
}

impl<R: Runtime> Publisher<R> {
    fn publish(&self, event: &ChatEvent) {
        let mut log = self.log.lock().unwrap();
        for e in log.compactor.push(event) {
            if let Err(err) = self.store.append(&self.id, &e) {
                eprintln!("moonfish: couldn't log chat {}: {err}", self.id);
            }
        }
        log.seq += 1;
        feed_chat(&self.feed, &self.id, log.seq, event);
        let _ = self.app.emit("chat-event", Envelope { chat_id: &self.id, event, seq: log.seq });
        let dispatch = log.queue.observe(event);
        drop(log);
        if dispatch {
            chat_queue::dispatch(&self.app, &self.id);
        }
    }
}

impl Chats {
    pub fn new(store: ChatStore) -> Chats {
        for mut meta in store.list().into_iter().filter(|m| m.agent != AgentKind::Codex) {
            if let Some(goal) = meta.goal.as_mut().filter(|g| g.status == moonfish_core::chats::GoalStatus::Active) {
                goal.status = moonfish_core::chats::GoalStatus::Paused;
                goal.note = Some("Moonfish was closed. Resume to continue.".into());
                let _ = store.save(&meta);
            }
        }
        Chats {
            store,
            index: moonfish_agents::search::Index::new(),
            live: Mutex::new(HashMap::new()),
            goal_runs: Mutex::new(HashMap::new()),
            next_goal: AtomicU64::new(1),
            stopping: std::sync::atomic::AtomicBool::new(false),
            sending: Mutex::new(HashMap::new()),
            activity: Mutex::new(HashMap::new()),
            metas: Arc::new(Mutex::new(HashMap::new())),
            restart: Arc::new(Mutex::new(std::collections::HashSet::new())),
            logs: Mutex::new(HashMap::new()),
            subagents: Arc::new(Mutex::new(HashMap::new())),
            feed: broadcast::channel(4096).0,
        }
    }

    fn remember_permission(&self, chat_id: &str, permission: Option<PermissionMode>) -> Result<(), String> {
        if permission.is_some() {
            self.edit(chat_id, |meta| meta.permission = permission)?;
        }
        Ok(())
    }

    /// Chats left from a removed project whose folder is `project`'s (the same folder added
    /// again) move to it, with the rest of their old project's chats (its worktrees'); returns
    /// how many.
    pub fn adopt(&self, settings: &Settings, project: &moonfish_core::settings::Project) -> usize {
        let known: std::collections::HashSet<&str> = settings.projects.iter().map(|p| p.id.as_str()).collect();
        let same = |a: &str, b: &str| a.trim_end_matches(['/', '\\']) == b.trim_end_matches(['/', '\\']);
        let orphans: Vec<ChatMeta> = self.store.list().into_iter().filter(|c| !known.contains(c.project_id.as_str())).collect();
        let old: std::collections::HashSet<String> =
            orphans.iter().filter(|c| same(&c.cwd, &project.path)).map(|c| c.project_id.clone()).collect();
        let mut n = 0;
        for c in orphans.iter().filter(|c| old.contains(&c.project_id)) {
            if self.edit(&c.id, |m| m.project_id = project.id.clone()).is_ok() {
                n += 1;
            }
        }
        n
    }

    /// Every chat's events, numbered per chat, and list changes.
    pub fn feed(&self) -> broadcast::Receiver<Feed> {
        self.feed.subscribe()
    }

    /// The chat's running agent.
    fn driver(&self, chat_id: &str) -> Option<Slot> {
        self.live.lock().unwrap().get(chat_id).cloned()
    }

    fn turn_active(&self, chat_id: &str) -> bool {
        let activity = self.activity.lock().unwrap().get(chat_id).cloned();
        // An agent can accept a prompt before it has reported its turn ID.
        activity.is_some_and(|a| a.lock().unwrap().busy) || self.driver(chat_id).is_some_and(|d| !d.lock().unwrap().idle())
    }

    fn add_driver(&self, chat_id: &str, driver: Box<dyn Driver>) -> Slot {
        let slot = Arc::new(Mutex::new(driver));
        self.live.lock().unwrap().insert(chat_id.to_string(), slot.clone());
        slot
    }

    /// Takes the chat's agent out of the running ones and stops it.
    fn stop_driver(&self, chat_id: &str) {
        let slot = self.live.lock().unwrap().remove(chat_id);
        if let Some(slot) = slot {
            slot.lock().unwrap().stop();
        }
    }

    fn send_gate(&self, chat_id: &str) -> Arc<Mutex<()>> {
        self.sending.lock().unwrap().entry(chat_id.to_string()).or_default().clone()
    }

    fn log(&self, chat_id: &str) -> Arc<Mutex<Log>> {
        self.logs.lock().unwrap().entry(chat_id.to_string()).or_default().clone()
    }

    fn publisher<R: Runtime>(&self, app: &AppHandle<R>, chat_id: &str) -> Publisher<R> {
        Publisher { app: app.clone(), store: self.store.clone(), feed: self.feed.clone(), log: self.log(chat_id), id: chat_id.to_string() }
    }

    /// The chat list changed (a chat was made, renamed, deleted or finished a turn): the page
    /// and phones list it again.
    pub fn changed<R: Runtime>(&self, app: &AppHandle<R>) {
        let _ = app.emit("chats-changed", ());
        let _ = self.feed.send(Feed::ChatsChanged);
    }

    /// The chat as it is now, mid-turn included.
    pub fn snapshot(&self, chat_id: &str) -> Result<Snapshot, String> {
        let log = self.log(chat_id);
        // A turn can finish between reading the driver's status and reading the log.
        // Read the status again if an event arrived, without locking a driver under the log.
        let (log, live, busy) = loop {
            let seq = log.lock().unwrap().seq;
            let live = self.driver(chat_id).is_some();
            let busy = live && self.turn_active(chat_id);
            let current = log.lock().unwrap();
            if current.seq == seq {
                break (current, live, busy);
            }
        };
        let mut events = self.store.events(chat_id).map_err(|e| e.to_string())?;
        events.extend(log.compactor.held().iter().filter_map(|e| serde_json::to_value(e).ok()));
        Ok(Snapshot { events, seq: log.seq, live, busy })
    }

    pub fn create(
        &self,
        project_id: &str,
        agent: AgentKind,
        model: &str,
        effort: &str,
        fast_mode: Option<bool>,
        cwd: &str,
    ) -> Result<ChatMeta, String> {
        let mut meta = self.store.create(project_id, agent, model, cwd).map_err(|e| e.to_string())?;
        if !effort.is_empty() || fast_mode.is_some() {
            meta.fast_mode = fast_mode;
            meta.effort = effort.to_string();
            self.store.save(&meta).map_err(|e| e.to_string())?;
        }
        Ok(meta)
    }

    /// Saves a prompt's images with the chat (so a reopened chat still shows them); returns the
    /// file names, in order.
    pub(crate) fn save_images(&self, chat_id: &str, prompt: &Prompt) -> Vec<String> {
        use base64::Engine;
        let Ok(dir) = self.store.images_dir(chat_id) else { return vec![] };
        prompt
            .images
            .iter()
            .enumerate()
            .filter_map(|(i, img)| {
                let ext = match img.mime.as_str() {
                    "image/png" => "png",
                    "image/jpeg" => "jpg",
                    "image/gif" => "gif",
                    "image/webp" => "webp",
                    _ => "img",
                };
                let bytes = base64::engine::general_purpose::STANDARD.decode(img.data.as_bytes()).ok()?;
                let name = format!("{}-{i}.{ext}", now_ms());
                std::fs::write(dir.join(&name), bytes).ok()?;
                Some(name)
            })
            .collect()
    }

    /// A saved image as a `data:` URL.
    pub fn image(&self, chat_id: &str, file: &str) -> Result<String, String> {
        use base64::Engine;
        let path = self.store.image_path(chat_id, file).map_err(|e| e.to_string())?;
        let bytes = std::fs::read(&path).map_err(|_| "that image is gone".to_string())?;
        let mime = match file.rsplit('.').next() {
            Some("png") => "image/png",
            Some("jpg") => "image/jpeg",
            Some("gif") => "image/gif",
            Some("webp") => "image/webp",
            _ => "application/octet-stream",
        };
        Ok(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
    }

    /// Sends `prompt`, starting the chat's agent first if it isn't running.
    pub fn send<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        settings: &Settings,
        chat_id: &str,
        mut prompt: Prompt,
        permission: Option<PermissionMode>,
        mode: SendMode,
    ) -> Result<(), String> {
        let gate = self.send_gate(chat_id);
        let _gate = gate.lock().unwrap();
        let meta = self.store.get(chat_id).map_err(crate::err)?;
        let busy = self.turn_active(chat_id);
        let pending = !meta.queued.is_empty() && self.log(chat_id).lock().unwrap().queue.can_dispatch(&meta);
        if mode != SendMode::Steer && (busy || pending || chat_queue::goal_active(&meta) || mode == SendMode::Queue) {
            self.enqueue_locked(app, chat_id, prompt)?;
            chat_queue::dispatch(app, chat_id);
            return Ok(());
        }
        if mode == SendMode::Steer && busy && meta.agent.is_acp() {
            self.stop_locked(app, settings, chat_id)?;
        }
        if !prompt.images.is_empty() && prompt.files.is_empty() {
            prompt.files = self.save_images(chat_id, &prompt);
        }
        self.remember_permission(chat_id, permission)?;
        self.send_locked(app, settings, chat_id, prompt)
    }

    fn send_locked<R: Runtime>(&self, app: &AppHandle<R>, settings: &Settings, chat_id: &str, prompt: Prompt) -> Result<(), String> {
        // An effort the CLI only takes at start: restart it (resuming) now that it's idle.
        if self.restart.lock().unwrap().contains(chat_id) {
            let ready = self.driver(chat_id).is_some_and(|d| {
                let d = d.lock().unwrap();
                d.idle() && d.background() == 0
            });
            if ready {
                // The old agent's last words (its `exited`) mustn't land in the new one's turn.
                self.silence(chat_id);
                self.stop_driver(chat_id);
                self.restart.lock().unwrap().remove(chat_id);
            }
        }
        let slot = match self.driver(chat_id) {
            Some(slot) => slot,
            None => {
                let driver = self.start(app, settings, chat_id)?;
                self.add_driver(chat_id, driver)
            }
        };
        {
            let mut metas = self.metas.lock().unwrap();
            if let Some(m) = metas.get_mut(chat_id) {
                if m.title.is_empty() {
                    m.title = title_from(&prompt.text);
                    self.changed(app);
                }
                m.updated_at = now_ms();
                let _ = self.store.save(m);
            }
        }
        let sent = slot.lock().unwrap().send(prompt.clone());
        match sent {
            // The CLI exited since the last message: start it again, resuming its session.
            Err(e) if e.contains("isn't running") => {
                self.live.lock().unwrap().remove(chat_id);
                let mut driver = self.start(app, settings, chat_id)?;
                let r = driver.send(prompt);
                self.add_driver(chat_id, driver);
                r
            }
            r => {
                if r.is_err() {
                    if let Some(activity) = self.activity.lock().unwrap().get(chat_id).cloned() {
                        activity.lock().unwrap().busy = false;
                    }
                }
                r
            }
        }
    }

    fn start<R: Runtime>(&self, app: &AppHandle<R>, settings: &Settings, chat_id: &str) -> Result<Box<dyn Driver>, String> {
        let meta = self.store.get(chat_id).map_err(crate::err)?;
        let project = settings.projects.iter().find(|p| p.id == meta.project_id).ok_or("this chat's project was removed")?;
        let provider = settings.providers.get(meta.agent);
        if !provider.enabled {
            return Err("This agent is turned off in Settings under Providers.".into());
        }
        let model = Some(meta.model.clone())
            .filter(|m| !m.is_empty())
            .or_else(|| Some(project.default_model.clone()).filter(|m| !m.is_empty()))
            .or_else(|| Some(provider.default_model.clone()).filter(|m| !m.is_empty()));
        let opts = StartOptions {
            kind: meta.agent,
            cwd: if meta.cwd.is_empty() { project.path.clone() } else { meta.cwd.clone() },
            location: project.location.clone(),
            binary: provider.binary_on(&project.location),
            model,
            effort: Some(meta.effort.clone()).filter(|e| !e.is_empty()),
            fast_mode: meta.fast_mode,
            permission: meta.permission.unwrap_or(provider.permission_mode),
            resume: meta.agent_session_id.clone(),
            env: crate::secrets::resolve(&crate::secrets::scope(meta.agent), &provider.env),
        };
        // A stopped distro starts here (a few seconds), so a missing or broken one says so
        // plainly instead of as an agent that exited.
        if let Location::Wsl { distro } = &project.location {
            moonfish_transport::wsl::start(distro)?;
        }
        {
            let mut metas = self.metas.lock().unwrap();
            // Rename and pin can arrive from another device while the agent starts.
            let mut latest = self.store.get(chat_id).map_err(crate::err)?;
            if latest.sleeping {
                latest.sleeping = false;
                self.store.save(&latest).map_err(crate::err)?;
            }
            metas.insert(chat_id.to_string(), latest);
        }

        let (publisher, metas, feed) = (self.publisher(app, chat_id), self.metas.clone(), self.feed.clone());
        let restart = self.restart.clone();
        let subagents = self.subagents.clone();
        let activity = Arc::new(Mutex::new(Activity::new()));
        self.activity.lock().unwrap().insert(chat_id.to_string(), activity.clone());
        let sink: driver::Sink = Arc::new(move |event: ChatEvent| {
            let mut activity = activity.lock().unwrap();
            if activity.stopped {
                return;
            }
            if let ChatEvent::SubagentEvent { task_id, event } = event {
                drop(activity);
                let ev = *event;
                let seq = {
                    let mut map = subagents.lock().unwrap();
                    let log = map.entry((publisher.id.clone(), task_id.clone())).or_default();
                    match &ev {
                        ChatEvent::TurnStarted { .. } | ChatEvent::UserMessage { .. } => log.busy = true,
                        ChatEvent::TurnFinished { .. } | ChatEvent::Exited { .. } | ChatEvent::Error { .. } => log.busy = false,
                        _ => {}
                    }
                    log.events.push(ev.clone());
                    log.events.len() as u64
                };
                let _ = publisher.app.emit(
                    "subagent-event",
                    SubagentEnvelope { chat_id: &publisher.id, task_id: &task_id, event: &ev, seq },
                );
                return;
            }
            let tasks_before = activity.tasks;
            activity.observe(&event);
            let mut event = activity.redact(event);
            if let ChatEvent::UserMessage { id, text, .. } = &mut event {
                if id.starts_with(moonfish_agents::goal::PROMPT_PREFIX) {
                    if let Some((_, objective)) = text.split_once('\n') {
                        if let Some((objective, _)) = objective.rsplit_once("\n\nWork within the current permission settings.") {
                            *text = format!("/goal {objective}");
                        }
                    }
                }
            }
            // Background work starting or ending changes the list too.
            let tasks_changed = activity.tasks != tasks_before;
            fold(&publisher.store, &metas, &publisher.id, &event);
            mark_restart_if_auth_error(&restart, &publisher.id, &event);
            publisher.publish(&event);
            if let ChatEvent::Exited { message: Some(msg), .. } = &event {
                if session_gone(msg) {
                    let note = ChatEvent::Notice {
                        level: NoticeLevel::Warn,
                        text: concat!(
                            "The agent no longer has this chat's earlier session. ",
                            "Send your message again to continue in a new session without earlier context."
                        )
                        .into(),
                    };
                    publisher.publish(&note);
                }
            }
            // The list shows what's busy and what waits for an answer: a turn starting or ending,
            // or a question or approval coming or going, changes it.
            if matches!(
                event,
                ChatEvent::TurnStarted { .. }
                    | ChatEvent::TurnFinished { .. }
                    | ChatEvent::Exited { .. }
                    | ChatEvent::Error { .. }
                    | ChatEvent::GoalChanged { .. }
                    | ChatEvent::PermissionChanged { .. }
                    | ChatEvent::UserMessage { .. }
                    | ChatEvent::Approval { .. }
                    | ChatEvent::ApprovalResolved { .. }
                    | ChatEvent::Question { .. }
                    | ChatEvent::QuestionResolved { .. }
            ) || tasks_changed
            {
                let _ = publisher.app.emit("chats-changed", ());
                let _ = feed.send(Feed::ChatsChanged);
            }
        });
        let resumed = opts.resume.is_some();
        match driver::start(opts.clone(), sink.clone()) {
            // The CLI no longer has the saved session (cleaned up, another profile, a new
            // machine): carry on in a new one rather than failing every message from now on.
            Err(e) if resumed && session_gone(&e) => {
                self.forget_session(chat_id, &e, &sink);
                driver::start(StartOptions { resume: None, ..opts }, sink)
            }
            r => r,
        }
    }

    /// The chat's saved CLI session is gone: the next start is a new session, and the chat says
    /// so (the agent won't remember what's above).
    fn forget_session(&self, chat_id: &str, why: &str, sink: &driver::Sink) {
        let _ = self.edit(chat_id, |m| m.agent_session_id = None);
        let why = why.lines().find(|l| !l.trim().is_empty()).unwrap_or(why).trim();
        sink(ChatEvent::Notice {
            level: NoticeLevel::Warn,
            text: format!(
                "The agent no longer has this chat's earlier session. Continuing in a new session without earlier context. Details: {why}"
            ),
        });
    }

    /// Stops one of the chat's background tasks.
    pub fn stop_task(&self, chat_id: &str, task_id: &str) -> Result<(), String> {
        match self.driver(chat_id) {
            Some(d) => d.lock().unwrap().stop_task(task_id),
            None => Err("The agent isn't running; its background work stopped with it.".into()),
        }
    }

    pub fn subagent_snapshot(&self, chat_id: &str, task_id: &str) -> SubagentSnapshot {
        let map = self.subagents.lock().unwrap();
        match map.get(&(chat_id.to_string(), task_id.to_string())) {
            Some(log) => SubagentSnapshot { events: log.events.clone(), seq: log.events.len() as u64, busy: log.busy },
            None => SubagentSnapshot { events: Vec::new(), seq: 0, busy: false },
        }
    }

    pub fn send_subagent(&self, chat_id: &str, task_id: &str, prompt: Prompt) -> Result<(), String> {
        match self.driver(chat_id) {
            Some(d) => d.lock().unwrap().send_subagent(task_id, prompt),
            None => Err("The chat's agent isn't running anymore.".into()),
        }
    }

    pub fn interrupt_subagent(&self, chat_id: &str, task_id: &str) -> Result<(), String> {
        match self.driver(chat_id) {
            Some(d) => d.lock().unwrap().interrupt_subagent(task_id),
            None => Ok(()),
        }
    }

    /// Chats with background work running (between turns, too).
    pub fn background(&self) -> Vec<String> {
        let slots: Vec<(String, Slot)> = self.live.lock().unwrap().iter().map(|(k, d)| (k.clone(), d.clone())).collect();
        slots.into_iter().filter(|(_, d)| d.lock().unwrap().background() > 0).map(|(k, _)| k).collect()
    }

    pub fn interrupt(&self, chat_id: &str) -> Result<(), String> {
        match self.driver(chat_id) {
            Some(d) => d.lock().unwrap().interrupt(),
            None => Ok(()),
        }
    }

    pub fn answer(&self, chat_id: &str, request_id: &str, decision: Decision) -> Result<(), String> {
        match self.driver(chat_id) {
            Some(d) => d.lock().unwrap().answer(request_id, decision),
            None => Err("this chat's agent isn't running anymore".into()),
        }
    }

    pub fn answer_question(
        &self,
        chat_id: &str,
        request_id: &str,
        answers: std::collections::BTreeMap<String, Vec<String>>,
    ) -> Result<(), String> {
        match self.driver(chat_id) {
            Some(d) => d.lock().unwrap().answer_question(request_id, answers),
            None => Err("this chat's agent isn't running anymore".into()),
        }
    }

    /// Changes a chat's meta on disk, and in memory while its agent runs, so the next usage
    /// update doesn't write the old one back.
    pub fn edit(&self, chat_id: &str, f: impl Fn(&mut ChatMeta)) -> Result<ChatMeta, String> {
        let mut metas = self.metas.lock().unwrap();
        let mut meta = self.store.get(chat_id).map_err(|e| e.to_string())?;
        f(&mut meta);
        self.store.save(&meta).map_err(|e| e.to_string())?;
        if let Some(m) = metas.get_mut(chat_id) {
            f(m);
        }
        Ok(meta)
    }

    /// Switches a chat's model: saved for next time, and applied now if its agent runs.
    pub fn set_model(&self, chat_id: &str, model: Option<&str>, effort: Option<&str>, fast_mode: Option<bool>) -> Result<ChatMeta, String> {
        let gate = self.send_gate(chat_id);
        let _guard = gate.lock().unwrap();
        let previous = self.store.get(chat_id).map_err(|e| e.to_string())?;
        let model = model.unwrap_or(&previous.model);
        let changed = previous.model != model;
        // Codex retains a tier on its thread. Explicitly clear it when switching away from
        // a Fast model, even when that switch comes from an older client.
        let speed = fast_mode.or(previous.fast_mode.map(|enabled| enabled && !changed));
        let effort = effort.unwrap_or(&previous.effort);
        if let Some(slot) = self.driver(chat_id) {
            let mut d = slot.lock().unwrap();
            if changed {
                d.set_model(model)?;
            }
            if effort != previous.effort && !d.set_effort(effort)? {
                self.restart.lock().unwrap().insert(chat_id.to_string());
            }
            if (speed != previous.fast_mode || (changed && speed == Some(true))) && !d.set_fast_mode(speed)? {
                self.restart.lock().unwrap().insert(chat_id.to_string());
            }
        }
        self.edit(chat_id, |m| {
            m.model = model.to_string();
            m.effort = effort.to_string();
            m.fast_mode = speed;
        })
    }

    pub fn set_effort(&self, chat_id: &str, effort: &str) -> Result<ChatMeta, String> {
        self.set_model(chat_id, None, Some(effort), None)
    }

    /// Switches a chat's permission mode: remembered for its next messages, applied now if its
    /// agent runs. The change is announced to all clients.
    pub fn set_permission<R: Runtime>(&self, app: &AppHandle<R>, chat_id: &str, mode: PermissionMode) -> Result<(), String> {
        let gate = self.send_gate(chat_id);
        let _gate = gate.lock().unwrap();
        if let Some(state) = app.try_state::<crate::AppState>() {
            if let Ok(meta) = self.store.get(chat_id) {
                let settings = state.settings.lock().unwrap().clone();
                let previous = meta.permission.unwrap_or(settings.providers.get(meta.agent).permission_mode);
                if previous != mode && meta.goal.is_some_and(|g| g.status == moonfish_core::chats::GoalStatus::Active) {
                    self.goal_locked(app, &settings, chat_id, GoalAction::Pause, None)?;
                }
            }
        }
        if let Some(d) = self.driver(chat_id) {
            let mut d = d.lock().unwrap();
            if d.takes_permission(mode) {
                d.set_permission(mode)?;
            } else {
                // Restarted (resuming) with it before the next message.
                self.restart.lock().unwrap().insert(chat_id.to_string());
            }
        }
        self.edit(chat_id, |meta| meta.permission = Some(mode))?;
        self.publisher(app, chat_id).publish(&ChatEvent::PermissionChanged { mode });
        self.changed(app);
        Ok(())
    }

    /// `/compact`: the agent's own way when it has one, else `/compact` as a message.
    pub fn compact<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        settings: &Settings,
        chat_id: &str,
        instructions: &str,
        permission: Option<PermissionMode>,
    ) -> Result<(), String> {
        let gate = self.send_gate(chat_id);
        let _gate = gate.lock().unwrap();
        self.remember_permission(chat_id, permission)?;
        let slot = match self.driver(chat_id) {
            Some(slot) => slot,
            None => {
                let driver = self.start(app, settings, chat_id)?;
                self.add_driver(chat_id, driver)
            }
        };
        let activity = self.activity.lock().unwrap().get(chat_id).cloned();
        let was_busy = activity.as_ref().is_some_and(|a| {
            let mut a = a.lock().unwrap();
            let busy = a.busy;
            a.busy = true;
            a.last = Instant::now();
            busy
        });
        let result = slot.lock().unwrap().compact(instructions);
        if matches!(result, Ok(true)) {
            return Ok(());
        }
        if let Some(a) = activity {
            a.lock().unwrap().busy = was_busy;
        }
        result?;
        let text = format!("/compact {}", instructions.trim()).trim_end().to_string();
        self.send_locked(app, settings, chat_id, Prompt { id: format!("compact-{}", now_ms()), text, images: vec![], files: vec![] })
    }

    /// `/clear`: the agent starts a new session with the next message. The chat keeps what was
    /// said, with a line marking where the agent's memory starts again.
    pub fn clear<R: Runtime>(&self, app: &AppHandle<R>, chat_id: &str) -> Result<(), String> {
        let gate = self.send_gate(chat_id);
        let _gate = gate.lock().unwrap();
        self.close_locked(chat_id);
        self.subagents.lock().unwrap().retain(|(cid, _), _| cid != chat_id);
        self.edit(chat_id, |meta| {
            meta.agent_session_id = None;
            meta.goal = None;
        })?;
        let publisher = self.publisher(app, chat_id);
        publisher.publish(&ChatEvent::GoalChanged { goal: None });
        publisher.publish(&ChatEvent::Exited { code: None, message: None });
        publisher.publish(&ChatEvent::Plan { items: vec![] });
        publisher.publish(&ChatEvent::Context { used_tokens: 0, window_tokens: 0, parts: vec![] });
        let event = ChatEvent::Notice {
            level: NoticeLevel::Info,
            text: "Context cleared. The agent starts fresh from the next message; it no longer sees what's above.".into(),
        };
        publisher.publish(&event);
        self.changed(app);
        Ok(())
    }

    /// Whatever the chat's current agent still reports from now on is dropped (it's being
    /// stopped for good, or replaced by a new one).
    fn silence(&self, chat_id: &str) {
        if let Some(a) = self.activity.lock().unwrap().get(chat_id) {
            a.lock().unwrap().stopped = true;
        }
    }

    /// Ends the chat's agent (it starts again, resumed, on the next message).
    pub fn close(&self, chat_id: &str) {
        let gate = self.send_gate(chat_id);
        let _gate = gate.lock().unwrap();
        self.close_locked(chat_id);
    }

    /// Restarts open drivers matching `(agent, location)` after a fresh sign-in: idle drivers stop
    /// immediately, while busy ones restart before their next message.
    pub fn restart_for_provider(&self, settings: &Settings, agent: AgentKind, location: &Location) {
        for chat_id in self.running() {
            let meta = self.metas.lock().unwrap().get(&chat_id).cloned().or_else(|| self.store.get(&chat_id).ok());
            let Some(meta) = meta else { continue };
            if meta.agent != agent {
                continue;
            }
            let matches_location = settings.projects.iter().find(|p| p.id == meta.project_id).is_some_and(|p| p.location == *location);
            if !matches_location {
                continue;
            }
            let gate = self.send_gate(&chat_id);
            let _gate = gate.lock().unwrap();
            let Some(slot) = self.driver(&chat_id) else { continue };
            let active_goal = self.goal_runs.lock().unwrap().contains_key(&chat_id);
            let busy = self.activity.lock().unwrap().get(&chat_id).cloned().is_some_and(|a| a.lock().unwrap().busy);
            let idle = !active_goal && !busy && {
                let d = slot.lock().unwrap();
                d.idle() && d.background() == 0
            };
            if idle {
                self.silence(&chat_id);
                self.stop_driver(&chat_id);
                self.activity.lock().unwrap().remove(&chat_id);
                self.metas.lock().unwrap().remove(&chat_id);
                self.restart.lock().unwrap().remove(&chat_id);
            } else {
                self.restart.lock().unwrap().insert(chat_id);
            }
        }
    }

    fn close_locked(&self, chat_id: &str) {
        self.hold_queue(chat_id);
        self.silence(chat_id);
        if self.goal_runs.lock().unwrap().remove(chat_id).is_some() {
            let _ = self.edit(chat_id, |meta| {
                if let Some(goal) = meta.goal.as_mut() {
                    goal.status = moonfish_core::chats::GoalStatus::Paused;
                    goal.note = Some("The chat was closed.".into());
                }
            });
        }
        self.stop_driver(chat_id);
        self.activity.lock().unwrap().remove(chat_id);
        self.metas.lock().unwrap().remove(chat_id);
    }

    /// Persist before stopping; refuse active turns and agents without resume support.
    pub fn sleep<R: Runtime>(&self, app: &AppHandle<R>, chat_id: &str, minimum: Duration) -> Result<(), String> {
        self.sleep_session(chat_id, minimum)?;
        self.changed(app);
        Ok(())
    }

    fn sleep_session(&self, chat_id: &str, minimum: Duration) -> Result<(), String> {
        // No message can start a turn while this decides.
        let gate = self.send_gate(chat_id);
        let _gate = gate.lock().unwrap();
        if self.store.get(chat_id).is_ok_and(|m| m.goal.is_some_and(|g| g.status == moonfish_core::chats::GoalStatus::Active)) {
            return Err("Pause the goal before putting the chat to sleep.".into());
        }
        let Some(slot) = self.driver(chat_id) else {
            self.edit(chat_id, |m| m.sleeping = true)?;
            return Ok(());
        };
        {
            let driver = slot.lock().unwrap();
            if !driver.idle() {
                return Err("Wait for the agent to finish before sleeping this chat.".into());
            }
            if driver.background() > 0 {
                return Err("This chat has background work running; sleeping it would stop that.".into());
            }
            if !driver.resumable() {
                return Err("This agent does not support resuming saved sessions.".into());
            }
        }
        let activity = self.activity.lock().unwrap().get(chat_id).cloned().ok_or("No session activity available")?;
        let mut activity = activity.lock().unwrap();
        if !activity.eligible(minimum) {
            return Err("This chat is active or has not been idle long enough.".into());
        }
        let mut metas = self.metas.lock().unwrap();
        let mut meta = self.store.get(chat_id).map_err(|e| e.to_string())?;
        if meta.agent_session_id.as_deref().is_none_or(str::is_empty) {
            return Err("The agent has not saved a resumable session yet.".into());
        }
        meta.sleeping = true;
        self.store.save(&meta).map_err(|e| e.to_string())?;
        activity.stopped = true;
        metas.remove(chat_id);
        drop(metas);
        drop(activity);
        self.stop_driver(chat_id);
        self.activity.lock().unwrap().remove(chat_id);
        self.metas.lock().unwrap().remove(chat_id);
        Ok(())
    }

    pub fn sleep_idle<R: Runtime>(&self, app: &AppHandle<R>, minutes: u32) {
        if minutes == 0 {
            return;
        }
        for id in self.running() {
            let _ = self.sleep(app, &id, Duration::from_secs(u64::from(minutes) * 60));
        }
    }

    pub fn delete(&self, chat_id: &str) -> Result<(), String> {
        let gate = self.send_gate(chat_id);
        let _gate = gate.lock().unwrap();
        self.close_locked(chat_id);
        self.restart.lock().unwrap().remove(chat_id);
        self.logs.lock().unwrap().remove(chat_id);
        self.subagents.lock().unwrap().retain(|(cid, _), _| cid != chat_id);
        // A rename or pin already saving must finish before the chat directory is removed.
        let _metas = self.metas.lock().unwrap();
        self.store.delete(chat_id).map_err(|e| e.to_string())
    }

    pub fn running(&self) -> Vec<String> {
        self.live.lock().unwrap().keys().cloned().collect()
    }

    /// What each running chat waits on the user for: `approval`, `plan` (an approval of a plan)
    /// or `question`. Chats waiting for nothing aren't in it.
    pub fn waiting(&self) -> std::collections::BTreeMap<String, &'static str> {
        self.running()
            .into_iter()
            .filter_map(|id| {
                let activity = self.activity.lock().unwrap().get(&id)?.clone();
                let activity = activity.lock().unwrap();
                if activity.stopped {
                    return None;
                }
                let what = match activity.approvals.first() {
                    Some(r) if r.kind == ApprovalKind::Plan => "plan",
                    Some(_) => "approval",
                    None if !activity.questions.is_empty() => "question",
                    None => return None,
                };
                Some((id, what))
            })
            .collect()
    }

    /// For phones' alerts: what each waiting chat asks first (its oldest approval, else its
    /// oldest question), and how the last turn of each running chat between turns ended.
    pub fn attention(&self) -> (std::collections::BTreeMap<String, Asking>, std::collections::BTreeMap<String, &'static str>) {
        let mut asking = std::collections::BTreeMap::new();
        let mut ended = std::collections::BTreeMap::new();
        for id in self.running() {
            let Some(activity) = self.activity.lock().unwrap().get(&id).cloned() else { continue };
            let activity = activity.lock().unwrap();
            if activity.stopped {
                continue;
            }
            if let Some(r) = activity.approvals.first() {
                asking.insert(id, Asking::Approval(r.clone()));
            } else if let Some(q) = activity.questions.first() {
                asking.insert(id, Asking::Question(q.clone()));
            } else if let (false, Some(how)) = (activity.busy, activity.ended) {
                ended.insert(id, how);
            }
        }
        (asking, ended)
    }

    /// Chats mid-turn right now.
    pub fn busy(&self) -> Vec<String> {
        self.running().into_iter().filter(|id| self.turn_active(id)).collect()
    }

    /// Stops every agent (the app is quitting or updating), after writing out what each chat
    /// was still streaming, so the half-finished answer is there when it's reopened.
    pub fn stop_all(&self) {
        self.stopping.store(true, Ordering::Release);
        let goals: Vec<String> = self.goal_runs.lock().unwrap().drain().map(|(id, _)| id).collect();
        for id in goals {
            let _ = self.edit(&id, |meta| {
                if let Some(goal) = meta.goal.as_mut() {
                    goal.status = moonfish_core::chats::GoalStatus::Paused;
                    goal.note = Some("Moonfish was closed. Resume to continue.".into());
                }
            });
        }
        let logs: Vec<(String, Arc<Mutex<Log>>)> = self.logs.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (id, log) in logs {
            for e in log.lock().unwrap().compactor.flush() {
                let _ = self.store.append(&id, &e);
            }
        }
        let ids: Vec<String> = self.running();
        for id in &ids {
            self.silence(id);
        }
        let all: Vec<Slot> = self.live.lock().unwrap().drain().map(|(_, d)| d).collect();
        for d in all {
            d.lock().unwrap().stop();
        }
    }
}

/// Whether a CLI's error says the session it was asked to resume doesn't exist (as opposed to
/// a timeout or a crash, after which it may still be there).
pub(crate) fn session_gone(e: &str) -> bool {
    let e = e.to_ascii_lowercase();
    ["no conversation found", "no rollout found", "thread not found", "session not found", "no such session", "unknown session"]
        .iter()
        .any(|p| e.contains(p))
}

/// Whether an agent error says it isn't signed in, so its driver should restart before the next
/// message to read freshly saved credentials.
pub(crate) fn auth_error(e: &str) -> bool {
    let e = e.to_ascii_lowercase();
    ["not logged in", "please run /login", "authentication_failed", "authentication required"].iter().any(|p| e.contains(p))
}

fn mark_restart_if_auth_error(restart: &Mutex<std::collections::HashSet<String>>, chat_id: &str, event: &ChatEvent) {
    if let ChatEvent::Error { message } = event {
        if auth_error(message) {
            restart.lock().unwrap().insert(chat_id.to_string());
        }
    }
}

/// Keeps the meta in step with what happened: CLI session id, usage, and an `Exited` agent
/// is forgotten so the next message starts it again.
fn fold(store: &ChatStore, metas: &Mutex<HashMap<String, ChatMeta>>, id: &str, event: &ChatEvent) {
    let mut metas = metas.lock().unwrap();
    let Some(m) = metas.get_mut(id) else { return };
    let changed = match event {
        ChatEvent::PermissionChanged { mode } if m.permission != Some(*mode) => {
            m.permission = Some(*mode);
            true
        }
        ChatEvent::GoalChanged { goal } => {
            if let Some(goal) = goal {
                if m.title.is_empty() {
                    m.title = title_from(&goal.objective);
                }
            }
            m.goal = goal.clone();
            true
        }
        ChatEvent::Started { session_id: Some(s), .. } if m.agent_session_id.as_deref() != Some(s) => {
            m.agent_session_id = Some(s.clone());
            true
        }
        // Claude Code says so on its way out: start the next one fresh, not with the same id.
        ChatEvent::Exited { message: Some(msg), .. } if m.agent_session_id.is_some() && session_gone(msg) => {
            m.agent_session_id = None;
            true
        }
        ChatEvent::TurnFinished { usage, cost_usd, .. } => {
            m.tokens += usage.map(|u| u.fresh()).unwrap_or(0);
            m.cost_usd += cost_usd.unwrap_or(0.0);
            m.updated_at = now_ms();
            true
        }
        _ => false,
    };
    if changed {
        let _ = store.save(m);
    }
}

/// What goes in the chat's log: streamed text and reasoning are held back and written as a
/// whole `Text` / `Reasoning` event when anything else happens. The UI still gets every delta
/// live; the log stays about one line per message instead of one per fragment.
///
/// A message can be split by other events (a task update, the context meter, a steered
/// message) and go on streaming after them. Its text is kept for the whole turn, and each
/// write is the message so far, so the last one written (which replaces the earlier ones when
/// the chat is reopened) is the whole message, not just the part after the split.
#[derive(Default)]
pub(crate) struct Compactor {
    /// In the order the items started.
    held: Vec<Held>,
}

struct Held {
    item_id: String,
    reasoning: bool,
    text: String,
    /// Changed since it was last written.
    dirty: bool,
}

impl Held {
    fn event(&self) -> ChatEvent {
        let (item_id, text) = (self.item_id.clone(), self.text.clone());
        if self.reasoning {
            ChatEvent::Reasoning { item_id, text }
        } else {
            ChatEvent::Text { item_id, text }
        }
    }
}

impl Compactor {
    pub(crate) fn push(&mut self, e: &ChatEvent) -> Vec<ChatEvent> {
        let (item_id, reasoning, text, replace) = match e {
            ChatEvent::TextDelta { item_id, text } => (item_id, false, text, false),
            ChatEvent::ReasoningDelta { item_id, text } => (item_id, true, text, false),
            ChatEvent::Text { item_id, text } => (item_id, false, text, true),
            ChatEvent::Reasoning { item_id, text } => (item_id, true, text, true),
            other => {
                let mut out = self.flush();
                // A turn's messages don't go on after it.
                if matches!(other, ChatEvent::TurnFinished { .. } | ChatEvent::Exited { .. }) {
                    self.held.clear();
                }
                out.push(other.clone());
                return out;
            }
        };
        match self.held.iter_mut().find(|h| h.item_id == *item_id && h.reasoning == reasoning) {
            Some(h) if replace => h.text = text.clone(),
            Some(h) => h.text.push_str(text),
            None => self.held.push(Held { item_id: item_id.clone(), reasoning, text: text.clone(), dirty: true }),
        }
        if let Some(h) = self.held.iter_mut().find(|h| h.item_id == *item_id && h.reasoning == reasoning) {
            h.dirty = true;
        }
        Vec::new()
    }

    /// What's held back and not yet written, as whole events (without letting go of it).
    pub(crate) fn held(&self) -> Vec<ChatEvent> {
        self.held.iter().filter(|h| h.dirty).map(Held::event).collect()
    }

    /// Writes what changed: each such message whole, so far.
    pub(crate) fn flush(&mut self) -> Vec<ChatEvent> {
        self.held
            .iter_mut()
            .filter(|h| h.dirty)
            .map(|h| {
                h.dirty = false;
                h.event()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SleepDriver {
        store: ChatStore,
        id: String,
        stopped: Arc<std::sync::atomic::AtomicBool>,
        resumable: bool,
    }

    impl Driver for SleepDriver {
        fn send(&mut self, _: Prompt) -> Result<(), String> {
            Ok(())
        }
        fn interrupt(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn answer(&mut self, _: &str, _: Decision) -> Result<(), String> {
            Ok(())
        }
        fn resumable(&self) -> bool {
            self.resumable
        }
        fn stop(&mut self) {
            let meta = self.store.get(&self.id).unwrap();
            assert!(meta.sleeping);
            assert_eq!(meta.agent_session_id.as_deref(), Some("provider-session"));
            self.stopped.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn sleep_preserves_session_and_refuses_active_or_unsaved_chats() {
        let root = std::env::temp_dir().join(format!("moonfish-sleep-{}-{}", std::process::id(), now_ms()));
        let chats = Chats::new(ChatStore::new(root.clone()));
        let mut meta = chats.store.create("project", AgentKind::Claude, "model", "/work").unwrap();
        meta.agent_session_id = Some("provider-session".into());
        meta.effort = "high".into();
        chats.store.save(&meta).unwrap();
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let make_driver = |resumable| {
            Box::new(SleepDriver { store: chats.store.clone(), id: meta.id.clone(), stopped: stopped.clone(), resumable })
                as Box<dyn Driver>
        };
        chats.add_driver(&meta.id, make_driver(false));
        let activity = Arc::new(Mutex::new(Activity::new()));
        chats.activity.lock().unwrap().insert(meta.id.clone(), activity.clone());
        assert!(chats.sleep_session(&meta.id, Duration::ZERO).is_err());
        chats.add_driver(&meta.id, make_driver(true));
        let mut unsaved = meta.clone();
        unsaved.agent_session_id = None;
        chats.store.save(&unsaved).unwrap();
        assert!(chats.sleep_session(&meta.id, Duration::ZERO).is_err());
        chats.store.save(&meta).unwrap();
        activity.lock().unwrap().observe(&ChatEvent::UserMessage { id: "m".into(), text: "work".into(), images: vec![], files: vec![] });
        assert!(chats.sleep_session(&meta.id, Duration::ZERO).is_err());
        activity.lock().unwrap().observe(&ChatEvent::TurnFinished {
            turn_id: "t".into(),
            usage: None,
            cost_usd: None,
            duration_ms: None,
            interrupted: false,
        });
        assert!(chats.sleep_session(&meta.id, Duration::from_secs(60)).is_err());
        // A save that fails leaves the agent running.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = root.join(&meta.id);
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
            let refused = chats.sleep_session(&meta.id, Duration::ZERO);
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(refused.is_err());
            assert!(!stopped.load(std::sync::atomic::Ordering::SeqCst));
        }
        activity.lock().unwrap().last = Instant::now() - Duration::from_secs(61);
        chats.sleep_session(&meta.id, Duration::from_secs(60)).unwrap();
        assert!(stopped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(chats.running().is_empty());
        meta.sleeping = true;
        assert_eq!(chats.store.get(&meta.id).unwrap(), meta);
        assert!(activity.lock().unwrap().stopped);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_agent_leaving_plan_mode_updates_the_saved_chat() {
        let dir = tempfile::tempdir().unwrap();
        let chats = Chats::new(ChatStore::new(dir.path()));
        let meta = chats.store.create("project", AgentKind::Claude, "", "").unwrap();
        let id = meta.id.clone();
        chats.metas.lock().unwrap().insert(id.clone(), meta);
        chats.remember_permission(&id, Some(PermissionMode::Plan)).unwrap();
        fold(&chats.store, &chats.metas, &id, &ChatEvent::PermissionChanged { mode: PermissionMode::Ask });
        assert_eq!(chats.metas.lock().unwrap()[&id].permission, Some(PermissionMode::Ask));
        assert_eq!(ChatStore::new(dir.path()).list()[0].permission, Some(PermissionMode::Ask));
    }

    #[test]
    fn secret_answers_are_masked_everywhere_after_the_agent() {
        use moonfish_agents::event::{Question, QuestionRequest};
        let mut a = Activity::new();
        let q = |id: &str, secret| Question {
            id: id.into(),
            header: String::new(),
            question: "?".into(),
            options: vec![],
            multi: false,
            other: true,
            secret,
        };
        a.observe(&ChatEvent::Question {
            request: QuestionRequest { request_id: "r".into(), questions: vec![q("user", false), q("pw", true)] },
        });
        let answers = [("user".to_string(), vec!["me".to_string()]), ("pw".to_string(), vec!["hunter2".to_string()])].into_iter().collect();
        let out = a.redact(ChatEvent::QuestionResolved { request_id: "r".into(), answers });
        let text = serde_json::to_string(&out).unwrap();
        assert!(!text.contains("hunter2") && text.contains("me"), "{text}");
    }

    #[test]
    fn phones_learn_what_a_chat_asks_and_how_its_turn_ended() {
        let root = std::env::temp_dir().join(format!("moonfish-attention-{}-{}", std::process::id(), now_ms()));
        let chats = Chats::new(ChatStore::new(root.clone()));
        let meta = chats.store.create("project", AgentKind::Claude, "model", "/work").unwrap();
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        chats.add_driver(&meta.id, Box::new(SleepDriver { store: chats.store.clone(), id: meta.id.clone(), stopped, resumable: true }));
        let activity = Arc::new(Mutex::new(Activity::new()));
        chats.activity.lock().unwrap().insert(meta.id.clone(), activity.clone());
        let observe = |e: ChatEvent| activity.lock().unwrap().observe(&e);
        let finished =
            |interrupted| ChatEvent::TurnFinished { turn_id: "t".into(), usage: None, cost_usd: None, duration_ms: None, interrupted };

        observe(ChatEvent::TurnStarted { turn_id: "t".into() });
        let approval = ApprovalRequest {
            request_id: "r1".into(),
            item_id: None,
            kind: ApprovalKind::Command,
            title: "Run a command".into(),
            detail: Some("npm test".into()),
            diff: None,
            can_remember: false,
        };
        observe(ChatEvent::Approval { request: approval.clone() });
        let (asking, ended) = chats.attention();
        assert!(matches!(asking.get(&meta.id), Some(Asking::Approval(r)) if *r == approval));
        assert!(ended.is_empty());
        assert_eq!(chats.waiting().get(&meta.id), Some(&"approval"));

        observe(ChatEvent::ApprovalResolved { request_id: "r1".into(), decision: Decision::Allow });
        observe(finished(false));
        let (asking, ended) = chats.attention();
        assert!(asking.is_empty());
        assert_eq!(ended.get(&meta.id), Some(&"done"));

        observe(ChatEvent::UserMessage { id: "m".into(), text: "more".into(), images: vec![], files: vec![] });
        assert!(chats.attention().1.is_empty(), "a new turn hasn't ended");
        observe(finished(true));
        assert_eq!(chats.attention().1.get(&meta.id), Some(&"interrupted"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tells_a_missing_session_from_a_failure() {
        assert!(session_gone("No conversation found with session ID: 1234"));
        assert!(session_gone("thread/resume failed: no rollout found for thread id abc"));
        assert!(!session_gone("codex didn't answer thread/resume"));
        assert!(!session_gone("Claude Code exited"));
    }

    #[test]
    fn compacts_streamed_text() {
        let mut c = Compactor::default();
        let d = |t: &str| ChatEvent::TextDelta { item_id: "a".into(), text: t.into() };
        assert!(c.push(&d("Hel")).is_empty());
        assert_eq!(c.held(), vec![ChatEvent::Text { item_id: "a".into(), text: "Hel".into() }]);
        assert!(c.push(&ChatEvent::ReasoningDelta { item_id: "r".into(), text: "hm".into() }).is_empty());
        assert!(c.push(&d("lo")).is_empty());
        let out = c.push(&ChatEvent::TurnStarted { turn_id: "t".into() });
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], ChatEvent::Text { item_id: "a".into(), text: "Hello".into() });
        assert_eq!(out[1], ChatEvent::Reasoning { item_id: "r".into(), text: "hm".into() });
        assert!(matches!(out[2], ChatEvent::TurnStarted { .. }));
        assert!(c.push(&ChatEvent::TurnStarted { turn_id: "u".into() }).len() == 1);
    }

    /// A message split by another event and streaming on after it is written whole the second
    /// time, so reopening the chat (which keeps the last `text` for an item) shows all of it.
    #[test]
    fn a_split_message_is_written_whole() {
        let mut c = Compactor::default();
        let d = |t: &str| ChatEvent::TextDelta { item_id: "a".into(), text: t.into() };
        c.push(&d("The answer "));
        let split = c.push(&ChatEvent::Tasks { tasks: vec![] });
        assert_eq!(split[0], ChatEvent::Text { item_id: "a".into(), text: "The answer ".into() });
        c.push(&d("is 42."));
        assert_eq!(c.held(), vec![ChatEvent::Text { item_id: "a".into(), text: "The answer is 42.".into() }]);
        let end =
            c.push(&ChatEvent::TurnFinished { turn_id: "t".into(), usage: None, cost_usd: None, duration_ms: None, interrupted: false });
        assert_eq!(end[0], ChatEvent::Text { item_id: "a".into(), text: "The answer is 42.".into() });
        // Unchanged items aren't written again, and the next turn starts clean.
        assert_eq!(c.push(&ChatEvent::TurnStarted { turn_id: "u".into() }).len(), 1);
        c.push(&d("New"));
        assert_eq!(c.held(), vec![ChatEvent::Text { item_id: "a".into(), text: "New".into() }]);
    }

    struct StubDriver {
        idle: bool,
        stopped: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Driver for StubDriver {
        fn send(&mut self, _: Prompt) -> Result<(), String> {
            Ok(())
        }
        fn interrupt(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn answer(&mut self, _: &str, _: Decision) -> Result<(), String> {
            Ok(())
        }
        fn idle(&self) -> bool {
            self.idle
        }
        fn stop(&mut self) {
            self.stopped.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn restarts_provider_drivers_and_marks_auth_errors() {
        let dir = tempfile::tempdir().unwrap();
        let chats = Chats::new(ChatStore::new(dir.path()));
        let mut settings = Settings::default();
        let ssh_loc = Location::Ssh { host: "mac".into() };
        let ssh_proj = settings.add_project(ssh_loc.clone(), "/remote").unwrap();
        let local_proj = settings.add_project(Location::Local, "/local").unwrap();

        let idle_ssh = chats.store.create(&ssh_proj.id, AgentKind::Claude, "", "/remote").unwrap();
        let busy_ssh = chats.store.create(&ssh_proj.id, AgentKind::Claude, "", "/remote").unwrap();
        let other_loc = chats.store.create(&local_proj.id, AgentKind::Claude, "", "/local").unwrap();
        let other_agent = chats.store.create(&ssh_proj.id, AgentKind::Codex, "", "/remote").unwrap();

        let add = |id: &str, idle: bool| {
            let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
            chats.add_driver(id, Box::new(StubDriver { idle, stopped: stopped.clone() }));
            stopped
        };
        let idle_stopped = add(&idle_ssh.id, true);
        let busy_stopped = add(&busy_ssh.id, false);
        let local_stopped = add(&other_loc.id, true);
        let codex_stopped = add(&other_agent.id, true);

        let auth_err = ChatEvent::Error { message: "Not logged in · Please run /login".into() };
        let other_err = ChatEvent::Error { message: "Rate limit exceeded".into() };
        mark_restart_if_auth_error(&chats.restart, &idle_ssh.id, &auth_err);
        mark_restart_if_auth_error(&chats.restart, &other_loc.id, &other_err);
        assert!(chats.restart.lock().unwrap().contains(&idle_ssh.id));
        assert!(!chats.restart.lock().unwrap().contains(&other_loc.id));

        chats.restart_for_provider(&settings, AgentKind::Claude, &ssh_loc);

        assert!(idle_stopped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(chats.driver(&idle_ssh.id).is_none());
        assert!(!chats.restart.lock().unwrap().contains(&idle_ssh.id));

        assert!(!busy_stopped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(chats.driver(&busy_ssh.id).is_some());
        assert!(chats.restart.lock().unwrap().contains(&busy_ssh.id));

        assert!(!local_stopped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!codex_stopped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(chats.driver(&other_loc.id).is_some());
        assert!(chats.driver(&other_agent.id).is_some());
    }
}
