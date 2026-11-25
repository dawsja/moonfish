use std::sync::{Arc, Mutex};
use std::time::Duration;

use moonfish_remote::alerts::{Action, Alert, AlertKind, Prefs, Tracker};
use moonfish_remote::client::Client;
use moonfish_remote::proto::Status;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::plugin::{Builder, TauriPlugin};
use tauri::{AppHandle, Emitter, Manager, Wry};
use tokio::sync::{mpsc, Notify};

use crate::AppState;

/// While watching, how often the time is checked (the idle grace, giving up on a lost desktop).
const TICK: Duration = Duration::from_secs(15);
/// The Android side answers at once; a call that doesn't is dropped rather than wait forever.
const NATIVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Except Android's "Allow notifications?" dialog, which waits on the person.
const PERMISSION_TIMEOUT: Duration = Duration::from_secs(300);

/// Saved with the paired desktops.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub enabled: bool,
    /// Also when a chat finishes its turn.
    #[serde(default)]
    pub done: bool,
}

/// What the Settings screen shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertsState {
    pub enabled: bool,
    pub done: bool,
    /// `granted`, `prompt` (Android can still ask), `denied` (only its settings can change it),
    /// or `unsupported` (not the Android app).
    pub permission: String,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ---- The Android side ------------------------------------------------------------------------

/// The notifications and the service, in the order they were asked for.
pub struct Shade {
    jobs: mpsc::UnboundedSender<(String, Action)>,
    #[cfg(target_os = "android")]
    native: tauri::plugin::PluginHandle<Wry>,
}

impl Shade {
    fn act(&self, desktop_id: &str, actions: Vec<Action>) {
        for a in actions {
            let _ = self.jobs.send((desktop_id.to_string(), a));
        }
    }

    async fn call(&self, command: &str, args: Value) -> Result<Value, String> {
        self.call_within(NATIVE_TIMEOUT, command, args).await
    }

    #[cfg(target_os = "android")]
    async fn call_within(&self, timeout: Duration, command: &str, args: Value) -> Result<Value, String> {
        match tokio::time::timeout(timeout, self.native.run_mobile_plugin_async::<Value>(command, args)).await {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_) => Err("Android didn't answer".into()),
        }
    }

    #[cfg(not(target_os = "android"))]
    async fn call_within(&self, _timeout: Duration, _command: &str, _args: Value) -> Result<Value, String> {
        Err("Alerts need the Android app.".into())
    }
}

/// The plugin call for an action.
fn native_call(desktop_id: &str, action: Action) -> (&'static str, Value) {
    match action {
        Action::Alert(a) => {
            let kind = match a.kind {
                AlertKind::Approval => "approval",
                AlertKind::Plan => "plan",
                AlertKind::Question => "question",
                AlertKind::Done => "done",
                AlertKind::Failed => "failed",
            };
            (
                "alert",
                json!({
                    "chatId": a.chat_id, "desktopId": desktop_id, "kind": kind, "title": a.title, "body": a.body,
                    "detail": a.detail, "requestId": a.request_id, "loud": a.loud,
                }),
            )
        }
        Action::Clear(chat_id) => ("clear", json!({ "chatId": chat_id })),
        Action::Watch(w) => ("watch", json!({ "title": w.title, "text": w.text, "lost": w.lost })),
        Action::Release => ("release", json!({})),
        Action::Note(text) => ("note", json!({ "text": text })),
    }
}

pub fn init() -> TauriPlugin<Wry> {
    Builder::new("phone-alerts")
        .setup(|app, _api| {
            let (jobs, mut rx) = mpsc::unbounded_channel::<(String, Action)>();
            app.manage(Shade {
                jobs,
                #[cfg(target_os = "android")]
                native: _api.register_android_plugin("run.moonfish.mobile", "AlertsPlugin")?,
            });
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                listen(&app).await;
                // One at a time, in order: a notification shown and then cleared stays cleared.
                while let Some((desktop, action)) = rx.recv().await {
                    let (command, args) = native_call(&desktop, action);
                    let _ = app.state::<Shade>().call(command, args).await;
                }
            });
            Ok(())
        })
        .build()
}

/// Allow, Deny and taps on notifications come back from Android on this channel.
async fn listen(app: &AppHandle) {
    let a = app.clone();
    let channel = tauri::ipc::Channel::<Value>::new(move |body| {
        if let Ok(msg) = body.deserialize::<Value>() {
            from_android(&a, msg);
        }
        Ok(())
    });
    let _ = app.state::<Shade>().call("listen", json!({ "handler": channel })).await;
}

