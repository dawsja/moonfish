use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use moonfish_remote::client;
use moonfish_remote::server::{Feed, Server};
use moonfish_remote::Identity;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::Desktop;

const DATA: &str = include_str!("../demo/demo.json");
/// Between the pieces of an answer as it streams in.
const TICK: Duration = Duration::from_millis(45);
/// Before a step that isn't text (a tool starting, finishing).
const STEP: Duration = Duration::from_millis(350);

/// The demo while it runs: dropping it stops its server.
pub struct Demo {
    pub desktop: Desktop,
    _server: Server,
}

/// Starts the demo desktop and pairs this phone (`me`) with it, over loopback.
pub async fn start(me: &Identity) -> Result<Demo, String> {
    let host = Arc::new(Host::new());
    let server = Server::bind(([127, 0, 0, 1], 0).into(), host).await.map_err(|e| e.to_string())?;
    let address = server.local_addr().to_string();
    let code = server.open_pairing().code;
    let paired = client::pair(std::slice::from_ref(&address), &code, "This phone", me).await?;
    let desktop = Desktop {
        id: moonfish_remote::b64(&paired.desktop_key),
        name: paired.desktop_name,
        addresses: vec![address.clone()],
        device_id: paired.device_id,
        paired_at: now(),
        last_address: Some(address),
        demo: true,
    };
    Ok(Demo { desktop, _server: server })
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

struct Chat {
    meta: Value,
    events: Vec<Value>,
    queued: Vec<Value>,
    /// What it waits on the person for: "approval" or "question".
    waiting: Option<String>,
    /// A turn is under way (streaming, or waiting on the person).
    busy: bool,
    /// Stop was pressed during this turn.
    stop: bool,
}

struct Inner {
    data: Value,
    chats: Mutex<Vec<Chat>>,
    tx: broadcast::Sender<Feed>,
    next: AtomicU64,
}

impl Inner {
    fn id(&self, prefix: &str) -> String {
        format!("{prefix}{}", self.next.fetch_add(1, Ordering::Relaxed))
    }

    fn queue_meta(c: &mut Chat) {
        c.meta["queued"] =
            json!(c.queued.iter().map(|prompt| json!({
            "id": prompt["id"], "text": prompt["text"],
            "images": prompt["images"].as_array().map(|a| a.iter().map(|image| &image["mime"]).collect::<Vec<_>>()).unwrap_or_default(),
        })).collect::<Vec<_>>());
    }

    fn submit(&self, chat_id: &str, mut prompt: Value, mode: &Value, permission: &Value) -> Result<Option<(String, Vec<Value>)>, String> {
        if !prompt["id"].is_string() {
            prompt["id"] = json!(self.id("u"));
        }
        let text = prompt["text"].as_str().unwrap_or_default().to_string();
        let (mut was_busy, mut queued) = (false, false);
        self.change(chat_id, |c| {
            was_busy = c.busy;
            if c.busy && mode != "steer" {
                c.queued.push(prompt.clone());
                Self::queue_meta(c);
                queued = true;
                return;
            }
            c.busy = true;
            c.meta["updatedAt"] = json!(now());
            if permission.is_string() {
                c.meta["permission"] = permission.clone();
            }
            if c.meta["title"] == "" {
                c.meta["title"] = json!(text.chars().take(48).collect::<String>());
            }
        })?;
        if queued {
            return Ok(None);
        }
        let images: Vec<&Value> = prompt["images"].as_array().map(|a| a.iter().map(|i| &i["mime"]).collect()).unwrap_or_default();
        self.emit(chat_id, json!({ "type": "userMessage", "id": prompt["id"], "text": text, "images": images, "files": [] }));
        if was_busy {
            return Ok(None);
        }
        let turn = self.id("t");
        self.emit(chat_id, json!({ "type": "turnStarted", "turnId": turn }));
        let reply = json!({ "type": "text", "itemId": self.id("a"), "text": self.data["reply"] });
        Ok(Some((turn, vec![reply])))
    }

    /// Adds an event to a chat and feeds it, numbered, under one lock (as the app does).
    fn emit(&self, chat_id: &str, event: Value) {
        let mut chats = self.chats.lock().unwrap();
        if let Some(c) = chats.iter_mut().find(|c| c.meta["id"] == chat_id) {
            c.events.push(event.clone());
            let _ = self.tx.send(Feed::Chat { chat_id: chat_id.into(), seq: c.events.len() as u64, event: Arc::new(event) });
        }
    }

    /// Changes a chat and tells every session the list changed; returns the chat's meta.
    fn change(&self, chat_id: &str, f: impl FnOnce(&mut Chat)) -> Result<Value, String> {
        let meta = {
            let mut chats = self.chats.lock().unwrap();
            let c = chats.iter_mut().find(|c| c.meta["id"] == chat_id).ok_or("That chat isn't in the demo anymore.")?;
            f(c);
            c.meta.clone()
        };
        let _ = self.tx.send(Feed::ChatsChanged);
        Ok(meta)
    }

    fn stopped(&self, chat_id: &str) -> bool {
        self.chats.lock().unwrap().iter().find(|c| c.meta["id"] == chat_id).is_none_or(|c| c.stop)
    }

    fn list(&self) -> Value {
        let chats = self.chats.lock().unwrap();
        let busy: Vec<&Value> = chats.iter().filter(|c| c.busy).map(|c| &c.meta["id"]).collect();
        let waiting: serde_json::Map<String, Value> =
            chats.iter().filter_map(|c| Some((c.meta["id"].as_str()?.to_string(), json!(c.waiting.as_ref()?)))).collect();
        json!({ "metas": chats.iter().map(|c| &c.meta).collect::<Vec<_>>(), "running": busy, "busy": busy, "waiting": waiting })
    }

    /// Plays the rest of a turn: each step after a pause, text a few words at a time (Stop ends
    /// it there), then the turn's end.
    async fn play(&self, chat_id: &str, turn: String, steps: Vec<Value>) -> bool {
        let started = Instant::now();
        let mut interrupted = false;
        'steps: for step in steps {
            if step["type"] != "text" {
                tokio::time::sleep(STEP).await;
                if self.stopped(chat_id) {
                    interrupted = true;
                    break;
                }
                self.emit(chat_id, step);
                continue;
            }
            let (item, text) = (step["itemId"].clone(), step["text"].as_str().unwrap_or_default());
            let mut said = String::new();
            for piece in pieces(text) {
                tokio::time::sleep(TICK).await;
                if self.stopped(chat_id) {
                    interrupted = true;
                    if !said.is_empty() {
                        self.emit(chat_id, json!({ "type": "text", "itemId": item, "text": said }));
                    }
                    break 'steps;
                }
                said.push_str(piece);
                self.emit(chat_id, json!({ "type": "textDelta", "itemId": item, "text": piece }));
            }
            self.emit(chat_id, json!({ "type": "text", "itemId": item, "text": text }));
        }
        let ms = started.elapsed().as_millis() as u64;
        self.emit(
            chat_id,
            json!({ "type": "turnFinished", "turnId": turn, "usage": null, "costUsd": null, "durationMs": ms, "interrupted": interrupted }),
        );
        let _ = self.change(chat_id, |c| {
            c.busy = false;
            c.stop = false;
            c.meta["updatedAt"] = json!(now());
        });
        !interrupted
    }
}

