//! Claude Code over its stream-json protocol:
//!
//! `claude -p --input-format stream-json --output-format stream-json --verbose
//!  --include-partial-messages --permission-prompt-tool stdio --permission-mode <mode>`
//!
//! We write `user` messages and `control_request`s (initialize, interrupt, set_permission_mode)
//! to stdin. Claude writes `system`/`init`, `stream_event`s (text deltas), whole `assistant`
//! messages (text, thinking, tool_use), `user` messages carrying tool results, `control_request`
//! `can_use_tool` (answered with a `control_response`), and a `result` per turn.

use moonfish_transport::{Proc, Stdin};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

use moonfish_core::settings::{AgentKind, PermissionMode};
use moonfish_transport::Spec;
use serde_json::{json, Value};

use crate::driver::{Driver, DriverResult, Prompt, Sink, StartOptions};
use crate::event::*;

pub struct ClaudeDriver {
    stdin: Arc<Mutex<Option<Stdin>>>,
    child: Arc<Mutex<Option<Proc>>>,
    state: Arc<Mutex<State>>,
    sink: Sink,
    turn: u32,
    /// Started able to switch to Full access (Claude Code only allows that when it's launched
    /// with `--allow-dangerously-skip-permissions`).
    full_access: bool,
}

/// What the reader thread and the driver share.
#[derive(Default)]
struct State {
    /// Approval requests waiting for an answer: request id → (tool input, suggestions).
    pending: HashMap<String, (Value, Value)>,
    /// The turn being run, for `TurnFinished`.
    turn_id: Option<String>,
    /// The `uuid`s of messages sent and not yet answered, oldest first. A message sent while a
    /// turn runs is folded into it at the next tool boundary (its `result` lists both), or, if it
    /// came too late, runs as its own turn after it.
    waiting: Vec<String>,
    seq: u32,
    /// The model from `system/init`.
    model: Option<String>,
    /// AskUserQuestion requests waiting: request id → the tool input.
    questions: HashMap<String, Value>,
    /// `total_cost_usd` is per process; turns report the difference.
    cost_so_far: f64,
    /// Messages whose text arrived as stream deltas (their final text isn't sent again).
    streamed: HashSet<String>,
    /// The current streamed message id and its block kinds by index.
    message_id: String,
    blocks: HashMap<u64, &'static str>,
    alive: bool,
    /// Pending approvals that are ExitPlanMode (approving one leaves plan mode).
    plans: HashSet<String>,
    /// The mode to go back to when a plan is approved: the last one that wasn't plan.
    after_plan: PermissionMode,
    /// `claude_code_version` from `initialize`.
    version: Option<String>,
    /// Background tasks and in-turn subagents (`system` `task_*` messages); non-agent tasks
    /// run inside a turn stay in `inside` and show only if they outlive it.
    tasks: TaskList,
    inside: HashMap<String, BackgroundTask>,
    /// Turns Claude starts by itself (a background task ended and it looks at the result).
    woke: u32,
    /// Set when Claude emits an `authentication_failed` synthetic assistant message in this turn.
    auth_failed: bool,
}

pub fn permission_mode_flag(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "default",
        PermissionMode::AcceptEdits => "acceptEdits",
        PermissionMode::Plan => "plan",
        PermissionMode::FullAccess => "bypassPermissions",
    }
}

impl ClaudeDriver {
    pub fn start(opts: StartOptions, sink: Sink) -> DriverResult<ClaudeDriver> {
        let mut args: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
            permission_mode_flag(opts.permission),
        ]
        .map(String::from)
        .to_vec();
        if opts.permission == PermissionMode::FullAccess {
            args.push("--allow-dangerously-skip-permissions".into());
        }
        if let Some(m) = &opts.model {
            args.extend(["--model".into(), m.clone()]);
        }
        if let Some(e) = opts.effort.as_deref().filter(|e| valid_effort(e)) {
            args.extend(["--effort".into(), e.to_string()]);
        }
        if let Some(fast) = opts.fast_mode {
            args.extend(["--settings".into(), json!({"fastMode": fast}).to_string()]);
        }
        if let Some(r) = &opts.resume {
            args.extend(["--resume".into(), r.clone()]);
        }
        let program = if opts.binary.trim().is_empty() { AgentKind::Claude.default_binary().to_string() } else { opts.binary.clone() };
        let spec = Spec { location: &opts.location, cwd: &opts.cwd, program: &program, args, env: &opts.env };
        let mut child = moonfish_transport::spawn(&spec).map_err(|e| format!("couldn't start Claude Code: {e}"))?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let stdin = Arc::new(Mutex::new(child.stdin.take()));
        let child = Arc::new(Mutex::new(Some(child)));
        let after_plan = if opts.permission == PermissionMode::Plan { PermissionMode::Ask } else { opts.permission };
        let state = Arc::new(Mutex::new(State { alive: true, after_plan, ..Default::default() }));

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
            let (sink, state, child, stdin_for_reader) = (sink.clone(), state.clone(), child.clone(), stdin.clone());
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                let mut tr = Translator::new(sink.clone(), state.clone());
                tr.stdin = Some(stdin_for_reader);
                tr.child = Some(child.clone());
                for line in reader.lines().map_while(Result::ok) {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        tr.handle(&v);
                        if !state.lock().unwrap().alive {
                            break;
                        }
                    }
                }
                state.lock().unwrap().alive = false;
                let code = child.lock().unwrap().as_mut().and_then(|c| c.wait().ok()).and_then(|s| s.code());
                std::thread::sleep(std::time::Duration::from_millis(50));
                // 127: the shell couldn't find the program (on an SSH host, usually not installed).
                let message = if code == Some(127) {
                    Some("Claude Code isn't installed there, or isn't on that machine's PATH.".to_string())
                } else {
                    Some(tail.lock().unwrap().clone()).filter(|m| !m.is_empty())
                };
                let (turn, tasks) = {
                    let mut s = state.lock().unwrap();
                    s.waiting.clear();
                    s.inside.clear();
                    let tasks = s.tasks.stop_all().then(|| s.tasks.event());
                    (s.turn_id.take(), tasks)
                };
                if let Some(turn_id) = turn {
                    sink(ChatEvent::TurnFinished { turn_id, usage: None, cost_usd: None, duration_ms: None, interrupted: true });
                }
                // Its background tasks went with it.
                if let Some(e) = tasks {
                    sink(e);
                }
                sink(ChatEvent::Exited { code, message });
            });
        }

        let d = ClaudeDriver { stdin, child, state, sink, turn: 0, full_access: opts.permission == PermissionMode::FullAccess };
        d.write(&json!({"type": "control_request", "request_id": "moonfish-init", "request": {"subtype": "initialize", "hooks": null}}))?;
        d.write(&json!({"type": "control_request", "request_id": "moonfish-models", "request": {"subtype": "list_models"}}))?;
        Ok(d)
    }

    fn write(&self, v: &Value) -> DriverResult<()> {
        let mut g = self.stdin.lock().unwrap();
        let stdin = g.as_mut().ok_or("Claude Code isn't running")?;
        let mut line = serde_json::to_vec(v).map_err(|e| e.to_string())?;
        line.push(b'\n');
        stdin.write_all(&line).and_then(|_| stdin.flush()).map_err(|_| "Claude Code isn't running".to_string())
    }
}

impl Driver for ClaudeDriver {
    fn send(&mut self, prompt: Prompt) -> DriverResult<()> {
        if !self.state.lock().unwrap().alive {
            return Err("Claude Code isn't running".into());
        }
        self.turn += 1;
        let turn_id = format!("turn-{}", self.turn);
        let uuid = message_uuid(&prompt.id);
        let mut content = vec![json!({"type": "text", "text": prompt.text})];
        for img in &prompt.images {
            content.push(json!({"type": "image", "source": {"type": "base64", "media_type": img.mime, "data": img.data}}));
        }
        (self.sink)(ChatEvent::UserMessage {
            id: prompt.id.clone(),
            text: prompt.text.clone(),
            images: prompt.images.iter().map(|i| i.mime.clone()).collect(),
            files: prompt.files.clone(),
        });
        let (started, tasks) = {
            let mut s = self.state.lock().unwrap();
            s.waiting.push(uuid.clone());
            let tasks = s.tasks.clear_ended().then(|| s.tasks.event());
            (s.turn_id.is_none().then(|| s.turn_id = Some(turn_id.clone())).is_some(), tasks)
        };
        if started {
            (self.sink)(ChatEvent::TurnStarted { turn_id });
        }
        if let Some(e) = tasks {
            (self.sink)(e);
        }
        self.write(&json!({"type": "user", "uuid": uuid, "message": {"role": "user", "content": content}}))
    }

    fn interrupt(&mut self) -> DriverResult<()> {
        self.write(
            &json!({"type": "control_request", "request_id": format!("moonfish-int-{}", self.turn), "request": {"subtype": "interrupt"}}),
        )
    }

    fn answer(&mut self, request_id: &str, decision: Decision) -> DriverResult<()> {
        let (input, suggestions, plan, after_plan) = {
            let mut s = self.state.lock().unwrap();
            let (input, suggestions) = s.pending.remove(request_id).ok_or("that question was already answered")?;
            (input, suggestions, s.plans.remove(request_id), s.after_plan)
        };
        let response = approval_response(input, &suggestions, plan, decision);
        (self.sink)(ChatEvent::ApprovalResolved { request_id: request_id.into(), decision });
        self.write(
            &json!({"type": "control_response", "response": {"subtype": "success", "request_id": request_id, "response": response}}),
        )?;
        if plan && decision != Decision::Deny {
            // Claude stays in plan mode after ExitPlanMode is allowed; put back the earlier mode.
            self.set_permission(after_plan)?;
            (self.sink)(ChatEvent::PermissionChanged { mode: after_plan });
        }
        Ok(())
    }