fn from_android(app: &AppHandle, msg: Value) {
    match msg["kind"].as_str() {
        // A notification was tapped: the app reads which chat (`alerts_take_open`).
        Some("open") => {
            let _ = app.emit("alerts-open", ());
        }
        Some("answer") => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { answer(&app, &msg).await });
        }
        _ => {}
    }
}

/// Allow or Deny on an approval's notification.
async fn answer(app: &AppHandle, msg: &Value) {
    let field = |k: &str| msg[k].as_str().unwrap_or_default().to_string();
    let (chat_id, request_id, desktop_id) = (field("chatId"), field("requestId"), field("desktopId"));
    let allow = msg["decision"] == "allow";
    let live = app.state::<AppState>().live_watcher(Some(&desktop_id));
    let Some((watcher, client)) = live else {
        // This phone follows another desktop now (or none): the chat opens to answer there.
        let alert = Alert {
            chat_id,
            kind: AlertKind::Approval,
            title: "Approval needed".into(),
            body: "Open Moonfish to answer. This phone isn't connected to that desktop now.".into(),
            detail: None,
            request_id: None,
            loud: false,
        };
        app.state::<Shade>().act(&desktop_id, vec![Action::Alert(alert)]);
        return;
    };
    watcher.answering(&chat_id, &request_id, allow);
    let decision = if allow { "allow" } else { "deny" };
    let sent = match client {
        Some(c) => c.call("chat.answer", json!({ "chatId": chat_id, "requestId": request_id, "decision": decision })).await,
        None => Err("Not connected to the desktop.".into()),
    };
    if let Err(why) = sent {
        watcher.answer_failed(&chat_id, &request_id, &why);
    }
}

// ---- Following a desktop ---------------------------------------------------------------------

/// Follows the desktop the phone keeps a session with, from `connect` until another desktop
/// (or `disconnect`, `forget`).
#[derive(Clone)]
pub struct Watcher(Arc<Inner>);

struct Inner {
    jobs: mpsc::UnboundedSender<(String, Action)>,
    saved: Arc<Mutex<crate::Saved>>,
    desktop_id: String,
    /// The demo inside the app: its chats wait forever, so it's never followed.
    demo: bool,
    tracker: Mutex<Tracker>,
    /// The desktop's chat list changed, a session opened, or the settings changed: look again.
    changed: Notify,
}

impl Watcher {
    pub(crate) fn new(app: &AppHandle, saved: Arc<Mutex<crate::Saved>>, desktop_id: &str, desktop_name: &str, demo: bool) -> Watcher {
        Watcher(Arc::new(Inner {
            jobs: app.state::<Shade>().jobs.clone(),
            saved,
            desktop_id: desktop_id.to_string(),
            demo,
            tracker: Mutex::new(Tracker::new(desktop_name)),
            changed: Notify::new(),
        }))
    }

    /// Alerts are off for the demo.
    fn settings(&self) -> Settings {
        let s = self.0.saved.lock().unwrap().alerts;
        Settings { enabled: s.enabled && !self.0.demo, ..s }
    }

    fn act(&self, actions: Vec<Action>) {
        for a in actions {
            let _ = self.0.jobs.send((self.0.desktop_id.clone(), a));
        }
    }

    /// A push from the desktop.
    pub fn push(&self, push: &Value) {
        if matches!(push["push"].as_str(), Some("chats" | "resync")) {
            self.0.changed.notify_one();
        }
    }

    /// Look now: a session opened, or alerts were turned on.
    pub fn poke(&self) {
        self.0.changed.notify_one();
    }

    /// The session dropped.
    pub fn lost(&self) {
        if !self.settings().enabled {
            return;
        }
        let mut t = self.0.tracker.lock().unwrap();
        let actions = t.lost(now_ms());
        self.act(actions);
    }

    /// The chat is open on the phone.
    pub fn seen(&self, chat_id: &str) {
        let mut t = self.0.tracker.lock().unwrap();
        let actions = t.seen(chat_id);
        self.act(actions);
    }

    /// Alerts were turned off, or this desktop isn't followed now.
    pub fn reset(&self) {
        let mut t = self.0.tracker.lock().unwrap();
        let actions = t.reset();
        self.act(actions);
    }

    fn answering(&self, chat_id: &str, request_id: &str, allow: bool) {
        let mut t = self.0.tracker.lock().unwrap();
        let action = t.answering(chat_id, request_id, allow);
        self.act(action.into_iter().collect());
    }

