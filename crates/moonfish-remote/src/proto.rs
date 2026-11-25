//! The messages inside a session, as JSON.
//!
//! - phone → desktop: [`Request`] `{id, method, params}`
//! - desktop → phone: [`Response`] `{id, ok, result | error}`, or a push:
//!   `{push: "chat", chatId, event}` (an event of a chat the phone opened),
//!   `{push: "chats"}` (the chat list changed: list it again),
//!   `{push: "resync"}` (events were missed: list and open the chats again).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn from(id: u64, r: Result<Value, String>) -> Response {
        match r {
            Ok(result) => Response { id, ok: true, result, error: None },
            Err(e) => Response { id, ok: false, result: Value::Null, error: Some(e) },
        }
    }
}

/// What the desktop sends: an answer, or a push.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Incoming {
    Response(Response),
    Push(Value),
}

/// Methods the server answers itself.
pub const HELLO: &str = "hello";
/// `{chatId}` → the chat's snapshot (from [`crate::server::Host::open_chat`]); its events push
/// from then on, until `chat.close`.
pub const CHAT_OPEN: &str = "chat.open";
pub const CHAT_CLOSE: &str = "chat.close";

/// Methods the server passes to the host. Anything not here (or above) is refused, so a paired
/// phone can do exactly this much.
pub const HOST_METHODS: &[&str] = &[
    // → [{id, name, location, path}]
    "projects.list",
    // {kind} → the provider’s plan limits on the paired desktop.
    "provider.limits",
    // → {agents: [{kind, name, defaultModel, permissionMode}], defaultAgent}
    "agents.list",
    // → {metas, running, busy, waiting}
    "chats.list",
    // → [`Status`]: what's busy and what waits on you, for the phone's alerts. Small, so the
    // phone can ask on every `chats` push while the app is in the background.
    "chats.status",
    // {projectId, agent?, model?, effort?, cwd?} → meta
    "chat.create",
    // {chatId, prompt: {id, text, images: [{mime, data}]}, permission?, mode?}
    "chat.send",
    // The same prompt, queued on the desktop rather than started immediately.
    "chat.enqueue",
    // {chatId, command: {action: "remove" | "send", id}} → whether the item was still queued
    "chat.queue",
    // {chatId, command: {action, objective?}, permission?}
    "chat.goal",
    // {chatId}
    "chat.interrupt",
    // {chatId, taskId}
    "chat.stopTask",
    // {chatId, requestId, decision}
    "chat.answer",
    // {chatId, requestId, answers}
    "chat.answerQuestion",
    // {chatId, model} → meta
    "chat.setModel",
    // {chatId, effort} → meta
    "chat.setEffort",
    // {chatId, mode}
    "chat.setPermission",
    // {chatId, instructions, permission?}
    "chat.compact",
    // {chatId}
    "chat.clear",
    // {chatId, file} → data: URL
    "chat.image",
    "chat.readImage",
    // {chatId, title} → meta
    "chat.rename",
    // {chatId, pinned} → meta
    "chat.pin",
    // {chatId}: ends the agent's process; the next message resumes the session
    "chat.sleep",
    // {chatId}
    "chat.delete",
    // {kind, projectId?} → {models, commands}
    "provider.catalog",
];

/// `chats.status`: the chats that are busy or wait on you, and just enough about them to say
/// so in a notification (the phone's alerts, [`crate::alerts`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Chats mid-turn.
    #[serde(default)]
    pub busy: Vec<String>,
    /// What each chat waiting on you asks first.
    #[serde(default)]
    pub waiting: BTreeMap<String, Need>,
    /// How the last turn ended, for chats whose agent runs but isn't mid-turn: `done`,
    /// `interrupted` or `error`.
    #[serde(default)]
    pub ended: BTreeMap<String, String>,
    /// Chats whose next turn starts by itself (queued messages, an active goal): between two
    /// turns they aren't done.
    #[serde(default)]
    pub continuing: Vec<String>,
    /// The title of each chat above.
    #[serde(default)]
    pub titles: BTreeMap<String, String>,
    /// Made from `chats.list` (a desktop without `chats.status`): no requests, no outcomes.
    #[serde(skip)]
    pub legacy: bool,
}

/// What a chat waits on you for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Need {
    /// `approval`, `plan` or `question`.
    pub kind: String,
    /// Answered with `chat.answer` (an approval or a plan) or `chat.answerQuestion`.
    #[serde(default)]
    pub request_id: Option<String>,
    /// "Run a command", "Edit src/main.rs", or the question.
    #[serde(default)]
    pub title: String,
    /// The command line, or why; the start of a plan.
    #[serde(default)]
    pub detail: Option<String>,
}

impl Status {
    /// The same from a `chats.list` answer, for desktops without `chats.status`.
    pub fn from_list(list: &Value) -> Status {
        let ids = |k: &str| -> Vec<String> {
            list[k].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
        };
        let busy = ids("busy");
        let waiting: BTreeMap<String, Need> = list["waiting"]
            .as_object()
            .map(|w| {
                w.iter().filter_map(|(id, kind)| Some((id.clone(), Need { kind: kind.as_str()?.to_string(), ..Need::default() }))).collect()
            })
            .unwrap_or_default();
        let mut titles = BTreeMap::new();
        let mut continuing = Vec::new();
        for m in list["metas"].as_array().into_iter().flatten() {
            let Some(id) = m["id"].as_str() else { continue };
            if !busy.iter().any(|b| b == id) && !waiting.contains_key(id) {
                continue;
            }
            titles.insert(id.to_string(), m["title"].as_str().unwrap_or_default().to_string());
            let queued = m["queued"].as_array().is_some_and(|q| !q.is_empty());
            if queued || m["goal"]["status"] == "active" {
                continuing.push(id.to_string());
            }
        }
        Status { busy, waiting, ended: BTreeMap::new(), continuing, titles, legacy: true }
    }
}

/// One of a chat's events, with its number (as in the `chat.open` snapshot's `seq`), so the phone
/// can line live events up with a snapshot that arrives by another way.
pub fn chat_push(chat_id: &str, seq: u64, event: &Value) -> Value {
    serde_json::json!({ "push": "chat", "chatId": chat_id, "seq": seq, "event": event })
}