    fn answer_question(&mut self, request_id: &str, answers: std::collections::BTreeMap<String, Vec<String>>) -> DriverResult<()> {
        let input = self.state.lock().unwrap().questions.remove(request_id).ok_or("that question was already answered")?;
        let response = question_response(&input, &answers);
        (self.sink)(ChatEvent::QuestionResolved { request_id: request_id.into(), answers });
        self.write(&json!({"type": "control_response", "response": {"subtype": "success", "request_id": request_id, "response": response}}))
    }

    fn set_model(&mut self, model: &str) -> DriverResult<()> {
        let m = if model.is_empty() { Value::Null } else { json!(model) };
        self.write(&json!({"type": "control_request", "request_id": format!("moonfish-model-{}", self.turn), "request": {"subtype": "set_model", "model": m}}))
    }

    fn takes_permission(&self, mode: PermissionMode) -> bool {
        mode != PermissionMode::FullAccess || self.full_access
    }

    fn set_permission(&mut self, mode: PermissionMode) -> DriverResult<()> {
        if mode != PermissionMode::Plan {
            self.state.lock().unwrap().after_plan = mode;
        }
        self.write(&json!({"type": "control_request", "request_id": format!("moonfish-mode-{}", self.turn), "request": {"subtype": "set_permission_mode", "mode": permission_mode_flag(mode)}}))
    }

    fn set_fast_mode(&mut self, fast_mode: Option<bool>) -> DriverResult<bool> {
        // 2.1.325 acknowledges this flag and reports the new fast_mode_state at initialize.
        // Earlier builds can take the documented launch setting when the session resumes.
        if !self.state.lock().unwrap().version.as_deref().is_some_and(|v| !crate::probe::older(v, "2.1.325")) {
            return Ok(false);
        }
        self.write(&json!({"type": "control_request", "request_id": format!("moonfish-speed-{}", self.turn),
            "request": {"subtype": "apply_flag_settings", "settings": {"fastMode": fast_mode.unwrap_or(false)}}}))?;
        Ok(true)
    }

    /// Claude Code 2.1.50 and newer switch effort mid-session (`apply_flag_settings`); older
    /// ones take it only as `--effort` when they start.
    fn set_effort(&mut self, effort: &str) -> DriverResult<bool> {
        let live = self.state.lock().unwrap().version.as_deref().is_some_and(|v| !crate::probe::older(v, "2.1.50"));
        if !live {
            return Ok(false);
        }
        let level = if effort.is_empty() { Value::Null } else { json!(effort) };
        self.write(&json!({"type": "control_request", "request_id": format!("moonfish-effort-{}", self.turn), "request": {"subtype": "apply_flag_settings", "settings": {"effortLevel": level}}}))?;
        Ok(true)
    }

    fn resumable(&self) -> bool {
        true
    }

    fn stop_task(&mut self, task_id: &str) -> DriverResult<()> {
        self.write(&json!({"type": "control_request", "request_id": format!("moonfish-stop-{task_id}"), "request": {"subtype": "stop_task", "task_id": task_id}}))
    }

    fn background(&self) -> usize {
        self.state.lock().unwrap().tasks.running()
    }

    fn idle(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.turn_id.is_none() && s.waiting.is_empty() && s.pending.is_empty() && s.questions.is_empty()
    }

    fn stop(&mut self) {
        self.stdin.lock().unwrap().take();
        // Stdin closed: it may exit by itself (saving its session); then it and everything it
        // started (commands, servers) end.
        if let Some(mut c) = self.child.lock().unwrap().take() {
            moonfish_transport::stop_tree(&mut c, std::time::Duration::from_millis(500));
        }
    }
}

impl Drop for ClaudeDriver {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---- Translation -----------------------------------------------------------------------------

struct Translator {
    sink: Sink,
    state: Arc<Mutex<State>>,
    /// To ask Claude for its context breakdown after a turn (none in tests).
    stdin: Option<Arc<Mutex<Option<Stdin>>>>,
    /// Stopped cleanly if an auth error leaves the process unable to read fresh credentials.
    child: Option<Arc<Mutex<Option<Proc>>>>,
    asked: u32,
}

const MAX_OUTPUT: usize = 16 * 1024;

impl Translator {
    fn new(sink: Sink, state: Arc<Mutex<State>>) -> Translator {
        Translator { sink, state, stdin: None, child: None, asked: 0 }
    }

    /// `get_context_usage`: what the window holds, by category (answered as a control_response).
    fn ask_context(&mut self) {
        let Some(stdin) = &self.stdin else { return };
        self.asked += 1;
        let req = json!({"type": "control_request", "request_id": format!("moonfish-ctx-{}", self.asked), "request": {"subtype": "get_context_usage"}});
        if let Some(w) = stdin.lock().unwrap().as_mut() {
            let mut line = serde_json::to_vec(&req).unwrap_or_default();
            line.push(b'\n');
            let _ = w.write_all(&line).and_then(|_| w.flush());
        }
    }

    fn emit(&self, e: ChatEvent) {
        (self.sink)(e)
    }

    fn handle(&mut self, v: &Value) {
        match v["type"].as_str().unwrap_or("") {
            "system" if v["subtype"] == "init" => {
                self.emit(ChatEvent::Started {
                    session_id: v["session_id"].as_str().map(String::from),
                    model: v["model"].as_str().map(String::from),
                    cwd: v["cwd"].as_str().map(String::from),
                });
                self.state.lock().unwrap().model = v["model"].as_str().map(String::from);
            }
            "system" if v["subtype"] == "commands_changed" && v["commands"].is_array() => {
                self.emit(ChatEvent::Commands { commands: parse_commands(v) });
            }
            "system" if v["subtype"].as_str().is_some_and(|s| s.starts_with("task_") || s == "background_tasks_changed") => self.task(v),
            "stream_event" => self.stream_event(&v["event"]),
            "assistant" => self.assistant(v),
            "user" => self.tool_results(v),
            "control_request" => self.control_request(v),
            "control_response"
                if v["response"]["subtype"] == "error"
                    && v["response"]["request_id"].as_str().is_some_and(|id| {
                        ["moonfish-model-", "moonfish-mode-", "moonfish-effort-", "moonfish-speed-", "moonfish-stop-"]
                            .iter()
                            .any(|p| id.starts_with(p))
                    }) =>
            {
                let what = v["response"]["error"].as_str().unwrap_or("Claude Code refused a setting change");
                self.emit(ChatEvent::Notice { level: NoticeLevel::Warn, text: what.to_string() });
            }
            "control_response" if v["response"]["request_id"] == "moonfish-init" => {
                self.state.lock().unwrap().version = v["response"]["response"]["claude_code_version"].as_str().map(String::from);
                let commands = parse_commands(&v["response"]["response"]);
                if !commands.is_empty() {
                    self.emit(ChatEvent::Commands { commands });
                }
            }
            "control_response" if v["response"]["request_id"].as_str().is_some_and(|id| id.starts_with("moonfish-ctx-")) => {
                if let Some(e) = parse_context(&v["response"]["response"]) {
                    self.emit(e);
                }
            }
            "control_response" if v["response"]["request_id"] == "moonfish-models" => {
                let models = parse_models(&v["response"]["response"]);
                if !models.is_empty() {
                    let current = self.state.lock().unwrap().model.clone();
                    self.emit(ChatEvent::Models { models, current });
                }
            }
            "result" => self.result(v),
            "rate_limit_event" => {
                let info = &v["rate_limit_info"];
                if info["status"].as_str().is_some_and(|s| s != "allowed" && s != "allowed_warning") {
                    self.emit(ChatEvent::Notice {
                        level: NoticeLevel::Warn,
                        text: "You've hit a Claude usage limit; requests will fail until it resets.".into(),
                    });
                }
            }
            _ => {}
        }
    }