    fn answer_failed(&self, chat_id: &str, request_id: &str, why: &str) {
        let mut t = self.0.tracker.lock().unwrap();
        let action = t.answer_failed(chat_id, request_id, why);
        self.act(action.into_iter().collect());
    }

    /// Runs alongside the session task: asks the desktop what's busy and waiting whenever its
    /// chat list changes (only with alerts on), and checks the time while watching.
    pub async fn run(self, client: Arc<Mutex<Option<Arc<Client>>>>) {
        // The session known to answer `chats.status` with "unknown method" (an older desktop).
        let mut legacy: Option<usize> = None;
        let mut look = true;
        loop {
            if look && self.settings().enabled {
                let session = client.lock().unwrap().clone();
                if let Some(session) = session {
                    if let Some(status) = fetch(&session, &mut legacy).await {
                        // Turned off meanwhile: nothing more to show.
                        let settings = self.settings();
                        if settings.enabled {
                            let mut t = self.0.tracker.lock().unwrap();
                            let actions = t.status(&status, Prefs { done: settings.done }, now_ms());
                            self.act(actions);
                        }
                    }
                }
            }
            let watching = self.0.tracker.lock().unwrap().watching();
            look = tokio::select! {
                _ = self.0.changed.notified() => true,
                _ = tokio::time::sleep(TICK), if watching => false,
            };
            let mut t = self.0.tracker.lock().unwrap();
            let actions = t.tick(now_ms());
            self.act(actions);
        }
    }
}

/// What's busy and waiting on the desktop: `chats.status`, or `chats.list` on a desktop
/// without it (no Allow and Deny then).
async fn fetch(session: &Arc<Client>, legacy: &mut Option<usize>) -> Option<Status> {
    let id = Arc::as_ptr(session) as usize;
    if *legacy != Some(id) {
        match session.call("chats.status", json!({})).await {
            Ok(v) => return serde_json::from_value(v).ok(),
            Err(e) if e.starts_with("unknown method") => *legacy = Some(id),
            Err(_) => return None,
        }
    }
    session.call("chats.list", json!({})).await.ok().map(|v| Status::from_list(&v))
}

// ---- Commands --------------------------------------------------------------------------------

async fn state_of(app: &AppHandle) -> AlertsState {
    let s = app.state::<AppState>().alerts();
    let permission = match app.state::<Shade>().call("permission", json!({})).await {
        Ok(v) => v["state"].as_str().unwrap_or("denied").to_string(),
        Err(_) => "unsupported".into(),
    };
    AlertsState { enabled: s.enabled, done: s.done, permission }
}

#[tauri::command]
pub async fn alerts_state(app: AppHandle) -> AlertsState {
    state_of(&app).await
}

/// Turns alerts on (asking Android for notifications first) or off, and "also when done".
#[tauri::command]
pub async fn alerts_set(app: AppHandle, enabled: Option<bool>, done: Option<bool>) -> Result<AlertsState, String> {
    if enabled == Some(true) {
        let asked = app.state::<Shade>().call_within(PERMISSION_TIMEOUT, "requestPermission", json!({})).await?;
        if asked["state"] != "granted" {
            // Left off: the screen says how to allow them.
            return Ok(state_of(&app).await);
        }
    }
    let state = app.state::<AppState>();
    state.set_alerts(|s| {
        if let Some(e) = enabled {
            s.enabled = e;
        }
        if let Some(d) = done {
            s.done = d;
        }
    });
    if let Some(w) = state.live_watcher(None).map(|(w, _)| w) {
        if enabled == Some(false) {
            w.reset();
        } else {
            w.poke();
        }
    }
    Ok(state_of(&app).await)
}

/// Android's notification settings for Moonfish.
#[tauri::command]
pub async fn alerts_settings(app: AppHandle) -> Result<(), String> {
    app.state::<Shade>().call("openSettings", json!({})).await.map(|_| ())
}

/// The chat a notification tap asked to open (`{chatId, desktopId}`), once.
#[tauri::command]
pub async fn alerts_take_open(app: AppHandle) -> Option<Value> {
    let open = app.state::<Shade>().call("takeOpen", json!({})).await.ok()?;
    open["chatId"].as_str().filter(|c| !c.is_empty())?;
    Some(open)
}

/// The chat is open on screen: its notification goes.
#[tauri::command]
pub fn alerts_seen(app: AppHandle, chat_id: String) {
    if let Some((w, _)) = app.state::<AppState>().live_watcher(None) {
        w.seen(&chat_id);
    }
}