/// A few words at a time, the way an answer arrives.
fn pieces(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut words) = (0, 0);
    for (i, c) in text.char_indices() {
        if c == ' ' || c == '\n' {
            words += 1;
            if words == 3 {
                out.push(&text[start..=i]);
                (start, words) = (i + 1, 0);
            }
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// The scripted desktop behind the demo's server.
struct Host {
    identity: Identity,
    /// The one phone it pairs with: this one.
    phone: Mutex<Option<[u8; 32]>>,
    inner: Arc<Inner>,
    runtime: tokio::runtime::Handle,
}

impl Host {
    /// Must be made inside a tokio runtime (its answers stream from tasks on it).
    fn new() -> Host {
        let mut data: Value = serde_json::from_str(DATA).expect("demo.json");
        let started = now();
        let chats: Vec<Chat> = data["chats"]
            .take()
            .as_array_mut()
            .map(std::mem::take)
            .unwrap_or_default()
            .into_iter()
            .map(|mut c| {
                // `ago` (minutes before now) becomes the times a real chat has.
                let at = started.saturating_sub(c["meta"]["ago"].as_u64().unwrap_or(0) * 60_000);
                let mut meta = c["meta"].take();
                if let Some(m) = meta.as_object_mut() {
                    m.remove("ago");
                    m.insert("createdAt".into(), json!(at));
                    m.insert("updatedAt".into(), json!(at));
                }
                let waiting = c["waiting"].as_str().map(String::from);
                let events = c["events"].as_array_mut().map(std::mem::take).unwrap_or_default();
                Chat { meta, events, queued: vec![], busy: waiting.is_some(), waiting, stop: false }
            })
            .collect();
        let inner = Inner { data, chats: Mutex::new(chats), tx: broadcast::channel(1024).0, next: AtomicU64::new(1) };
        Host { identity: Identity::generate(), phone: Mutex::new(None), inner: Arc::new(inner), runtime: tokio::runtime::Handle::current() }
    }

    /// Starts the rest of a turn on the runtime.
    fn play(&self, chat_id: &str, turn: String, steps: Vec<Value>) {
        let inner = self.inner.clone();
        let chat_id = chat_id.to_owned();
        self.runtime.spawn(async move {
            let mut next = Some((turn, steps));
            while let Some((turn, steps)) = next.take() {
                if !inner.play(&chat_id, turn, steps).await {
                    break;
                }
                let mut prompt = None;
                let _ = inner.change(&chat_id, |c| {
                    if !c.busy && !c.queued.is_empty() {
                        prompt = Some(c.queued.remove(0));
                        Inner::queue_meta(c);
                    }
                });
                if let Some(prompt) = prompt {
                    next = inner.submit(&chat_id, prompt, &Value::Null, &Value::Null).unwrap_or_default();
                }
            }
        });
    }

    /// The demo's steps named `key` (a list of events in demo.json).
    fn steps(&self, key: &str) -> Vec<Value> {
        self.inner.data[key].as_array().cloned().unwrap_or_default()
    }

    fn create(&self, p: &Value) -> Result<Value, String> {
        let inner = &self.inner;
        let project = inner.data["projects"]
            .as_array()
            .and_then(|all| all.iter().find(|x| x["id"] == p["projectId"]))
            .ok_or("That project isn't in the demo.")?;
        let agent = [&p["agent"], &project["defaultAgent"], &inner.data["agents"]["defaultAgent"]]
            .into_iter()
            .find_map(|a| a.as_str())
            .unwrap_or("claude")
            .to_string();
        let default_model = inner.data["agents"]["agents"]
            .as_array()
            .and_then(|all| all.iter().find(|a| a["kind"] == agent.as_str()))
            .map(|a| a["defaultModel"].clone())
            .unwrap_or(json!(""));
        let model = if p["model"].is_string() { p["model"].clone() } else { default_model };
        let at = now();
        let meta = json!({
            "id": inner.id("c-new-"), "projectId": project["id"], "agent": agent, "effort": p["effort"].as_str().unwrap_or(""),
            "model": model, "title": "", "cwd": project["path"], "agentSessionId": null, "createdAt": at, "updatedAt": at,
            "tokens": 0, "costUsd": 0.0, "pinned": false,
        });
        let started = json!({ "type": "started", "sessionId": null, "model": model, "cwd": project["path"] });
        inner.chats.lock().unwrap().push(Chat {
            meta: meta.clone(),
            events: vec![started],
            queued: vec![],
            waiting: None,
            busy: false,
            stop: false,
        });
        let _ = inner.tx.send(Feed::ChatsChanged);
        Ok(meta)
    }

    /// An answer to what the chat was waiting on: the request is resolved and the turn goes on.
    fn resolve(&self, chat_id: &str, kind: &str, resolved: Value, steps: Vec<Value>) -> Result<Value, String> {
        let mut waiting = false;
        self.inner.change(chat_id, |c| {
            waiting = c.waiting.as_deref() == Some(kind);
            if waiting {
                c.waiting = None;
            }
        })?;
        if !waiting {
            return Err("That was already answered.".into());
        }
        self.inner.emit(chat_id, resolved);
        self.play(chat_id, self.inner.id("t"), steps);
        Ok(Value::Null)
    }
}

impl moonfish_remote::server::Host for Host {
    fn identity(&self) -> Identity {
        self.identity.clone()
    }

    fn desktop_name(&self) -> String {
        self.inner.data["desktopName"].as_str().unwrap_or("Demo computer").to_string()
    }

    fn device(&self, key: &[u8; 32]) -> Option<String> {
        (self.phone.lock().unwrap().as_ref() == Some(key)).then(|| "demo".to_string())
    }

    fn pair(&self, _name: &str, key: &[u8; 32]) -> Result<String, String> {
        *self.phone.lock().unwrap() = Some(*key);
        Ok("demo".into())
    }

    fn call(&self, _device: &str, method: &str, p: Value) -> Result<Value, String> {
        let inner = &self.inner;
        let chat = p["chatId"].as_str().unwrap_or_default();
        match method {
            "projects.list" => Ok(inner.data["projects"].clone()),
            "agents.list" => Ok(inner.data["agents"].clone()),
            "chats.list" => Ok(inner.list()),
            "provider.limits" => {
                let kind = p["kind"].as_str().unwrap_or("codex");
                let limits = json!({
                    "windows": [
                        {"label": "5-hour window", "usedPercent": 35, "resetsAt": null},
                        {"label": "Weekly window", "usedPercent": 60, "resetsAt": null}
                    ],
                    "extra": [],
                    "error": null,
                    "missing": false,
                    "accountEmail": "demo@moonfish.app",
                    "accountPlan": "pro"
                });
                Ok(json!({
                    "windows": limits["windows"],
                    "extra": limits["extra"],
                    "error": null,
                    "missing": false,
                    "accountEmail": "demo@moonfish.app",
                    "accountPlan": "pro",
                    "accounts": [{
                        "agent": kind,
                        "email": "demo@moonfish.app",
                        "plan": "pro",
                        "locations": ["This computer"],
                        "limits": limits
                    }]
                }))
            }
            "provider.catalog" => {
                let models = &inner.data["models"][p["kind"].as_str().unwrap_or_default()];
                Ok(json!({ "models": if models.is_array() { models.clone() } else { json!([]) }, "commands": [] }))
            }
            "chat.create" => self.create(&p),
            "chat.send" | "chat.enqueue" => {
                if let Some((turn, steps)) = inner.submit(chat, p["prompt"].clone(), &p["mode"], &p["permission"])? {
                    self.play(chat, turn, steps);
                }
                Ok(Value::Null)
            }
            "chat.queue" => {
                let command = &p["command"];
                let mut prompt = None;
                inner.change(chat, |c| {
                    if let Some(index) = c.queued.iter().position(|p| p["id"] == command["id"]) {
                        prompt = Some(c.queued.remove(index));
                        Inner::queue_meta(c);
                    }
                })?;
                let Some(prompt) = prompt else { return Ok(Value::Bool(false)) };
                if command["action"] == "send" {
                    if let Some((turn, steps)) = inner.submit(chat, prompt, &json!("steer"), &Value::Null)? {
                        self.play(chat, turn, steps);
                    }
                }
                Ok(Value::Bool(true))
            }
            "chat.interrupt" => inner.change(chat, |c| c.stop = c.busy && c.waiting.is_none()).map(|_| Value::Null),
            "chat.answer" => {
                let decision = p["decision"].as_str().unwrap_or("deny");
                let resolved = json!({ "type": "approvalResolved", "requestId": p["requestId"], "decision": decision });
                self.resolve(chat, "approval", resolved, self.steps(if decision == "deny" { "afterDeny" } else { "afterAllow" }))
            }
            "chat.answerQuestion" => {
                let resolved = json!({ "type": "questionResolved", "requestId": p["requestId"], "answers": p["answers"] });
                let reply = json!({ "type": "text", "itemId": inner.id("a"), "text": inner.data["afterAnswer"] });
                self.resolve(chat, "question", resolved, vec![reply])
            }
            "chat.setModel" => inner.change(chat, |c| {
                c.meta["model"] = p["model"].clone();
                c.meta["effort"] = p["effort"].clone();
                if p["fastMode"].is_boolean() {
                    c.meta["fastMode"] = p["fastMode"].clone();
                }
            }),
            "chat.setEffort" => inner.change(chat, |c| c.meta["effort"] = p["effort"].clone()),
            "chat.rename" => inner.change(chat, |c| c.meta["title"] = p["title"].clone()),
            "chat.pin" => inner.change(chat, |c| c.meta["pinned"] = json!(p["pinned"] == true)),
            "chat.setPermission" => {
                inner.change(chat, |c| c.meta["permission"] = p["mode"].clone())?;
                inner.emit(chat, json!({ "type": "permissionChanged", "mode": p["mode"] }));
                Ok(Value::Null)
            }
            "chat.delete" => {
                inner.chats.lock().unwrap().retain(|c| c.meta["id"] != chat);
                let _ = inner.tx.send(Feed::ChatsChanged);
                Ok(Value::Null)
            }
            "chat.clear" | "chat.compact" => {
                inner.emit(chat, json!({ "type": "notice", "level": "info", "text": "The demo keeps its conversation as it is." }));
                Ok(Value::Null)
            }
            "chat.sleep" => Ok(Value::Null),
            "chat.image" | "chat.readImage" => Err("The demo has no images to load.".into()),
            _ => Err(format!("The demo doesn't do {method}.")),
        }
    }

    fn open_chat(&self, chat_id: &str) -> Result<(Value, u64), String> {
        let chats = self.inner.chats.lock().unwrap();
        let c = chats.iter().find(|c| c.meta["id"] == chat_id).ok_or("That chat isn't in the demo anymore.")?;
        let seq = c.events.len() as u64;
        Ok((json!({ "events": c.events, "seq": seq, "live": c.busy, "busy": c.busy }), seq))
    }

    fn feed(&self) -> broadcast::Receiver<Feed> {
        self.inner.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonfish_remote::client::Client;
    use tokio::sync::mpsc;

    /// The demo, and this phone connected to it; chat pushes arrive on the receiver.
    async fn connected() -> (Demo, Client, mpsc::UnboundedReceiver<Value>) {
        let me = Identity::generate();
        let demo = start(&me).await.unwrap();
        assert!(demo.desktop.demo);
        let (tx, rx) = mpsc::unbounded_channel();
        let key = moonfish_remote::key_from_b64(&demo.desktop.id).unwrap();
        let client = Client::connect(&demo.desktop.addresses, &me, &key, move |push| {
            if push["push"] == "chat" {
                let _ = tx.send(push);
            }
        })
        .await
        .unwrap();
        (demo, client, rx)
    }

    /// A chat's pushes up to the end of the turn: their event types, numbered without a gap
    /// from `after`.
    async fn turn(rx: &mut mpsc::UnboundedReceiver<Value>, chat: &str, after: u64) -> Vec<Value> {
        let mut events = Vec::new();
        loop {
            let push = tokio::time::timeout(Duration::from_secs(20), rx.recv()).await.expect("the turn ended").unwrap();
            assert_eq!(push["chatId"], chat);
            assert_eq!(push["seq"], after + events.len() as u64 + 1, "events are numbered in order");
            events.push(push["event"].clone());
            if push["event"]["type"] == "turnFinished" {
                return events;
            }
        }
    }

    fn types(events: &[Value]) -> Vec<&str> {
        let mut out: Vec<&str> = events.iter().filter_map(|e| e["type"].as_str()).collect();
        out.dedup();
        out
    }

    #[test]
    fn splits_an_answer_without_losing_any_of_it() {
        let text = "One two three four five\n\n- six seven\n";
        assert_eq!(pieces(text).concat(), text);
        assert!(pieces(text).len() > 2);
        assert_eq!(pieces(""), Vec::<&str>::new());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lists_what_a_desktop_would() {
        let (_demo, c, _rx) = connected().await;
        assert_eq!(c.desktop_name, "Demo computer");
        assert_eq!(c.call("projects.list", Value::Null).await.unwrap().as_array().unwrap().len(), 2);
        assert_eq!(c.call("agents.list", Value::Null).await.unwrap()["defaultAgent"], "claude");
        let list = c.call("chats.list", Value::Null).await.unwrap();
        assert_eq!(list["metas"].as_array().unwrap().len(), 4);
        assert!(list["metas"][0]["updatedAt"].as_u64().unwrap() > 0 && list["metas"][0].get("ago").is_none());
        assert_eq!(list["waiting"], json!({ "c-checkout": "approval", "c-dates": "question" }));
        assert_eq!(list["busy"], json!(["c-checkout", "c-dates"]));
        let models = c.call("provider.catalog", json!({ "kind": "claude" })).await.unwrap();
        assert_eq!(models["models"][0]["id"], "sonnet");
        assert_eq!(c.call("provider.catalog", json!({ "kind": "antigravity" })).await.unwrap()["models"], json!([]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn provider_limits_returns_usage() {
        let (_demo, c, _rx) = connected().await;
        let limits = c.call("provider.limits", json!({"kind": "codex"})).await.unwrap();
        assert_eq!(limits["windows"][0]["usedPercent"], 35);
        assert_eq!(limits["accounts"][0]["agent"], "codex");
        assert_eq!(limits["accounts"][0]["email"], "demo@moonfish.app");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_approval_is_answered_and_the_turn_goes_on() {
        let (_demo, c, mut rx) = connected().await;
        let snap = c.call("chat.open", json!({ "chatId": "c-checkout" })).await.unwrap();
        let seq = snap["seq"].as_u64().unwrap();
        assert_eq!(snap["events"].as_array().unwrap().len() as u64, seq);
        assert_eq!(snap["events"][seq as usize - 1]["type"], "approval");
        assert_eq!(snap["busy"], true);

        c.call("chat.answer", json!({ "chatId": "c-checkout", "requestId": "q1", "decision": "allow" })).await.unwrap();
        let events = turn(&mut rx, "c-checkout", seq).await;
        assert_eq!(types(&events), ["approvalResolved", "toolStarted", "toolFinished", "textDelta", "text", "turnFinished"]);
        assert_eq!(events.last().unwrap()["interrupted"], false);
        let said: String = events.iter().filter(|e| e["type"] == "textDelta").filter_map(|e| e["text"].as_str()).collect();
        assert_eq!(json!(said), events[events.len() - 2]["text"], "the pieces add up to the answer");

        let list = c.call("chats.list", Value::Null).await.unwrap();
        assert_eq!(list["waiting"], json!({ "c-dates": "question" }));
        assert!(c.call("chat.answer", json!({ "chatId": "c-checkout", "requestId": "q1", "decision": "allow" })).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_queued_reply_is_in_the_list_and_sends_after_the_chat_is_closed() {
        let (_demo, c, _rx) = connected().await;
        let chat = json!({ "chatId": "c-checkout" });
        c.call("chat.open", chat.clone()).await.unwrap();
        for (id, text) in [("keep", "Run the tests"), ("drop", "Skip the tests")] {
            c.call(
                "chat.enqueue",
                json!({
                    "chatId": "c-checkout", "prompt": { "id": id, "text": text, "images": [] }
                }),
            )
            .await
            .unwrap();
        }
        let list = c.call("chats.list", Value::Null).await.unwrap();
        let meta = list["metas"].as_array().unwrap().iter().find(|m| m["id"] == "c-checkout").unwrap();
        assert_eq!(
            meta["queued"],
            json!([
                { "id": "keep", "text": "Run the tests", "images": [] },
                { "id": "drop", "text": "Skip the tests", "images": [] },
            ])
        );
        let remove = json!({ "chatId": "c-checkout", "command": { "action": "remove", "id": "drop" } });
        assert_eq!(c.call("chat.queue", remove.clone()).await.unwrap(), true);
        assert_eq!(c.call("chat.queue", remove).await.unwrap(), false);
        c.call("chat.close", chat.clone()).await.unwrap();
        c.call("chat.answer", json!({ "chatId": "c-checkout", "requestId": "q1", "decision": "allow" })).await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let snap = c.call("chat.open", chat.clone()).await.unwrap();
            let events = snap["events"].as_array().unwrap();
            if events.iter().any(|e| e["type"] == "userMessage" && e["id"] == "keep") {
                assert_eq!(events.iter().filter(|e| e["id"] == "keep").count(), 1);
                assert!(!events.iter().any(|e| e["id"] == "drop"));
                break;
            }
            assert!(Instant::now() < deadline, "the queued reply did not send");
            c.call("chat.close", chat.clone()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_chat_answers_and_stops_when_told() {
        let (_demo, c, mut rx) = connected().await;
        let meta = c.call("chat.create", json!({ "projectId": "p-billing" })).await.unwrap();
        let id = meta["id"].as_str().unwrap().to_string();
        assert_eq!((meta["agent"].as_str(), meta["model"].as_str()), (Some("codex"), Some("codex")));
        let snap = c.call("chat.open", json!({ "chatId": id })).await.unwrap();
        assert_eq!((snap["seq"].as_u64(), snap["busy"].as_bool()), (Some(1), Some(false)));

        let prompt = json!({ "id": "m1", "text": "Is this thing on?", "images": [] });
        c.call("chat.send", json!({ "chatId": id, "prompt": prompt })).await.unwrap();
        let events = turn(&mut rx, &id, 1).await;
        assert_eq!(types(&events), ["userMessage", "turnStarted", "textDelta", "text", "turnFinished"]);
        assert_eq!(events[0]["id"], "m1");
        assert!(events[events.len() - 2]["text"].as_str().unwrap().starts_with("This is Moonfish's demo"));
        let list = c.call("chats.list", Value::Null).await.unwrap();
        let titled = list["metas"].as_array().unwrap().iter().find(|m| m["id"] == id.as_str()).unwrap();
        assert_eq!(titled["title"], "Is this thing on?");
        assert_eq!(list["busy"], json!(["c-checkout", "c-dates"]), "the turn is over");

        // Stop, partway through the next answer.
        let seq = 1 + events.len() as u64;
        c.call("chat.send", json!({ "chatId": id, "prompt": { "id": "m2", "text": "And again", "images": [] } })).await.unwrap();
        tokio::time::sleep(TICK * 6).await;
        c.call("chat.interrupt", json!({ "chatId": id })).await.unwrap();
        let events = turn(&mut rx, &id, seq).await;
        assert_eq!(events.last().unwrap()["interrupted"], true);
        let full = events.iter().rfind(|e| e["type"] == "text").unwrap()["text"].as_str().unwrap();
        assert!(!full.is_empty() && full.len() < 200, "what was said so far is kept: {full}");
    }
}