    fn stream_event(&mut self, e: &Value) {
        let mut s = self.state.lock().unwrap();
        match e["type"].as_str().unwrap_or("") {
            "message_start" => {
                s.message_id = e["message"]["id"].as_str().unwrap_or("").to_string();
                s.blocks.clear();
                // Nobody sent anything, yet Claude answers: a background task ended and it's
                // looking at the result. That's a turn too (it ends with a `result`).
                if s.turn_id.is_none() && s.waiting.is_empty() {
                    s.woke += 1;
                    let turn_id = format!("turn-w{}", s.woke);
                    s.turn_id = Some(turn_id.clone());
                    drop(s);
                    self.emit(ChatEvent::TurnStarted { turn_id });
                }
            }
            "content_block_start" => {
                let kind = match e["content_block"]["type"].as_str() {
                    Some("text") => "text",
                    Some("thinking") => "thinking",
                    _ => "other",
                };
                s.blocks.insert(e["index"].as_u64().unwrap_or(0), kind);
            }
            "content_block_delta" => {
                let index = e["index"].as_u64().unwrap_or(0);
                let item_id = format!("{}:{index}", s.message_id);
                let d = &e["delta"];
                match d["type"].as_str() {
                    Some("text_delta") => {
                        let mid = s.message_id.clone();
                        s.streamed.insert(mid);
                        drop(s);
                        self.emit(ChatEvent::TextDelta { item_id, text: d["text"].as_str().unwrap_or("").into() });
                    }
                    Some("thinking_delta") => {
                        let t = d["thinking"].as_str().unwrap_or("");
                        if !t.is_empty() {
                            drop(s);
                            self.emit(ChatEvent::ReasoningDelta { item_id, text: t.into() });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn assistant(&mut self, v: &Value) {
        let api_error = v["is_api_error_message"].as_bool() == Some(true) && v["error"].is_string();
        if v["error"].as_str() == Some("authentication_failed") {
            self.state.lock().unwrap().auth_failed = true;
        }
        let msg = &v["message"];
        let mid = msg["id"].as_str().unwrap_or("").to_string();
        let streamed = self.state.lock().unwrap().streamed.contains(&mid);
        for (i, block) in msg["content"].as_array().into_iter().flatten().enumerate() {
            match block["type"].as_str() {
                Some("text") if !streamed && !api_error => {
                    self.emit(ChatEvent::Text { item_id: format!("{mid}:t{i}"), text: block["text"].as_str().unwrap_or("").into() });
                }
                Some("thinking") if !streamed => {
                    let t = block["thinking"].as_str().unwrap_or("");
                    if !t.is_empty() {
                        self.emit(ChatEvent::Reasoning { item_id: format!("{mid}:r{i}"), text: t.into() });
                    }
                }
                Some("tool_use") => {
                    let id = block["id"].as_str().unwrap_or("").to_string();
                    let name = block["name"].as_str().unwrap_or("");
                    let input = &block["input"];
                    if name == "TodoWrite" {
                        self.emit(ChatEvent::Plan { items: todos(input) });
                    }
                    self.emit(ChatEvent::ToolStarted { item_id: id, tool: tool_call(name, input) });
                }
                _ => {}
            }
        }
    }

    fn tool_results(&mut self, v: &Value) {
        for e in tool_finished(v, &v["tool_use_result"]) {
            self.emit(e);
        }
    }

    fn control_request(&mut self, v: &Value) {
        let req = &v["request"];
        if req["subtype"] != "can_use_tool" {
            return;
        }
        let request_id = v["request_id"].as_str().unwrap_or("").to_string();
        let name = req["tool_name"].as_str().unwrap_or("");
        let input = req["input"].clone();
        if name == "AskUserQuestion" {
            let questions = input["questions"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(i, q)| Question {
                    id: format!("q{i}"),
                    header: q["header"].as_str().unwrap_or("").into(),
                    question: q["question"].as_str().unwrap_or("").into(),
                    options: q["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|o| QuestionOption {
                            label: o["label"].as_str().unwrap_or("").into(),
                            description: o["description"].as_str().unwrap_or("").into(),
                        })
                        .collect(),
                    multi: q["multiSelect"].as_bool().unwrap_or(false),
                    other: true,
                    secret: false,
                })
                .collect();
            self.state.lock().unwrap().questions.insert(request_id.clone(), input);
            self.emit(ChatEvent::Question { request: QuestionRequest { request_id, questions } });
            return;
        }
        if name == "ExitPlanMode" {
            let plan = input["plan"].as_str().map(String::from);
            {
                let mut st = self.state.lock().unwrap();
                st.pending.insert(request_id.clone(), (input, req["permission_suggestions"].clone()));
                st.plans.insert(request_id.clone());
            }
            self.emit(ChatEvent::Approval {
                request: ApprovalRequest {
                    request_id,
                    item_id: req["tool_use_id"].as_str().map(String::from),
                    kind: ApprovalKind::Plan,
                    title: "Leave plan mode and start on this plan?".into(),
                    detail: plan,
                    diff: None,
                    can_remember: false,
                },
            });
            return;
        }
        let call = tool_call(name, &input);
        let (kind, title, detail, diff) = match call.kind {
            ToolKind::Bash => (ApprovalKind::Command, "Run a command".to_string(), input["command"].as_str().map(String::from), None),
            ToolKind::Edit | ToolKind::Write => (
                ApprovalKind::Edit,
                format!("{} {}", if call.kind == ToolKind::Write { "Write" } else { "Edit" }, call.title),
                None,
                proposed_diff(name, &input),
            ),
            _ => {
                let label = mcp_title(name).unwrap_or_else(|| name.to_string());
                let summary =
                    crate::acp::input_summary(&input).or_else(|| Some(call.title.clone()).filter(|t| !t.is_empty() && *t != label));
                (ApprovalKind::Other, format!("Use {label}"), summary, None)
            }
        };
        let detail = detail.or_else(|| req["decision_reason"].as_str().map(String::from));
        let suggestions = req["permission_suggestions"].clone();
        let can_remember = suggestions.as_array().is_some_and(|a| !a.is_empty());
        self.state.lock().unwrap().pending.insert(request_id.clone(), (input, suggestions));
        self.emit(ChatEvent::Approval {
            request: ApprovalRequest {
                request_id,
                item_id: req["tool_use_id"].as_str().map(String::from),
                kind,
                title,
                detail,
                diff,
                can_remember,
            },
        });
    }

    /// `task_started` / `task_progress` / `task_updated` / `task_notification`, and
    /// `background_tasks_changed` (the running background tasks, whole).
    fn task(&mut self, v: &Value) {
        let id = v["task_id"].as_str().unwrap_or("").to_string();
        let mut ended = None;
        let mut sub_events: Vec<ChatEvent> = Vec::new();
        let event = {
            let mut s = self.state.lock().unwrap();
            let changed = match v["subtype"].as_str().unwrap_or("") {
                "task_started" if !id.is_empty() => {
                    let kind = task_kind(v["task_type"].as_str().unwrap_or(""));
                    let bg = v["is_backgrounded"] == true || s.turn_id.is_none();
                    let mut t = if bg {
                        BackgroundTask::running(&id, kind, task_title(v))
                    } else {
                        BackgroundTask::in_turn(&id, kind, task_title(v))
                    };
                    t.item_id = v["tool_use_id"].as_str().map(String::from);
                    t.stoppable = true;
                    if kind == TaskKind::Agent {
                        if let Some(text) = v["prompt"]
                            .as_str()
                            .or_else(|| v["description"].as_str())
                            .map(str::trim)
                            .filter(|p| !p.is_empty())
                        {
                            sub_events.push(ChatEvent::UserMessage {
                                id: format!("{id}:init"),
                                text: text.to_string(),
                                images: vec![],
                                files: vec![],
                            });
                        }
                        sub_events.push(ChatEvent::TurnStarted { turn_id: format!("sub-{id}") });
                    }
                    if bg || kind == TaskKind::Agent {
                        s.tasks.add(t)
                    } else {
                        s.inside.insert(id.clone(), t);
                        false
                    }
                }
                "task_progress" => {
                    let existing = s.tasks.get(&id).or_else(|| s.inside.get(&id));
                    let (current_title, is_agent) = existing.map(|t| (t.title.clone(), t.kind == TaskKind::Agent)).unwrap_or_default();
                    let detail = task_progress_detail(v, &current_title);
                    if let (Some(t), Some(d)) = (s.inside.get_mut(&id), &detail) {
                        t.detail = Some(d.clone());
                    }
                    if is_agent {
                        if let Some(d) = &detail {
                            sub_events.push(ChatEvent::Notice { level: NoticeLevel::Info, text: d.clone() });
                        }
                    }
                    let title = if is_agent { None } else { v["description"].as_str() };
                    s.tasks.progress(&id, title, detail.as_deref())
                }
                "task_updated" => {
                    let patch = &v["patch"];
                    let status = patch["status"].as_str().and_then(task_status);
                    match status {
                        Some(st) if st != TaskStatus::Running => {
                            s.inside.remove(&id);
                            if s.tasks.get(&id).is_some_and(|t| t.kind == TaskKind::Agent) {
                                sub_events.push(ChatEvent::TurnFinished {
                                    turn_id: format!("sub-{id}"),
                                    usage: None,
                                    cost_usd: None,
                                    duration_ms: None,
                                    interrupted: st == TaskStatus::Stopped,
                                });
                            }
                            s.tasks.finish(&id, st, patch["error"].as_str().map(String::from)).is_some()
                        }
                        _ => {
                            let mut changed = false;
                            if patch["is_backgrounded"] == true {
                                changed |= s.tasks.promote_to_background(&id);
                            }
                            changed |= s.tasks.progress(&id, patch["description"].as_str(), None);
                            changed
                        }
                    }
                }
                "task_notification" => {
                    s.inside.remove(&id);
                    let st = v["status"].as_str().and_then(task_status).unwrap_or(TaskStatus::Completed);
                    let summary = v["summary"].as_str().filter(|d| !d.is_empty()).map(String::from);
                    if s.tasks.get(&id).is_some_and(|t| t.kind == TaskKind::Agent) {
                        if let Some(text) = &summary {
                            sub_events.push(ChatEvent::Text { item_id: format!("{id}:summary"), text: text.clone() });
                        }
                        sub_events.push(ChatEvent::TurnFinished {
                            turn_id: format!("sub-{id}"),
                            usage: None,
                            cost_usd: None,
                            duration_ms: None,
                            interrupted: st == TaskStatus::Stopped,
                        });
                    }
                    // `task_updated` may have ended it already; the summary is still news.
                    let shown = s.tasks.finish(&id, st, summary.clone()).is_some() || s.tasks.set_detail(&id, summary.clone());
                    if let Some(t) = s.tasks.get(&id).filter(|t| t.background) {
                        ended = Some(summary.unwrap_or_else(|| format!("Background task ended: {}", t.title)));
                    }
                    shown
                }
                "background_tasks_changed" => {
                    // Running background tasks we never saw start (a resumed session's), or
                    // in-turn ones that moved to the background.
                    let mut changed = false;
                    for t in v["tasks"].as_array().into_iter().flatten() {
                        let Some(tid) = t["task_id"].as_str() else { continue };
                        if s.tasks.get(tid).is_some() {
                            changed |= s.tasks.promote_to_background(tid);
                        } else if !s.inside.contains_key(tid) {
                            let mut task = BackgroundTask::running(tid, task_kind(t["task_type"].as_str().unwrap_or("")), task_title(t));
                            task.stoppable = true;
                            changed |= s.tasks.add(task);
                        }
                    }
                    changed
                }
                _ => false,
            };
            changed.then(|| s.tasks.event())
        };
        for ev in sub_events {
            self.emit(ChatEvent::SubagentEvent { task_id: id.clone(), event: Box::new(ev) });
        }
        if let Some(e) = event {
            self.emit(e);
        }
        if let Some(text) = ended {
            self.emit(ChatEvent::Notice { level: NoticeLevel::Info, text });
        }
    }

    fn result(&mut self, v: &Value) {
        let (turn_id, cost, auth_failed) = {
            let mut s = self.state.lock().unwrap();
            let total = v["total_cost_usd"].as_f64().unwrap_or(s.cost_so_far);
            let cost = (total - s.cost_so_far).max(0.0);
            s.cost_so_far = total;
            s.pending.clear();
            s.questions.clear();
            let auth_failed = std::mem::take(&mut s.auth_failed);
            (s.turn_id.take(), cost, auth_failed)
        };
        let u = &v["usage"];
        let usage = Usage {
            input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
            cache_read_tokens: u["cache_read_input_tokens"].as_u64().unwrap_or(0),
            cache_write_tokens: u["cache_creation_input_tokens"].as_u64().unwrap_or(0),
        };
        let mut auth_error = false;
        if v["is_error"].as_bool() == Some(true) {
            let msg = v["result"]
                .as_str()
                .filter(|r| !r.is_empty())
                .map(String::from)
                .unwrap_or_else(|| format!("Claude Code stopped: {}", v["subtype"].as_str().unwrap_or("error")));
            auth_error = auth_failed || is_auth_error(&msg);
            if v["subtype"] != "error_during_execution" {
                self.emit(ChatEvent::Error { message: msg });
            }
        }
        let interrupted =
            v["subtype"] == "error_during_execution" || v["terminal_reason"].as_str().is_some_and(|r| r.contains("interrupt"));
        // Tasks the turn started and didn't wait for are background work now (unless the turn was
        // stopped: then in-turn ones were cut off with it).
        let outlived = {
            let mut s = self.state.lock().unwrap();
            let left: Vec<BackgroundTask> = s.inside.drain().map(|(_, t)| t).collect();
            let mut changed = false;
            if interrupted {
                changed |= s.tasks.stop_in_turn();
            } else {
                for mut t in left {
                    t.background = true;
                    changed |= s.tasks.add(t);
                }
                changed |= s.tasks.promote_running_agents();
            }
            changed.then(|| s.tasks.event())
        };
        if let Some(e) = outlived {
            self.emit(e);
        }
        self.emit(ChatEvent::TurnFinished {
            turn_id: turn_id.unwrap_or_else(|| "turn".into()),
            usage: Some(usage),
            cost_usd: Some(cost),
            duration_ms: v["duration_ms"].as_u64(),
            interrupted,
        });
        if auth_error {
            // Claude caches failed credential reads in-process; stop it so the next message
            // starts a fresh process that reads newly saved credentials.
            {
                let mut s = self.state.lock().unwrap();
                s.alive = false;
                s.waiting.clear();
            }
            if let Some(stdin) = &self.stdin {
                stdin.lock().unwrap().take();
            }
            if let Some(child) = &self.child {
                if let Some(mut c) = child.lock().unwrap().take() {
                    moonfish_transport::stop_tree(&mut c, std::time::Duration::from_millis(500));
                }
            }
            return;
        }
        // Context fill: the last request's prompt size against the model's window.
        if let Some(last) = u["iterations"].as_array().and_then(|a| a.last()) {
            let used = ["input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"]
                .iter()
                .map(|k| last[*k].as_u64().unwrap_or(0))
                .sum::<u64>();
            let window = v["modelUsage"].as_object().and_then(|m| m.values().find_map(|x| x["contextWindow"].as_u64())).unwrap_or(200_000);
            if used > 0 {
                self.emit(ChatEvent::Context { used_tokens: used, window_tokens: window, parts: vec![] });
            }
        }
        // And what that context is made of (Claude Code's /context).
        self.ask_context();
        // A message sent during that turn and not folded into it runs now, as the next turn.
        let next = {
            let mut s = self.state.lock().unwrap();
            settle(&mut s.waiting, v, interrupted);
            if !s.waiting.is_empty() {
                s.seq += 1;
                let id = format!("turn-q{}", s.seq);
                s.turn_id = Some(id.clone());
                Some(id)
            } else {
                None
            }
        };
        if let Some(turn_id) = next {
            self.emit(ChatEvent::TurnStarted { turn_id });
        }
    }
}

/// Whether a Claude Code error reports missing or invalid authentication.
pub fn is_auth_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["not logged in", "please run /login", "authentication_failed", "authentication required"].iter().any(|p| lower.contains(p))
}

/// Claude Code's `task_type`.
fn task_kind(t: &str) -> TaskKind {
    match t {
        "local_bash" => TaskKind::Command,
        t if t.starts_with("monitor") => TaskKind::Watch,
        t if t.contains("agent") || t.contains("teammate") => TaskKind::Agent,
        _ => TaskKind::Other,
    }
}

fn task_title(v: &Value) -> String {
    ["description", "prompt", "subagent_type", "task_type"]
        .iter()
        .find_map(|k| v[*k].as_str().filter(|s| !s.trim().is_empty()))
        .map(|s| s.lines().next().unwrap_or(s).to_string())
        .unwrap_or_else(|| "Background task".into())
}

fn task_progress_detail(v: &Value, current_title: &str) -> Option<String> {
    if let Some(summary) = v["summary"].as_str().map(str::trim).filter(|d| !d.is_empty()) {
        return Some(summary.to_string());
    }
    let tool = v["last_tool_name"].as_str().map(str::trim).filter(|n| !n.is_empty());
    let desc = v["description"].as_str().map(str::trim).filter(|d| !d.is_empty() && *d != current_title);
    match (tool, desc) {
        (Some(n), Some(d)) => Some(format!("Using {n} · {d}")),
        (Some(n), None) => Some(format!("Using {n}")),
        (None, Some(d)) => Some(d.to_string()),
        (None, None) => None,
    }
}

fn task_status(s: &str) -> Option<TaskStatus> {
    match s {
        "running" | "pending" => Some(TaskStatus::Running),
        "completed" => Some(TaskStatus::Completed),
        "failed" => Some(TaskStatus::Failed),
        "killed" | "stopped" | "cancelled" => Some(TaskStatus::Stopped),
        _ => None,
    }
}

/// The `uuid` Claude Code tags a message with (and lists in the `result` that answers it): the
/// prompt's own id when it is one, else a fresh one.
fn message_uuid(id: &str) -> String {
    let is_uuid =
        id.len() == 36 && id.char_indices().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_hexdigit() });
    if is_uuid {
        return id.to_ascii_lowercase();
    }
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for half in bytes.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write(id.as_bytes());
        half.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// Drops the messages a `result` answered from `waiting`. It names them in `user_message_uuids`,
/// an interrupted one too: a message still queued behind a stopped turn isn't cancelled, it runs
/// next (the interrupt's reply lists it as `still_queued`), so it keeps waiting. A CLI that
/// doesn't name them answered the oldest, and its interrupt drops the rest.
fn settle(waiting: &mut Vec<String>, result: &Value, interrupted: bool) {
    if let Some(done) = result["user_message_uuids"].as_array() {
        waiting.retain(|u| !done.iter().any(|d| d.as_str() == Some(u)));
    } else if interrupted {
        waiting.clear();
    } else if !waiting.is_empty() {
        waiting.remove(0);
    }
}

/// `list_models`' answer: `{models: [{value, displayName, description, disabled, supportsEffort,
/// supportedEffortLevels}]}`.
pub fn parse_models(r: &Value) -> Vec<ModelOption> {
    claude_models(r, false)
}

fn fast_available(init: &Value) -> bool {
    matches!(init["fast_mode_state"].as_str(), Some("off" | "on" | "cooldown")) && init["fast_mode_disabled_reason"].is_null()
}

fn claude_models(r: &Value, fast_available: bool) -> Vec<ModelOption> {
    r["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| !m["disabled"].as_bool().unwrap_or(false))
        .filter_map(|m| {
            let id = m["value"].as_str()?.to_string();
            let efforts = if m["supportsEffort"].as_bool() == Some(true) { strings(&m["supportedEffortLevels"]) } else { vec![] };
            Some(ModelOption {
                name: m["displayName"].as_str().unwrap_or(&id).to_string(),
                description: m["description"].as_str().unwrap_or("").to_string(),
                id,
                efforts,
                default_effort: None,
                fast_mode: (fast_available && m["supportsFastMode"] == true).then_some(false),
            })
        })
        .collect()
}

/// `get_context_usage`'s answer: `{categories: [{name, tokens, kind}], totalTokens, maxTokens}`.
/// Empty categories are left out; the rest go largest first, with free space last.
pub fn parse_context(r: &Value) -> Option<ChatEvent> {
    let used = r["totalTokens"].as_u64()?;
    let window = r["maxTokens"].as_u64().filter(|w| *w > 0)?;
    let mut parts: Vec<ContextPart> = r["categories"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let tokens = c["tokens"].as_u64().filter(|t| *t > 0)?;
            Some(ContextPart { name: c["name"].as_str()?.to_string(), tokens, kind: c["kind"].as_str().unwrap_or("used").to_string() })
        })
        .collect();
    parts.sort_by_key(|p| (p.kind == "free", std::cmp::Reverse(p.tokens)));
    Some(ChatEvent::Context { used_tokens: used, window_tokens: window, parts })
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect()
}

/// An effort level safe to pass as a flag.
pub fn valid_effort(e: &str) -> bool {
    !e.is_empty() && e.len() <= 16 && e.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The `can_use_tool` answer. "Always" keeps Claude's suggested rules for this session only (they
/// would otherwise land in the project's or the user's settings for good); a plan is approved or
/// sent back for more planning.
pub fn approval_response(input: Value, suggestions: &Value, plan: bool, decision: Decision) -> Value {
    match decision {
        Decision::Deny if plan => {
            json!({"behavior": "deny", "message": "The user asked you to keep planning. Revise the plan and call ExitPlanMode again."})
        }
        Decision::Deny => json!({"behavior": "deny", "message": "The user declined this. Don't try it again; ask what they want instead."}),
        Decision::AllowAlways if !plan && suggestions.as_array().is_some_and(|a| !a.is_empty()) => {
            let session: Vec<Value> = suggestions
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| {
                    let mut s = s.clone();
                    s["destination"] = json!("session");
                    s
                })
                .collect();
            json!({"behavior": "allow", "updatedInput": input, "updatedPermissions": session})
        }
        _ => json!({"behavior": "allow", "updatedInput": input}),
    }
}

/// Commands that only work in Claude Code's terminal UI (its `terminal_slash_commands`, and a
/// few more that act on that UI).
const TERMINAL_ONLY: &[&str] = &["doctor", "color", "focus", "reload-plugins", "heapdump"];

/// `initialize`'s answer: `{commands: [{name, description, argumentHint, aliases, builtin}]}`.
/// Internal (`__`), removed and renamed commands are left out, and so are terminal-only ones.
pub fn parse_commands(r: &Value) -> Vec<SlashCommand> {
    let mut out = Vec::new();
    for c in r["commands"].as_array().into_iter().flatten() {
        let Some(name) = c["name"].as_str().map(|n| n.trim_start_matches('/')) else { continue };
        let description = c["description"].as_str().unwrap_or("");
        let gone = description.starts_with("(removed)") || description.starts_with("Renamed to");
        if name.is_empty() || name.starts_with("__") || gone || TERMINAL_ONLY.contains(&name) {
            continue;
        }
        let hint = c["argumentHint"].as_str().unwrap_or("").to_string();
        out.push(SlashCommand { name: name.into(), description: description.into(), hint: hint.clone(), skill: false });
        // `/review` is `/code-review`'s alias: offered under both names.
        for alias in c["aliases"].as_array().into_iter().flatten().filter_map(|a| a.as_str()) {
            out.push(SlashCommand { name: alias.into(), description: format!("Same as /{name}"), hint: hint.clone(), skill: false });
        }
    }
    out
}

/// Claude Code's models and slash commands, from a short-lived `claude -p` (for a chat that
/// hasn't started).
pub fn catalog(
    binary: &str,
    location: &moonfish_core::settings::Location,
    cwd: &str,
    env: &[moonfish_core::settings::EnvVar],
) -> DriverResult<(Vec<ModelOption>, Vec<SlashCommand>)> {
    let program = if binary.trim().is_empty() { AgentKind::Claude.default_binary().to_string() } else { binary.to_string() };
    // Only the catalog process opts in. It never receives a user turn. Without the flag
    // Claude reports sdk_opt_in_required before checking account and organization access.
    let args = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--no-session-persistence",
        "--settings",
        r#"{"fastMode":true}"#,
    ]
    .map(String::from)
    .to_vec();
    let mut child = moonfish_transport::spawn(&Spec { location, cwd, program: &program, args, env })?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut commands = Vec::new();
        let mut available = false;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v["type"] != "control_response" {
                    continue;
                }
                let r = &v["response"]["response"];
                if v["response"]["request_id"] == "i" {
                    commands = parse_commands(r);
                    available = fast_available(r);
                } else if v["response"]["request_id"] == "m" {
                    let _ = tx.send((claude_models(r, available), commands));
                    return;
                }
            }
        }
    });
    for req in [json!({"subtype": "initialize", "hooks": null}), json!({"subtype": "list_models"})] {
        let id = if req["subtype"] == "list_models" { "m" } else { "i" };
        writeln!(stdin, "{}", json!({"type": "control_request", "request_id": id, "request": req})).map_err(|e| e.to_string())?;
    }
    let got = rx.recv_timeout(std::time::Duration::from_secs(30)).map_err(|_| "Claude Code didn't list its models".to_string());
    let _ = child.kill();
    let _ = child.wait();
    got
}

/// AskUserQuestion's answer, as Claude Code takes it: the input plus `answers` keyed by each
/// question's text (one pick a string, several a list). No answers at all declines.
pub fn question_response(input: &Value, answers: &std::collections::BTreeMap<String, Vec<String>>) -> Value {
    let mut by_text = serde_json::Map::new();
    for (i, q) in input["questions"].as_array().into_iter().flatten().enumerate() {
        let key = q["question"].as_str().or(q["header"].as_str()).unwrap_or("").to_string();
        if let Some(a) = answers.get(&format!("q{i}")).filter(|a| !a.is_empty()) {
            let multi = q["multiSelect"].as_bool().unwrap_or(false);
            by_text.insert(key, if multi { json!(a) } else { json!(a[0]) });
        }
    }
    if by_text.is_empty() {
        return json!({"behavior": "deny", "message": "The user dismissed these questions without answering. End your turn and wait for them."});
    }
    let mut updated = input.clone();
    updated["answers"] = Value::Object(by_text);
    json!({"behavior": "allow", "updatedInput": updated})
}

fn mcp_title(name: &str) -> Option<String> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    (!server.is_empty() && !tool.is_empty()).then(|| format!("{server} · {tool}"))
}

/// Claude's tool names to Moonfish's kinds, with a one-line title.
pub fn tool_call(name: &str, input: &Value) -> ToolCall {
    let s = |k: &str| input[k].as_str().unwrap_or("").to_string();
    let path = ["file_path", "notebook_path", "path"].iter().map(|k| s(k)).find(|p| !p.is_empty());
    let (kind, title) = match name {
        "Read" => (ToolKind::Read, s("file_path")),
        "Edit" | "MultiEdit" | "NotebookEdit" => (ToolKind::Edit, path.clone().unwrap_or_default()),
        "Write" => (ToolKind::Write, s("file_path")),
        "Bash" | "BashOutput" | "PowerShell" => (ToolKind::Bash, s("command")),
        "Grep" => (ToolKind::Search, s("pattern")),
        "Glob" => (ToolKind::Search, s("pattern")),
        "WebFetch" => (ToolKind::Web, s("url")),
        "WebSearch" => (ToolKind::Web, s("query")),
        "Task" | "Agent" => (ToolKind::Task, s("description")),
        "TodoWrite" => (ToolKind::Other, "Update plan".into()),
        "Skill" => {
            let skill = s("skill");
            let args = s("args");
            let title = match (skill.trim(), args.trim()) {
                ("", _) => "Skill".to_string(),
                (sk, "") => sk.to_string(),
                (sk, a) => format!("{sk} {a}"),
            };
            (ToolKind::Other, title)
        }
        _ => (ToolKind::Other, mcp_title(name).unwrap_or_else(|| name.to_string())),
    };
    ToolCall { kind, name: name.into(), title: if title.is_empty() { name.into() } else { title }, input: input.clone(), path }
}

/// A user message's tool results as `ToolFinished` events; `result` is what Claude Code adds
/// about the tool's effect (an edit's patch).
fn tool_finished(v: &Value, result: &Value) -> Vec<ChatEvent> {
    let Some(content) = v["message"]["content"].as_array() else { return vec![] };
    content
        .iter()
        .filter(|b| b["type"] == "tool_result")
        .map(|block| {
            let id = block["tool_use_id"].as_str().unwrap_or("").to_string();
            let ok = !block["is_error"].as_bool().unwrap_or(false);
            let mut output = result_text(&block["content"]);
            if output.len() > MAX_OUTPUT {
                let mut cut = MAX_OUTPUT;
                while !output.is_char_boundary(cut) {
                    cut -= 1;
                }
                output.truncate(cut);
                output.push_str("\n…");
            }
            let diff = if ok { patch_of(result) } else { None };
            ChatEvent::ToolFinished { item_id: id, ok, output, diff }
        })
        .collect()
}

fn todos(input: &Value) -> Vec<PlanItem> {
    input["todos"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| PlanItem {
            text: t["content"].as_str().unwrap_or("").into(),
            status: match t["status"].as_str() {
                Some("completed") => PlanStatus::Completed,
                Some("in_progress") => PlanStatus::InProgress,
                _ => PlanStatus::Pending,
            },
        })
        .collect()
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// The diff Claude reports after an edit (`tool_use_result.structuredPatch`).
fn patch_of(r: &Value) -> Option<FileDiff> {
    let path = r["filePath"].as_str()?;
    let hunks = r["structuredPatch"].as_array().filter(|h| !h.is_empty());
    match hunks {
        Some(hunks) => {
            let mut patch = String::new();
            for h in hunks {
                patch.push_str(&format!(
                    "@@ -{},{} +{},{} @@\n",
                    h["oldStart"].as_u64().unwrap_or(0),
                    h["oldLines"].as_u64().unwrap_or(0),
                    h["newStart"].as_u64().unwrap_or(0),
                    h["newLines"].as_u64().unwrap_or(0)
                ));
                for l in h["lines"].as_array().into_iter().flatten().filter_map(|l| l.as_str()) {
                    patch.push_str(l);
                    patch.push('\n');
                }
            }
            Some(FileDiff::from_patch(path, patch))
        }
        // A new file: everything is added.
        None if r["type"] == "create" => Some(FileDiff::replace(path, "", r["content"].as_str().unwrap_or(""))),
        None => None,
    }
}

/// What an edit would change, for the approval card.
fn proposed_diff(name: &str, input: &Value) -> Option<FileDiff> {
    let path = input["file_path"].as_str()?;
    match name {
        "Edit" => Some(FileDiff::replace(path, input["old_string"].as_str()?, input["new_string"].as_str()?)),
        "MultiEdit" => {
            let mut patch = String::new();
            for e in input["edits"].as_array()? {
                patch.push_str(&FileDiff::replace(path, e["old_string"].as_str()?, e["new_string"].as_str()?).patch);
            }
            Some(FileDiff::from_patch(path, patch))
        }
        "Write" => Some(FileDiff::replace(path, "", input["content"].as_str()?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_with_state(lines: &[Value]) -> (Vec<ChatEvent>, Arc<Mutex<State>>) {
        let out = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let sink: Sink = Arc::new(move |e| o.lock().unwrap().push(e));
        let state = Arc::new(Mutex::new(State { alive: true, turn_id: Some("turn-1".into()), ..Default::default() }));
        let mut tr = Translator::new(sink, state.clone());
        for l in lines {
            tr.handle(l);
        }
        let v = out.lock().unwrap().clone();
        (v, state)
    }

    fn run(lines: &[Value]) -> Vec<ChatEvent> {
        run_with_state(lines).0
    }

    #[test]
    fn refused_setting_changes_are_said() {
        let err = |id: &str| json!({"type": "control_response", "response": {"subtype": "error", "request_id": id, "error": "Cannot set permission mode to bypassPermissions"}});
        let out = run(&[err("moonfish-mode-3"), err("moonfish-ctx-3")]);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(matches!(&out[0], ChatEvent::Notice { text, .. } if text.contains("bypassPermissions")));
    }

    #[test]
    fn reads_the_context_breakdown() {
        let r = json!({"totalTokens": 45061, "maxTokens": 1000000, "categories": [
            {"name": "System prompt", "tokens": 3501, "kind": "used"},
            {"name": "Free space", "tokens": 921939, "kind": "free"},
            {"name": "Deferred tools", "tokens": 176195, "kind": "deferred"},
            {"name": "Messages", "tokens": 12359, "kind": "used"},
            {"name": "Memory files", "tokens": 0, "kind": "used"}
        ]});
        let Some(ChatEvent::Context { used_tokens, window_tokens, parts }) = parse_context(&r) else { panic!() };
        assert_eq!((used_tokens, window_tokens), (45061, 1_000_000));
        assert_eq!(
            parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["Deferred tools", "Messages", "System prompt", "Free space"]
        );
        assert!(parse_context(&json!({"categories": []})).is_none());
        let ev =
            run(&[json!({"type": "control_response", "response": {"subtype": "success", "request_id": "moonfish-ctx-1", "response": r}})]);
        assert!(matches!(&ev[..], [ChatEvent::Context { parts, .. }] if parts.len() == 4));
    }

    #[test]
    fn fast_mode_needs_both_session_and_model_support() {
        let list = json!({"models":[{"value":"eligible","supportsFastMode":true},{"value":"standard"},{"value":"disabled","supportsFastMode":true,"disabled":true}]});
        for (init, eligible) in [
            (json!({"fast_mode_state":"on"}), Some(false)),
            (json!({"fast_mode_state":"off","fast_mode_disabled_reason":"extra_usage_disabled"}), None),
            (json!({"fast_mode_state":"off","fast_mode_disabled_reason":"sdk_opt_in_required"}), None),
            (json!({}), None),
        ] {
            let models = claude_models(&list, fast_available(&init));
            assert_eq!(models.iter().map(|m| m.fast_mode).collect::<Vec<_>>(), [eligible, None]);
        }
    }

    #[test]
    fn reads_effort_levels() {
        let m = parse_models(&json!({"models": [
            {"value": "opus", "displayName": "Opus 5.5", "supportsEffort": true, "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"]},
            {"value": "claude-haiku-4-5", "displayName": "Haiku 4.5", "supportsThinkingOff": true}
        ]}));
        assert_eq!(m[0].efforts, ["low", "medium", "high", "xhigh", "max"]);
        assert!(m[1].efforts.is_empty());
        assert!(valid_effort("xhigh") && !valid_effort("hi gh") && !valid_effort(""));
    }

    #[test]
    fn always_allow_stays_in_this_session() {
        let input = json!({"command": "ls"});
        let sugg = json!([{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "ls"}], "behavior": "allow", "destination": "localSettings"}]);
        let r = approval_response(input.clone(), &sugg, false, Decision::AllowAlways);
        assert_eq!(r["updatedPermissions"][0]["destination"], "session");
        assert_eq!(r["updatedPermissions"][0]["rules"][0]["ruleContent"], "ls");
        assert!(approval_response(input.clone(), &sugg, false, Decision::Allow).get("updatedPermissions").is_none());
        // A plan never saves rules; turning it down asks for more planning.
        assert!(approval_response(input.clone(), &sugg, true, Decision::AllowAlways).get("updatedPermissions").is_none());
        assert!(approval_response(input, &sugg, true, Decision::Deny)["message"].as_str().unwrap().contains("keep planning"));
    }

    #[test]
    fn reads_commands_from_initialize() {
        let r = json!({"commands": [
            {"name": "compact", "description": "Free up context", "argumentHint": "<instructions>", "builtin": true},
            {"name": "color", "description": "Set the prompt bar color", "argumentHint": "", "builtin": true},
            {"name": "__remote-workflow", "description": "internal", "argumentHint": ""},
            {"name": "agents", "description": "(removed) Ask Claude", "argumentHint": ""},
            {"name": "deploy", "description": "Ship it", "argumentHint": ""},
            {"name": "code-review", "description": "Review the diff", "argumentHint": "[pr]", "aliases": ["review"]}
        ]});
        let c = parse_commands(&r);
        assert_eq!(c.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["compact", "deploy", "code-review", "review"]);
        assert_eq!(c[3].hint, "[pr]");
        assert_eq!(c[0].hint, "<instructions>");
        let ev =
            run(&[json!({"type": "control_response", "response": {"subtype": "success", "request_id": "moonfish-init", "response": r}})]);
        assert!(matches!(&ev[..], [ChatEvent::Commands { commands }] if commands.len() == 4));
    }

    #[test]
    #[ignore]
    fn real_claude_background() {
        use std::time::{Duration, Instant};
        let dir = std::env::var("MOONFISH_TEST_DIR").expect("MOONFISH_TEST_DIR");
        let events = Arc::new(Mutex::new(Vec::<ChatEvent>::new()));
        let e = events.clone();
        let sink: Sink = Arc::new(move |ev| e.lock().unwrap().push(ev));
        let opts = StartOptions {
            kind: AgentKind::Claude,
            cwd: dir,
            location: moonfish_core::settings::Location::Local,
            binary: String::new(),
            model: Some("haiku".into()),
            effort: None,
            fast_mode: None,
            permission: PermissionMode::FullAccess,
            resume: None,
            env: vec![],
        };
        let mut d = ClaudeDriver::start(opts, sink).expect("start claude");
        let text = "Use the Bash tool with run_in_background=true to run: sleep 120. Then reply OK and end your turn. Don't check on it.";
        d.send(Prompt { id: "p1".into(), text: text.into(), images: vec![], files: vec![] }).unwrap();
        let wait = |f: &dyn Fn(&[ChatEvent]) -> bool| {
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(90) {
                if f(&events.lock().unwrap()) {
                    return true;
                }
                thread::sleep(Duration::from_millis(250));
            }
            false
        };
        assert!(wait(&|ev| ev.iter().any(|e| matches!(e, ChatEvent::TurnFinished { .. }))), "the turn ends");
        let last_tasks =
            |ev: &[ChatEvent]| ev.iter().rev().find_map(|e| if let ChatEvent::Tasks { tasks } = e { Some(tasks.clone()) } else { None });
        let tasks = last_tasks(&events.lock().unwrap()).expect("a background task");
        eprintln!("{}", serde_json::to_string(&tasks).unwrap());
        assert_eq!((tasks[0].kind, tasks[0].status), (TaskKind::Command, TaskStatus::Running));
        assert_eq!(d.background(), 1);
        d.stop_task(&tasks[0].id).unwrap();
        assert!(wait(&|ev| last_tasks(ev).is_some_and(|t| t[0].status == TaskStatus::Stopped)), "stopping it ends it");
        eprintln!("{}", serde_json::to_string(&last_tasks(&events.lock().unwrap())).unwrap());
        assert_eq!(d.background(), 0);
        d.stop();
    }

    #[test]
    fn tracks_background_tasks() {
        // As Claude Code 2.1.319 says it: a `Bash` with `run_in_background`, an in-turn
        // non-agent command (stays hidden), an in-turn subagent (shows live with
        // `background: false`), the turn ends, the command ends, then Claude wakes up.
        let ev = run(&[
            json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b1","task_type":"local_bash","description":"Sleep 8 seconds"}]}),
            json!({"type":"system","subtype":"task_started","task_id":"b1","tool_use_id":"tu1","description":"Sleep 8 seconds","is_backgrounded":true,"task_type":"local_bash"}),
            json!({
                "type": "system", "subtype": "task_started", "task_id": "c1", "tool_use_id": "tu0",
                "description": "cargo check", "is_backgrounded": false, "task_type": "local_bash"
            }),
            json!({"type":"system","subtype":"task_notification","task_id":"c1","status":"completed","summary":"ok"}),
            json!({"type":"system","subtype":"task_started","task_id":"a1","tool_use_id":"tu2","description":"Explore the repo","is_backgrounded":false,"task_type":"local_agent"}),
            json!({"type":"system","subtype":"task_progress","task_id":"a1","description":"Explore the repo","last_tool_name":"Grep","usage":{}}),
            json!({"type":"system","subtype":"task_notification","task_id":"a1","status":"completed","summary":"done"}),
            json!({"type":"result","subtype":"success","total_cost_usd":0.0,"usage":{}}),
            json!({"type":"system","subtype":"task_updated","task_id":"b1","patch":{"status":"completed","end_time":1}}),
            json!({"type":"system","subtype":"task_notification","task_id":"b1","status":"completed","summary":"Background command \"Sleep 8 seconds\" completed (exit code 0)"}),
            json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}),
            json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"m9"}}}),
            json!({"type":"result","subtype":"success","total_cost_usd":0.0,"usage":{}}),
        ]);
        let lists: Vec<&Vec<BackgroundTask>> =
            ev.iter().filter_map(|e| if let ChatEvent::Tasks { tasks } = e { Some(tasks) } else { None }).collect();
        // Normal in-turn bash commands never show; the background command and subagent do.
        assert!(lists.iter().all(|l| l.iter().all(|t| t.id != "c1")));
        assert_eq!((lists[0][0].id.as_str(), lists[0][0].status, lists[0][0].background), ("b1", TaskStatus::Running, true));
        assert_eq!((lists[1][0].kind, lists[1][0].item_id.as_deref(), lists[1][0].stoppable), (TaskKind::Command, Some("tu1"), true));
        // The in-turn subagent streams live with `background: false` and does not emit a Notice.
        let a1_start = lists[2].iter().find(|t| t.id == "a1").unwrap();
        assert_eq!(
            (a1_start.kind, a1_start.status, a1_start.background, a1_start.item_id.as_deref()),
            (TaskKind::Agent, TaskStatus::Running, false, Some("tu2"))
        );
        let a1_prog = lists[3].iter().find(|t| t.id == "a1").unwrap();
        assert_eq!(a1_prog.detail.as_deref(), Some("Using Grep"));
        let last = lists.last().unwrap();
        let b1 = last.iter().find(|t| t.id == "b1").unwrap();
        assert_eq!(b1.status, TaskStatus::Completed);
        assert!(b1.detail.as_deref().unwrap().contains("exit code 0"));
        let a1 = last.iter().find(|t| t.id == "a1").unwrap();
        assert_eq!((a1.status, a1.detail.as_deref(), a1.background), (TaskStatus::Completed, Some("done"), false));
        let notices: Vec<&str> =
            ev.iter().filter_map(|e| if let ChatEvent::Notice { text, .. } = e { Some(text.as_str()) } else { None }).collect();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("exit code 0"));
        // Claude's own turn after it is a turn.
        assert!(ev.iter().any(|e| matches!(e, ChatEvent::TurnStarted { turn_id } if turn_id == "turn-w1")));
        assert!(matches!(ev.last(), Some(ChatEvent::TurnFinished { turn_id, .. }) if turn_id == "turn-w1"));
    }

    #[test]
    fn in_turn_subagent_streams_progress_without_counting_as_background() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let sink: Sink = Arc::new(move |e| o.lock().unwrap().push(e));
        let state = Arc::new(Mutex::new(State { alive: true, turn_id: Some("turn-1".into()), ..Default::default() }));
        let mut tr = Translator::new(sink, state.clone());

        tr.handle(&json!({
            "type": "system", "subtype": "task_started", "task_id": "a1", "tool_use_id": "tu1",
            "description": "Explore auth", "is_backgrounded": false, "task_type": "local_agent"
        }));
        assert_eq!(state.lock().unwrap().tasks.running(), 0);

        tr.handle(&json!({
            "type": "system", "subtype": "task_progress", "task_id": "a1",
            "description": "Searching login files", "last_tool_name": "Grep"
        }));
        assert_eq!(state.lock().unwrap().tasks.running(), 0);

        tr.handle(&json!({
            "type": "system", "subtype": "task_notification", "task_id": "a1",
            "status": "completed", "summary": "Found 3 handlers"
        }));
        assert_eq!(state.lock().unwrap().tasks.running(), 0);

        let ev = out.lock().unwrap().clone();
        let lists: Vec<&Vec<BackgroundTask>> =
            ev.iter().filter_map(|e| if let ChatEvent::Tasks { tasks } = e { Some(tasks) } else { None }).collect();
        assert_eq!(lists.len(), 3);
        assert_eq!((lists[0][0].title.as_str(), lists[0][0].background, lists[0][0].stoppable), ("Explore auth", false, true));
        assert_eq!(
            (lists[1][0].title.as_str(), lists[1][0].detail.as_deref()),
            ("Explore auth", Some("Using Grep · Searching login files"))
        );
        assert_eq!(
            (lists[2][0].status, lists[2][0].detail.as_deref(), lists[2][0].background),
            (TaskStatus::Completed, Some("Found 3 handlers"), false)
        );
        assert!(!ev.iter().any(|e| matches!(e, ChatEvent::Notice { .. })));
    }

    #[test]
    fn a_subagent_that_outlives_its_turn_shows() {
        let (ev, state) = run_with_state(&[
            json!({"type":"system","subtype":"task_started","task_id":"a1","description":"Watch CI","is_backgrounded":false,"task_type":"local_agent"}),
            json!({"type":"result","subtype":"success","total_cost_usd":0.0,"usage":{}}),
        ]);
        let lists: Vec<&Vec<BackgroundTask>> =
            ev.iter().filter_map(|e| if let ChatEvent::Tasks { tasks } = e { Some(tasks) } else { None }).collect();
        assert_eq!(lists.len(), 2);
        assert_eq!((lists[0][0].kind, lists[0][0].status, lists[0][0].background), (TaskKind::Agent, TaskStatus::Running, false));
        assert_eq!((lists[1][0].kind, lists[1][0].status, lists[1][0].background), (TaskKind::Agent, TaskStatus::Running, true));
        assert_eq!(state.lock().unwrap().tasks.running(), 1);
        // Promoted before the turn ends, so the turn's end doesn't settle it as cut off.
        let promoted = ev.iter().rposition(|e| matches!(e, ChatEvent::Tasks { .. })).unwrap();
        let finished = ev.iter().position(|e| matches!(e, ChatEvent::TurnFinished { .. })).unwrap();
        assert!(promoted < finished);
    }

    #[test]
    fn interrupted_turn_stops_in_turn_subagent_and_keeps_background_tasks() {
        let (ev, state) = run_with_state(&[
            json!({
                "type": "system", "subtype": "task_started", "task_id": "b1",
                "description": "Dev server", "is_backgrounded": true, "task_type": "local_bash"
            }),
            json!({
                "type": "system", "subtype": "task_started", "task_id": "a1",
                "description": "Explore auth", "is_backgrounded": false, "task_type": "local_agent"
            }),
            json!({"type":"result","subtype":"error_during_execution","is_error":true,"total_cost_usd":0.0,"usage":{}}),
        ]);
        let lists: Vec<&Vec<BackgroundTask>> =
            ev.iter().filter_map(|e| if let ChatEvent::Tasks { tasks } = e { Some(tasks) } else { None }).collect();
        let last = lists.last().unwrap();
        let b1 = last.iter().find(|t| t.id == "b1").unwrap();
        let a1 = last.iter().find(|t| t.id == "a1").unwrap();
        assert_eq!((b1.status, b1.background), (TaskStatus::Running, true));
        assert_eq!((a1.status, a1.background), (TaskStatus::Stopped, false));
        assert_eq!(state.lock().unwrap().tasks.running(), 1);
    }

    #[test]
    fn translates_a_turn() {
        let ev = run(&[
            json!({"type":"system","subtype":"init","session_id":"s1","model":"claude-haiku-5-5","cwd":"/x"}),
            json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}),
            json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}}),
            json!({"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Hi"}]}}),
            json!({"type":"assistant","message":{"id":"m2","content":[{"type":"tool_use","id":"tu1","name":"Edit","input":{"file_path":"a.txt","old_string":"hello","new_string":"hello world"}}]}}),
            json!({"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Edit","input":{"file_path":"a.txt","old_string":"hello","new_string":"hello world"},"permission_suggestions":[{"type":"setMode","mode":"acceptEdits","destination":"session"}],"tool_use_id":"tu1"}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":"ok"}]},"tool_use_result":{"filePath":"a.txt","structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-hello","+hello world"]}]}}),
            json!({"type":"result","subtype":"success","is_error":false,"duration_ms":6499,"total_cost_usd":0.0123,"usage":{"input_tokens":6,"output_tokens":506,"cache_read_input_tokens":100487,"cache_creation_input_tokens":54628,"iterations":[{"input_tokens":2,"cache_read_input_tokens":54008,"cache_creation_input_tokens":620}]},"modelUsage":{"claude-haiku-5-5":{"contextWindow":1000000}}}),
        ]);
        assert!(matches!(&ev[0], ChatEvent::Started { session_id: Some(s), .. } if s == "s1"));
        assert!(matches!(&ev[1], ChatEvent::TextDelta { item_id, text } if item_id == "m1:0" && text == "Hi"));
        assert!(!ev.iter().any(|e| matches!(e, ChatEvent::Text { .. })), "streamed text isn't sent twice");
        assert!(
            matches!(&ev[2], ChatEvent::ToolStarted { item_id, tool } if item_id == "tu1" && tool.kind == ToolKind::Edit && tool.title == "a.txt")
        );
        match &ev[3] {
            ChatEvent::Approval { request } => {
                assert_eq!(request.kind, ApprovalKind::Edit);
                assert!(request.can_remember);
                assert_eq!(request.diff.as_ref().unwrap().added, 1);
            }
            e => panic!("{e:?}"),
        }
        assert!(matches!(&ev[4], ChatEvent::ToolFinished { ok: true, diff: Some(d), .. } if d.added == 1 && d.removed == 1));
        match &ev[5] {
            ChatEvent::TurnFinished { turn_id, usage: Some(u), cost_usd: Some(c), .. } => {
                assert_eq!(turn_id, "turn-1");
                assert_eq!(u.output_tokens, 506);
                assert!((c - 0.0123).abs() < 1e-9);
            }
            e => panic!("{e:?}"),
        }
        assert!(matches!(&ev[6], ChatEvent::Context { used_tokens: 54630, window_tokens: 1_000_000, .. }));
    }

    #[test]
    fn asks_questions_and_queues_turns() {
        let ev = run(&[
            json!({"type":"control_request","request_id":"q1","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[{"question":"Which database?","header":"DB","multiSelect":false,"options":[{"label":"Postgres","description":"SQL"},{"label":"SQLite","description":"file"}]}]}}}),
        ]);
        match &ev[0] {
            ChatEvent::Question { request } => {
                assert_eq!(request.questions[0].id, "q0");
                assert_eq!(request.questions[0].options[1].label, "SQLite");
            }
            e => panic!("{e:?}"),
        }
        let input =
            json!({"questions":[{"question":"Which database?","multiSelect":false},{"question":"Which extras?","multiSelect":true}]});
        let mut a = std::collections::BTreeMap::new();
        a.insert("q0".to_string(), vec!["SQLite".to_string()]);
        a.insert("q1".to_string(), vec!["auth".to_string(), "jobs".to_string()]);
        let r = question_response(&input, &a);
        assert_eq!(r["behavior"], "allow");
        assert_eq!(r["updatedInput"]["answers"]["Which database?"], "SQLite");
        assert_eq!(r["updatedInput"]["answers"]["Which extras?"], json!(["auth", "jobs"]));
        assert_eq!(question_response(&input, &Default::default())["behavior"], "deny");
        let m =
            parse_models(&json!({"models":[{"value":"opus","displayName":"Opus 5.5","description":"x"},{"value":"old","disabled":true}]}));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "Opus 5.5");
    }

    #[test]
    fn steers_fold_into_the_running_turn() {
        let result = |uuids: &[&str]| json!({"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.0,"usage":{},"user_message_uuids":uuids});
        let started = |ev: &[ChatEvent]| ev.iter().filter(|e| matches!(e, ChatEvent::TurnStarted { .. })).count();
        let finished = |ev: &[ChatEvent]| ev.iter().filter(|e| matches!(e, ChatEvent::TurnFinished { .. })).count();
        let run_waiting = |waiting: &[&str], lines: &[Value]| {
            let out = Arc::new(Mutex::new(Vec::new()));
            let o = out.clone();
            let sink: Sink = Arc::new(move |e| o.lock().unwrap().push(e));
            let waiting = waiting.iter().map(|s| s.to_string()).collect();
            let state = Arc::new(Mutex::new(State { alive: true, turn_id: Some("turn-1".into()), waiting, ..Default::default() }));
            let mut tr = Translator::new(sink, state.clone());
            for l in lines {
                tr.handle(l);
            }
            let ev = out.lock().unwrap().clone();
            let s = state.lock().unwrap();
            (ev, s.turn_id.clone(), s.waiting.clone())
        };
        // Folded: one result answers both, and the chat is idle after it.
        let (ev, turn, waiting) = run_waiting(&["a", "b"], &[result(&["a", "b"])]);
        assert_eq!((finished(&ev), started(&ev), turn, waiting.len()), (1, 0, None, 0));
        // Too late to fold: it runs as the next turn, which its own result ends.
        let (ev, turn, waiting) = run_waiting(&["a", "b"], &[result(&["a"])]);
        assert_eq!((finished(&ev), started(&ev), turn.is_some(), waiting), (1, 1, true, vec!["b".to_string()]));
        let (ev, turn, _) = run_waiting(&["a", "b"], &[result(&["a"]), result(&["b"])]);
        assert_eq!((finished(&ev), started(&ev), turn), (2, 1, None));
        // Stopped with one queued behind it: the stopped turn names only its own message (as
        // Claude Code 2.1.319 does), and the queued one still runs, as the next turn.
        let stopped = |uuids: &[&str]| json!({"type":"result","subtype":"error_during_execution","is_error":true,"total_cost_usd":0.0,"usage":{},"user_message_uuids":uuids});
        let (ev, turn, waiting) = run_waiting(&["a", "b"], &[stopped(&["a"])]);
        assert_eq!((finished(&ev), started(&ev), turn.is_some(), waiting), (1, 1, true, vec!["b".to_string()]));
        let (ev, turn, _) = run_waiting(&["a", "b"], &[stopped(&["a"]), result(&["b"])]);
        assert_eq!((finished(&ev), started(&ev), turn), (2, 1, None));
        // Stopped with a message folded into it: both named, nothing runs after.
        let (ev, turn, _) = run_waiting(&["a", "b"], &[stopped(&["a", "b"])]);
        assert_eq!((finished(&ev), started(&ev), turn), (1, 0, None));
        // A CLI that doesn't name them: an interrupt drops what it cut off.
        let bare = json!({"type":"result","subtype":"error_during_execution","is_error":true,"total_cost_usd":0.0,"usage":{}});
        let (ev, turn, _) = run_waiting(&["a", "b"], &[bare]);
        assert_eq!((finished(&ev), started(&ev), turn), (1, 0, None));
        // A CLI that doesn't list them: one result per message, as before.
        let (ev, turn, _) = run_waiting(&["a", "b"], &[json!({"type":"result","subtype":"success","total_cost_usd":0.0,"usage":{}})]);
        assert_eq!((finished(&ev), started(&ev), turn.is_some()), (1, 1, true));
        assert_eq!(message_uuid("0F8FAD5B-D9CB-469F-A165-70867728950E"), "0f8fad5b-d9cb-469f-a165-70867728950e");
        let made = message_uuid("compact-1");
        assert_eq!((made.len(), &made[14..15]), (36, "4"));
    }

    #[test]
    fn maps_tools_and_todos() {
        assert_eq!(tool_call("Bash", &json!({"command":"ls -la"})).title, "ls -la");
        assert_eq!(tool_call("Grep", &json!({"pattern":"fn main"})).kind, ToolKind::Search);
        assert_eq!(tool_call("mcp__github__list_issues", &json!({"repo": "a/b"})).title, "github · list_issues");
        assert_eq!(tool_call("mcp__my_srv__sub__tool", &json!({})).title, "my_srv · sub__tool");
        assert_eq!(tool_call("Skill", &json!({"skill": "commit"})).title, "commit");
        assert_eq!(tool_call("Skill", &json!({"skill": "review", "args": "main..HEAD"})).title, "review main..HEAD");
        assert_eq!(tool_call("Skill", &json!({})).title, "Skill");
        let ev = run(&[
            json!({"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"t","name":"TodoWrite","input":{"todos":[{"content":"a","status":"completed"},{"content":"b","status":"in_progress"}]}}]}}),
        ]);
        assert!(matches!(&ev[0], ChatEvent::Plan { items } if items.len() == 2 && items[1].status == PlanStatus::InProgress));
    }

    #[test]
    fn mcp_approvals_show_readable_title_and_input_detail() {
        let ev = run(&[
            json!({"type":"control_request","request_id":"m1","request":{"subtype":"can_use_tool","tool_name":"mcp__github__create_issue","input":{"owner":"moonfish","repo":"app","title":"Fix bug"},"tool_use_id":"tu1"}}),
            json!({"type":"control_request","request_id":"m2","request":{"subtype":"can_use_tool","tool_name":"mcp__github__ping","input":{},"decision_reason":"MCP server requires approval"}}),
        ]);
        match &ev[0] {
            ChatEvent::Approval { request } => {
                assert_eq!(request.kind, ApprovalKind::Other);
                assert_eq!(request.title, "Use github · create_issue");
                assert_eq!(request.detail.as_deref(), Some("owner: moonfish\nrepo: app\ntitle: Fix bug"));
            }
            e => panic!("{e:?}"),
        }
        match &ev[1] {
            ChatEvent::Approval { request } => {
                assert_eq!(request.kind, ApprovalKind::Other);
                assert_eq!(request.title, "Use github · ping");
                assert_eq!(request.detail.as_deref(), Some("MCP server requires approval"));
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn auth_failure_emits_single_error_and_stops_stale_session() {
        assert!(is_auth_error("Not logged in · Please run /login"));
        assert!(is_auth_error("AUTHENTICATION_FAILED"));
        assert!(is_auth_error("Authentication required"));
        assert!(!is_auth_error("Rate limit exceeded"));

        let out = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let sink: Sink = Arc::new(move |e| o.lock().unwrap().push(e));
        let state = Arc::new(Mutex::new(State { alive: true, turn_id: Some("turn-1".into()), ..Default::default() }));
        let mut cmd = std::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" });
        if cfg!(windows) {
            cmd.args(["/C", "exit", "0"]);
        } else {
            cmd.args(["-c", "exit 0"]);
        }
        cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let mut proc = cmd.spawn().unwrap();
        let stdin = Arc::new(Mutex::new(proc.stdin.take().map(Stdin::Child)));
        assert!(stdin.lock().unwrap().is_some());

        let mut tr = Translator::new(sink, state.clone());
        tr.stdin = Some(stdin.clone());
        tr.handle(&json!({
            "type": "assistant",
            "error": "authentication_failed",
            "is_api_error_message": true,
            "message": {"id": "m-auth", "model": "<synthetic>", "content": [{"type": "text", "text": "Not logged in · Please run /login"}]}
        }));
        tr.handle(&json!({
            "type": "result",
            "is_error": true,
            "subtype": "success",
            "terminal_reason": "api_error",
            "result": "Not logged in · Please run /login",
            "total_cost_usd": 0.0,
            "usage": {}
        }));
        let _ = proc.wait();

        let ev = out.lock().unwrap().clone();
        assert!(!ev.iter().any(|e| matches!(e, ChatEvent::Text { .. })), "synthetic API error should not emit Text: {ev:?}");
        let errors: Vec<&str> =
            ev.iter().filter_map(|e| if let ChatEvent::Error { message } = e { Some(message.as_str()) } else { None }).collect();
        assert_eq!(errors, ["Not logged in · Please run /login"]);
        assert!(ev.iter().any(|e| matches!(e, ChatEvent::TurnFinished { .. })));
        assert!(!state.lock().unwrap().alive);
        assert!(stdin.lock().unwrap().is_none());
    }

    #[test]
    fn normal_assistant_text_still_emits_text() {
        let ev = run(&[
            json!({"type": "assistant", "message": {"id": "m1", "content": [{"type": "text", "text": "Hello from Claude"}]}}),
            json!({"type": "result", "is_error": false, "subtype": "success", "total_cost_usd": 0.0, "usage": {}}),
        ]);
        assert!(ev.iter().any(|e| matches!(e, ChatEvent::Text { text, .. } if text == "Hello from Claude")));
        assert!(!ev.iter().any(|e| matches!(e, ChatEvent::Error { .. })));
    }
}
