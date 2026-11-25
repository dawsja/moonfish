//! Antigravity over the Agent Client Protocol (ACP, v1):
//! `agy acp`, using JSON-RPC 2.0 on stdio.
//!
//! We send `initialize`, then `session/new` (or `session/load` / `session/resume` to reopen a
//! chat), and one `session/prompt` at a time: messages sent while a prompt runs wait their turn.
//! The agent streams `session/update` notifications (message and thought chunks, tool calls and
//! their updates, plans, usage, config options) and asks with requests:
//! `session/request_permission` (answered with one of its options) and `elicitation/create`
//! (a form, shown as a question card). We advertise no file system or terminal, so `fs/*` and
//! `terminal/*` requests are refused.

use moonfish_transport::Proc;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use moonfish_core::settings::{AgentKind, Location, PermissionMode};
use moonfish_transport::Spec;
use serde_json::{json, Value};

use crate::driver::{Driver, DriverResult, Prompt, Sink, StartOptions};
use crate::event::*;
use crate::jsonrpc::Rpc;

const PROTOCOL: u64 = 1;
/// A prompt can run for a long time; it ends when the agent says so (or is cancelled).
const PROMPT_LIMIT: Duration = Duration::from_secs(12 * 60 * 60);

pub struct AcpDriver {
    rpc: Rpc,
    child: Arc<Mutex<Option<Proc>>>,
    state: Arc<Mutex<State>>,
    sink: Sink,
    queue: Arc<Mutex<VecDeque<Prompt>>>,
    permission: PermissionMode,
    launch: StartOptions,
    subagent_drivers: HashMap<String, AcpDriver>,
}

#[derive(Default)]
struct State {
    session_id: Option<String>,
    turn_id: Option<String>,
    turns: u32,
    /// A prompt is running.
    busy: bool,
    alive: bool,
    resumable: bool,
    /// `session/load` replays history as updates; the chat already has it.
    loading: bool,
    /// Text and thought chunks without a message id are grouped per stretch between tool calls.
    stretch: u32,
    /// Tool calls so far, to merge partial updates.
    tools: HashMap<String, ToolCall>,
    /// Permission requests waiting: our id → (JSON-RPC id, the offered options).
    pending: HashMap<String, (Value, Vec<Value>)>,
    /// Form elicitations waiting: our id → (JSON-RPC id, the fields).
    questions: HashMap<String, (Value, Vec<Field>)>,
    /// Antigravity's questions, asked as permission requests whose options are the answers:
    /// our id → (JSON-RPC id, the options).
    choices: HashMap<String, (Value, Vec<Value>)>,
    /// The mode to go back to when the agent leaves plan mode by itself.
    after_plan: PermissionMode,
    /// The config option that picks the model (`session/set_config_option`), or none when the
    /// agent uses `session/set_model`.
    model_option: Option<String>,
    /// Mode switching: a config option id, or the session's modes.
    mode_option: Option<String>,
    modes: Vec<String>,
    /// The reasoning effort option (category `thought_level`) and its values, for the current
    /// model.
    effort_option: Option<String>,
    efforts: Vec<String>,
    /// Cumulative cost the agent reported; turns get the difference. None until known: a
    /// reopened session's earlier cost comes with its replay, if at all.
    cost: Option<f64>,
    cost_at_turn: Option<f64>,
    /// The agent takes images in prompts.
    images: bool,
    /// The running turn is `/compact`: its summary isn't shown, a note is.
    compacting: bool,
    /// Which CLI this is: modes, sign-in and model listings differ.
    kind: Option<AgentKind>,
    /// Each model's efforts as first listed (Antigravity's per-effort model ids), kept for the
    /// model lists that come later without them.
    known_efforts: HashMap<String, (Vec<String>, Option<String>)>,
    /// Set when the process starts; part of every turn id.
    nonce: String,
    permission: PermissionMode,
    /// Tool calls started this turn and not finished: one still open when the turn ends was
    /// left running (Antigravity's background commands), and shows as a background task.
    open: HashMap<String, ToolCall>,
    tasks: TaskList,
    location: Location,
    subagent_sessions: HashMap<String, String>,
    direct_subagents: HashSet<String>,
}

/// A form field from an elicitation, and how its answer goes back.
#[derive(Clone, Debug)]
pub(crate) struct Field {
    key: String,
    kind: FieldKind,
    /// `<key>__other` / `<key>_custom`: where a typed answer goes.
    other_key: Option<String>,
    /// The offered choices as (value sent back, label shown).
    choices: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
enum FieldKind {
    Bool,
    Choice,
    Multi,
    Text,
}

impl AcpDriver {
    pub fn start(opts: StartOptions, sink: Sink) -> DriverResult<AcpDriver> {
        let kind = opts.kind;
        let name = kind.label();
        let program = match opts.binary.trim() {
            "" => kind.default_binary().to_string(),
            b => b.to_string(),
        };
        let args = vec!["acp".to_string()];
        let mut env = opts.env.clone();
        if kind == AgentKind::Antigravity {
            crate::antigravity::acp_env_for(&opts.location, &mut env);
        }
        let mut child = moonfish_transport::spawn(&Spec { location: &opts.location, cwd: &opts.cwd, program: &program, args, env: &env })
            .map_err(|e| format!("couldn't start {program}: {e}"))?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let rpc = Rpc::new(child.stdin.take(), name);
        let child = Arc::new(Mutex::new(Some(child)));
        let nonce = format!(
            "{:x}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0) % 0xffffff
        );
        let launch = opts.clone();
        let after_plan = if opts.permission == PermissionMode::Plan { PermissionMode::Ask } else { opts.permission };
        let state = Arc::new(Mutex::new(State {
            alive: true,
            permission: opts.permission,
            after_plan,
            nonce,
            kind: Some(kind),
            location: opts.location.clone(),
            ..Default::default()
        }));

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
            let (sink, state, rpc, child, program) = (sink.clone(), state.clone(), rpc.clone(), child.clone(), program.clone());
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
                    let mut s = state.lock().unwrap();
                    s.open.clear();
                    let tasks = s.tasks.stop_all().then(|| s.tasks.event());
                    (s.turn_id.take(), tasks)
                };
                if let Some(turn_id) = turn {
                    sink(ChatEvent::TurnFinished { turn_id, usage: None, cost_usd: None, duration_ms: None, interrupted: true });
                }
                // What it left running went with it.
                if let Some(e) = tasks {
                    sink(e);
                }
                let message = if code == Some(127) {
                    Some(format!("{program} isn't installed there, or isn't on that machine's PATH."))
                } else {
                    Some(tail.lock().unwrap().clone()).filter(|m| !m.is_empty())
                };
                sink(ChatEvent::Exited { code, message });
            });
        }

        let d = AcpDriver {
            rpc,
            child,
            state,
            sink,
            queue: Arc::new(Mutex::new(VecDeque::new())),
            permission: opts.permission,
            launch,
            subagent_drivers: HashMap::new(),
        };
        let init = d.rpc.call(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false, "elicitation": {"form": {}}},
                "clientInfo": {"name": "moonfish", "title": "Moonfish", "version": env!("CARGO_PKG_VERSION")}
            }),
            Duration::from_secs(60),
        )?;
        if init["protocolVersion"].as_u64().is_some_and(|v| v != PROTOCOL) {
            return Err(format!("{name} speaks ACP version {}, Moonfish speaks {PROTOCOL}", init["protocolVersion"]));
        }
        if let Some(msg) = init["agentInfo"]["version"].as_str().and_then(|v| crate::probe::outdated(opts.kind, v)) {
            return Err(msg);
        }
        let caps = &init["agentCapabilities"];
        {
            let mut st = d.state.lock().unwrap();
            st.resumable = caps["loadSession"].as_bool() == Some(true) || !caps["sessionCapabilities"]["resume"].is_null();
            st.images = caps["promptCapabilities"]["image"].as_bool() == Some(true);
        }
        let session_params = |id: Option<&str>| {
            // ACP requires the server list for new and loaded sessions.
            let mut p = json!({"cwd": opts.cwd, "mcpServers": []});
            if let Some(id) = id {
                p["sessionId"] = json!(id);
            }
            p
        };
        // Reopen the chat's session if we can; otherwise start a new one.
        let mut session = None;
        if let Some(id) = &opts.resume {
            // Prefer resuming without replaying the conversation.
            let reopen = if !caps["sessionCapabilities"]["resume"].is_null() {
                d.rpc.call("session/resume", session_params(Some(id)), Duration::from_secs(120))
            } else if caps["loadSession"].as_bool() == Some(true) {
                d.state.lock().unwrap().loading = true;
                let r = d.rpc.call("session/load", session_params(Some(id)), Duration::from_secs(120));
                d.state.lock().unwrap().loading = false;
                r
            } else {
                Err("can't reopen sessions".into())
            };
            match reopen {
                Ok(mut r) => {
                    r["sessionId"] = json!(id);
                    session = Some(r);
                }
                // No answer, or the agent went away: the session may well still be there, so
                // the chat keeps it (and fails to start) instead of carrying on in a new one
                // that remembers nothing.
                Err(e) if e.contains("didn't answer") || e.ends_with(" exited") => {
                    return Err(format!("Couldn't reopen this chat's session. {e}. Try again."));
                }
                Err(e) => (d.sink)(ChatEvent::Notice {
                    level: NoticeLevel::Warn,
                    text: format!("Couldn't reopen the earlier session, so this continues in a new session. Details: {e}"),
                }),
            }
        }
        let new_session = || d.rpc.call("session/new", session_params(None), Duration::from_secs(120));
        let session = match session {
            Some(s) => s,
            None => match new_session() {
                Ok(s) => {
                    // A new session has cost nothing yet.
                    d.state.lock().unwrap().cost = Some(0.0);
                    s
                }
                Err(e) if is_auth_error(&e) => return Err(not_signed_in(kind, &opts.location)),
                Err(e) => return Err(e),
            },
        };
        let session_id = session["sessionId"].as_str().ok_or_else(|| format!("{name} didn't start a session"))?.to_string();
        d.state.lock().unwrap().session_id = Some(session_id.clone());

        let (models, current) = d.read_options(&session);
        (d.sink)(ChatEvent::Started { session_id: Some(session_id), model: current.clone(), cwd: Some(opts.cwd.clone()) });
        if !models.is_empty() {
            (d.sink)(ChatEvent::Models { models, current: current.clone() });
        }
        if let Some(m) = opts.model.as_deref().filter(|m| Some(*m) != current.as_deref()) {
            let mut me = d;
            if let Err(e) = me.set_model(m) {
                (me.sink)(ChatEvent::Notice { level: NoticeLevel::Warn, text: format!("Couldn't switch to {m}: {e}") });
            }
            let _ = me.apply_mode(opts.permission);
            if let Some(e) = opts.effort.as_deref().filter(|e| !e.is_empty()) {
                let _ = me.set_effort(e);
            }
            return Ok(me);
        }
        let _ = d.apply_mode(opts.permission);
        let mut d = d;
        if let Some(e) = opts.effort.as_deref().filter(|e| !e.is_empty()) {
            let _ = d.set_effort(e);
        }
        Ok(d)
    }

    /// The model and mode pickers from `session/new`'s config options (or its unstable
    /// `models` / `modes`).
    fn read_options(&self, session: &Value) -> (Vec<ModelOption>, Option<String>) {
        let mut st = self.state.lock().unwrap();
        let (mut models, current, model_option) = models_from(session);
        st.model_option = model_option;
        read_effort(&mut st, session);
        add_efforts(&mut models, current.as_deref(), &st.efforts);
        variant_efforts(&mut models, &session["models"]);
        st.known_efforts = models
            .iter()
            .filter(|m| !m.efforts.is_empty())
            .map(|m| (m.id.clone(), (m.efforts.clone(), m.default_effort.clone())))
            .collect();
        if let Some(opts) = session["configOptions"].as_array() {
            if let Some(mode) = opts.iter().find(|o| o["category"] == "mode" || o["id"] == "mode") {
                st.mode_option = mode["id"].as_str().map(String::from);
                st.modes = select_values(&mode["options"]).into_iter().map(|m| m.id).collect();
            }
        }
        if st.modes.is_empty() {
            st.modes = session["modes"]["availableModes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m["id"].as_str().map(String::from))
                .collect();
        }
        (models, current)
    }

    /// A config option change answers with every option as it is now: the model, its efforts.
    fn adopt_options(&self, r: &Value) {
        if !r["configOptions"].is_array() {
            return;
        }
        let mut st = self.state.lock().unwrap();
        read_effort(&mut st, r);
        let (mut models, current, _) = models_from(r);
        // Efforts as first listed (in order) before the option's.
        known_efforts(&mut models, &st.known_efforts);
        add_efforts(&mut models, current.as_deref(), &st.efforts);
        drop(st);
        if !models.is_empty() {
            (self.sink)(ChatEvent::Models { models, current });
        }
    }

    fn session(&self) -> DriverResult<String> {
        let s = self.state.lock().unwrap();
        if !s.alive {
            return Err(format!("{} isn't running", self.rpc.name));
        }
        s.session_id.clone().ok_or_else(|| format!("{} has no session", self.rpc.name))
    }

    fn apply_mode(&self, mode: PermissionMode) -> DriverResult<()> {
        let (option, modes, kind) = {
            let s = self.state.lock().unwrap();
            (s.mode_option.clone(), s.modes.clone(), s.kind)
        };
        let want = acp_wire_mode(kind, mode);
        if !modes.iter().any(|m| m == want) {
            return Ok(());
        }
        let sid = self.session()?;
        match option {
            Some(id) => self.rpc.call(
                "session/set_config_option",
                json!({"sessionId": sid, "configId": id, "value": want}),
                Duration::from_secs(30),
            ),
            None => self.rpc.call("session/set_mode", json!({"sessionId": sid, "modeId": want}), Duration::from_secs(30)),
        }
        .map(|_| ())
    }
}

/// Starts `prompt` now; when it ends, the next queued one starts.
fn run_prompt(rpc: Rpc, state: Arc<Mutex<State>>, sink: Sink, queue: Arc<Mutex<VecDeque<Prompt>>>, prompt: Prompt) {
    let (sid, turn_id, images) = {
        let mut s = state.lock().unwrap();
        s.busy = true;
        s.turns += 1;
        s.stretch += 1;
        s.cost_at_turn = s.cost;
        // Unique per process: a restarted agent counts from 1 again, and ids made from this one
        // (text without a message id) must not land in an earlier message.
        let id = format!("turn-{}-{}", s.nonce, s.turns);
        s.turn_id = Some(id.clone());
        (s.session_id.clone().unwrap_or_default(), id, s.images)
    };
    sink(ChatEvent::TurnStarted { turn_id: turn_id.clone() });
    let mut blocks = vec![json!({"type": "text", "text": prompt.text})];
    if images {
        for img in &prompt.images {
            blocks.push(json!({"type": "image", "mimeType": img.mime, "data": img.data}));
        }
    } else if !prompt.images.is_empty() {
        sink(ChatEvent::Notice {
            level: NoticeLevel::Warn,
            text: "This agent doesn't accept images, so Moonfish didn't send them.".into(),
        });
    }
    thread::spawn(move || {
        let started = std::time::Instant::now();
        let r = rpc.call("session/prompt", json!({"sessionId": sid, "prompt": blocks}), PROMPT_LIMIT);
        let (cost, interrupted, compacted, tasks) = {
            let mut s = state.lock().unwrap();
            s.busy = false;
            s.turn_id = None;
            let ended_well = r.as_ref().is_ok_and(|v| v["stopReason"] != "cancelled");
            let tasks = finish_turn_tasks(&mut s, ended_well);
            s.pending.clear();
            s.questions.clear();
            s.choices.clear();
            let reason = r.as_ref().ok().and_then(|v| v["stopReason"].as_str()).unwrap_or("");
            // Without a cost from before the turn, its share can't be told: none, not all of it.
            let cost = match (s.cost, s.cost_at_turn) {
                (Some(now), Some(before)) => Some((now - before).max(0.0)),
                _ => None,
            };
            (cost, reason == "cancelled", std::mem::take(&mut s.compacting), tasks)
        };
        // Before the turn's end, so that doesn't settle these tool calls as cut off.
        if let Some(e) = tasks {
            sink(e);
        }
        if compacted && r.is_ok() && !interrupted {
            sink(ChatEvent::Notice { level: NoticeLevel::Info, text: "Context compacted.".into() });
        }
        match &r {
            Ok(v) => {
                if let Some(reason) = v["stopReason"].as_str().filter(|r| matches!(*r, "max_tokens" | "max_turn_requests" | "refusal")) {
                    let text = match reason {
                        "max_tokens" => "The agent stopped after hitting its output limit.",
                        "max_turn_requests" => "The agent stopped after hitting its step limit for one turn.",
                        _ => "The agent declined to continue.",
                    };
                    sink(ChatEvent::Notice { level: NoticeLevel::Warn, text: text.into() });
                }
                sink(ChatEvent::TurnFinished {
                    turn_id,
                    usage: usage_of(&v["usage"]),
                    cost_usd: cost.filter(|c| *c > 0.0),
                    duration_ms: Some(started.elapsed().as_millis() as u64),
                    interrupted,
                });
            }
            Err(e) => {
                if state.lock().unwrap().alive {
                    sink(ChatEvent::Error { message: e.clone() });
                }
                sink(ChatEvent::TurnFinished { turn_id, usage: None, cost_usd: None, duration_ms: None, interrupted: true });
            }
        }
        let next = queue.lock().unwrap().pop_front();
        if let Some(p) = next.filter(|_| state.lock().unwrap().alive) {
            run_prompt(rpc, state, sink, queue, p);
        }
    });
}

impl Driver for AcpDriver {
    fn send(&mut self, prompt: Prompt) -> DriverResult<()> {
        self.session()?;
        (self.sink)(ChatEvent::UserMessage {
            id: prompt.id.clone(),
            text: prompt.text.clone(),
            images: prompt.images.iter().map(|i| i.mime.clone()).collect(),
            files: prompt.files.clone(),
        });
        // Ended tasks have been seen; so have ones the agent never reports on (it can say how
        // they went now, if asked).
        let tasks = {
            let mut s = self.state.lock().unwrap();
            let untracked = s.tasks.forget_untracked();
            (s.tasks.clear_ended() | untracked).then(|| s.tasks.event())
        };
        if let Some(e) = tasks {
            (self.sink)(e);
        }
        if self.state.lock().unwrap().busy {
            // One prompt at a time: this one runs when the current one ends.
            self.queue.lock().unwrap().push_back(prompt);
            return Ok(());
        }
        run_prompt(self.rpc.clone(), self.state.clone(), self.sink.clone(), self.queue.clone(), prompt);
        Ok(())
    }

    fn interrupt(&mut self) -> DriverResult<()> {
        let sid = self.session()?;
        // Questions in flight are withdrawn with the turn.
        let (pending, questions) = {
            let mut s = self.state.lock().unwrap();
            let mut pending = s.pending.drain().collect::<Vec<_>>();
            pending.extend(s.choices.drain());
            (pending, s.questions.drain().collect::<Vec<_>>())
        };
        for (_, (id, _)) in pending {
            let _ = self.rpc.reply(&id, json!({"outcome": {"outcome": "cancelled"}}));
        }
        for (_, (id, _)) in questions {
            let _ = self.rpc.reply(&id, json!({"action": "cancel"}));
        }
        // Messages already sent wait their turn: they're in the transcript as sent, and the
        // window steers by stopping the turn and then sending, so this one must still go.
        self.rpc.notify("session/cancel", Some(json!({"sessionId": sid})))
    }

    fn answer(&mut self, request_id: &str, decision: Decision) -> DriverResult<()> {
        let (rpc_id, options) = self.state.lock().unwrap().pending.remove(request_id).ok_or("that question was already answered")?;
        (self.sink)(ChatEvent::ApprovalResolved { request_id: request_id.into(), decision });
        self.rpc.reply(&rpc_id, permission_outcome(&options, decision))
    }

    fn answer_question(&mut self, request_id: &str, answers: std::collections::BTreeMap<String, Vec<String>>) -> DriverResult<()> {
        let choice = self.state.lock().unwrap().choices.remove(request_id);
        if let Some((rpc_id, options)) = choice {
            let reply = choice_reply(&options, &answers);
            (self.sink)(ChatEvent::QuestionResolved { request_id: request_id.into(), answers });
            return self.rpc.reply(&rpc_id, reply);
        }
        let (rpc_id, fields) = self.state.lock().unwrap().questions.remove(request_id).ok_or("that question was already answered")?;
        let reply = elicitation_reply(&fields, &answers);
        (self.sink)(ChatEvent::QuestionResolved { request_id: request_id.into(), answers });
        self.rpc.reply(&rpc_id, reply)
    }

    fn set_model(&mut self, model: &str) -> DriverResult<()> {
        let sid = self.session()?;
        let option = self.state.lock().unwrap().model_option.clone();
        let r = match option {
            Some(id) => self.rpc.call(
                "session/set_config_option",
                json!({"sessionId": sid, "configId": id, "value": model}),
                Duration::from_secs(30),
            ),
            None => self.rpc.call("session/set_model", json!({"sessionId": sid, "modelId": model}), Duration::from_secs(30)),
        }?;
        if r["configOptions"].is_array() {
            self.adopt_options(&r);
        }
        Ok(())
    }

    fn set_effort(&mut self, effort: &str) -> DriverResult<bool> {
        let sid = self.session()?;
        let (option, values) = {
            let s = self.state.lock().unwrap();
            (s.effort_option.clone(), s.efforts.clone())
        };
        // No effort to pick for this model: nothing to do.
        let Some(id) = option else { return Ok(true) };
        let value = if effort.is_empty() { "default" } else { effort };
        if !values.iter().any(|v| v == value) {
            return Ok(true);
        }
        let r = self.rpc.call(
            "session/set_config_option",
            json!({"sessionId": sid, "configId": id, "value": value}),
            Duration::from_secs(30),
        )?;
        // Changing effort can also change the model.
        self.adopt_options(&r);
        Ok(true)
    }

    fn compact(&mut self, instructions: &str) -> DriverResult<bool> {
        if !instructions.trim().is_empty() {
            return Err(format!("{}'s /compact doesn't take instructions", self.rpc.name));
        }
        self.session()?;
        {
            let mut s = self.state.lock().unwrap();
            if s.busy {
                return Err("Wait for the agent to finish before compacting.".into());
            }
            s.compacting = true;
        }
        let prompt =
            Prompt { id: format!("compact-{}", self.state.lock().unwrap().turns), text: "/compact".into(), images: vec![], files: vec![] };
        run_prompt(self.rpc.clone(), self.state.clone(), self.sink.clone(), self.queue.clone(), prompt);
        Ok(true)
    }

    fn background(&self) -> usize {
        self.state.lock().unwrap().tasks.running()
    }

    fn resumable(&self) -> bool {
        self.state.lock().unwrap().resumable
    }

    fn idle(&self) -> bool {
        let s = self.state.lock().unwrap();
        !s.busy && s.pending.is_empty() && s.questions.is_empty() && s.choices.is_empty() && self.queue.lock().unwrap().is_empty()
    }

    fn set_permission(&mut self, mode: PermissionMode) -> DriverResult<()> {
        self.permission = mode;
        {
            let mut s = self.state.lock().unwrap();
            s.permission = mode;
            if mode != PermissionMode::Plan {
                s.after_plan = mode;
            }
        }
        self.apply_mode(mode)
    }

    fn send_subagent(&mut self, task_id: &str, prompt: Prompt) -> DriverResult<()> {
        let conversation_id = {
            let mut s = self.state.lock().unwrap();
            let Some(cid) = s.subagent_sessions.get(task_id).cloned() else {
                return Err("This subagent cannot be messaged directly.".into());
            };
            s.direct_subagents.insert(task_id.to_string());
            let tasks_event = s.tasks.reopen(task_id).then(|| s.tasks.event());
            drop(s);
            if let Some(e) = tasks_event {
                (self.sink)(e);
            }
            cid
        };
        if !self.subagent_drivers.contains_key(task_id) {
            let mut sub_opts = self.launch.clone();
            sub_opts.resume = Some(conversation_id);
            let parent_sink = self.sink.clone();
            let parent_state = self.state.clone();
            let tid = task_id.to_string();
            let sub_sink: Sink = Arc::new(move |event: ChatEvent| {
                let tasks_event = match &event {
                    ChatEvent::TurnFinished { interrupted, .. } => {
                        let status = if *interrupted { TaskStatus::Stopped } else { TaskStatus::Completed };
                        let mut s = parent_state.lock().unwrap();
                        s.tasks.finish(&tid, status, None).is_some().then(|| s.tasks.event())
                    }
                    ChatEvent::Exited { code, .. } => {
                        let status = if code.is_some_and(|c| c != 0) { TaskStatus::Failed } else { TaskStatus::Completed };
                        let mut s = parent_state.lock().unwrap();
                        s.tasks.finish(&tid, status, None).is_some().then(|| s.tasks.event())
                    }
                    ChatEvent::Error { message } => {
                        let mut s = parent_state.lock().unwrap();
                        s.tasks.finish(&tid, TaskStatus::Failed, Some(first_line(message))).is_some().then(|| s.tasks.event())
                    }
                    ChatEvent::ToolStarted { tool, .. } => {
                        let mut s = parent_state.lock().unwrap();
                        let detail = if tool.title.is_empty() { tool.name.clone() } else { format!("{} {}", tool.name, tool.title) };
                        s.tasks.progress(&tid, None, Some(&first_line(&detail))).then(|| s.tasks.event())
                    }
                    _ => None,
                };
                if !matches!(event, ChatEvent::Started { .. } | ChatEvent::Models { .. } | ChatEvent::Commands { .. }) {
                    parent_sink(ChatEvent::SubagentEvent { task_id: tid.clone(), event: Box::new(event) });
                }
                if let Some(e) = tasks_event {
                    parent_sink(e);
                }
            });
            let driver = AcpDriver::start(sub_opts, sub_sink)?;
            self.subagent_drivers.insert(task_id.to_string(), driver);
        }
        self.subagent_drivers.get_mut(task_id).unwrap().send(prompt)
    }

    fn interrupt_subagent(&mut self, task_id: &str) -> DriverResult<()> {
        if let Some(d) = self.subagent_drivers.get_mut(task_id) {
            d.interrupt()
        } else {
            Err("This subagent can't be interrupted from Moonfish until you message it.".into())
        }
    }

    fn stop(&mut self) {
        for (_, mut d) in self.subagent_drivers.drain() {
            d.stop();
        }
        self.rpc.stdin.lock().unwrap().take();
        // Stdin closed: it may exit by itself (saving its session); then it and everything it
        // started (commands, servers) end.
        if let Some(mut c) = self.child.lock().unwrap().take() {
            moonfish_transport::stop_tree(&mut c, std::time::Duration::from_millis(500));
        }
    }
}

impl Drop for AcpDriver {
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
        if self.rpc.take_reply(v) || v.get("method").is_none() {
            return;
        }
        let method = v["method"].as_str().unwrap_or("");
        let p = &v["params"];
        if let Some(id) = v.get("id") {
            match method {
                "session/request_permission" => self.permission(id, p),
                "elicitation/create" => self.elicitation(id, p),
                _ => {
                    let _ = self.rpc.reply_error(id, -32601, "Moonfish doesn't offer this");
                }
            }
            return;
        }
        if method == "session/update" {
            // A reopened session replays its history as updates; the chat already has it. Its
            // usage still counts: the session's cost so far, so the next turn gets only its own.
            let kind = p["update"]["sessionUpdate"].as_str().unwrap_or("");
            if self.state.lock().unwrap().loading && !matches!(kind, "available_commands_update" | "usage_update") {
                return;
            }
            self.update(&p["update"]);
        }
    }

    fn update(&mut self, u: &Value) {
        match u["sessionUpdate"].as_str().unwrap_or("") {
            kind @ ("agent_message_chunk" | "agent_thought_chunk") => {
                let Some(text) = u["content"]["text"].as_str().filter(|t| !t.is_empty()) else { return };
                // `/compact`'s summary is for the agent; the chat gets a note when it's done.
                if self.state.lock().unwrap().compacting {
                    return;
                }
                let thought = kind == "agent_thought_chunk";
                let item_id = match u["messageId"].as_str() {
                    Some(m) => format!("{m}{}", if thought { ":r" } else { "" }),
                    None => {
                        let s = self.state.lock().unwrap();
                        format!("{}-{}{}", s.turn_id.as_deref().unwrap_or("t"), if thought { "r" } else { "a" }, s.stretch)
                    }
                };
                self.emit(if thought {
                    ChatEvent::ReasoningDelta { item_id, text: text.into() }
                } else {
                    ChatEvent::TextDelta { item_id, text: text.into() }
                });
            }
            "tool_call" | "tool_call_update" => self.tool(u),
            "plan" => self.emit(ChatEvent::Plan {
                items: u["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| PlanItem {
                        text: e["content"].as_str().unwrap_or("").into(),
                        status: match e["status"].as_str() {
                            Some("completed") => PlanStatus::Completed,
                            Some("in_progress") => PlanStatus::InProgress,
                            _ => PlanStatus::Pending,
                        },
                    })
                    .collect(),
            }),
            "usage_update" => {
                if let Some(amount) = u["cost"]["amount"].as_f64() {
                    self.state.lock().unwrap().cost = Some(amount);
                }
                if let (Some(used), Some(size)) = (u["used"].as_u64(), u["size"].as_u64()) {
                    self.emit(ChatEvent::Context { used_tokens: used, window_tokens: size, parts: vec![] });
                }
            }
            "config_option_update" => {
                let (mut models, current, option) = models_from(u);
                let mut st = self.state.lock().unwrap();
                read_effort(&mut st, u);
                if !models.is_empty() {
                    st.model_option = option;
                    known_efforts(&mut models, &st.known_efforts);
                    add_efforts(&mut models, current.as_deref(), &st.efforts);
                    drop(st);
                    self.emit(ChatEvent::Models { models, current });
                }
            }
            "available_commands_update" => {
                let mut commands = acp_commands(&u["availableCommands"]);
                if self.state.lock().unwrap().kind == Some(AgentKind::Antigravity) {
                    // Antigravity's `logout` would sign every chat out.
                    commands.retain(|c| c.name != "logout");
                }
                self.emit(ChatEvent::Commands { commands })
            }
            "current_mode_update" => {
                let Some(mode) = u["currentModeId"].as_str() else { return };
                let changed = {
                    let mut st = self.state.lock().unwrap();
                    match (mode == "plan", st.permission == PermissionMode::Plan) {
                        (true, false) => {
                            st.after_plan = st.permission;
                            st.permission = PermissionMode::Plan;
                            Some(PermissionMode::Plan)
                        }
                        (false, true) => {
                            st.permission = st.after_plan;
                            Some(st.after_plan)
                        }
                        _ => None,
                    }
                };
                if let Some(mode) = changed {
                    self.emit(ChatEvent::PermissionChanged { mode });
                }
            }
            "notice" => {
                if let Some(t) = u["message"].as_str().or(u["text"].as_str()) {
                    self.emit(ChatEvent::Notice { level: NoticeLevel::Info, text: t.into() });
                }
            }
            _ => {}
        }
    }

    fn tool(&mut self, u: &Value) {
        let Some(id) = u["toolCallId"].as_str().map(String::from) else { return };
        // A question's tool call ("Which color?" → "Response received"): the card is its record.
        if is_question(&id) && self.state.lock().unwrap().kind == Some(AgentKind::Antigravity) {
            return;
        }
        let mut st = self.state.lock().unwrap();
        // A text after this tool call is a new stretch.
        if u["sessionUpdate"] == "tool_call" {
            st.stretch += 1;
        }
        let known = st.tools.get(&id).cloned();
        let mut call = known.clone().unwrap_or(ToolCall {
            kind: ToolKind::Other,
            name: String::new(),
            title: String::new(),
            input: Value::Null,
            path: None,
        });
        if let Some(k) = u["kind"].as_str() {
            let next = tool_kind(k);
            if next != ToolKind::Other || call.kind == ToolKind::Other {
                call.kind = next;
            }
            call.name = k.to_string();
        }
        if let Some(t) = u["title"].as_str().filter(|t| !t.is_empty()) {
            call.title = t.to_string();
        }
        if !u["rawInput"].is_null() {
            call.input = u["rawInput"].clone();
            if call.title.is_empty() {
                call.title = input_title(&call.input);
            }
        }
        if let Some(p) = u["locations"][0]["path"].as_str() {
            call.path = Some(p.to_string());
        }
        if call.title.is_empty() {
            call.title = call.path.clone().unwrap_or_else(|| "tool".into());
        }
        call.kind = classify_tool(u, &call);
        st.tools.insert(id.clone(), call.clone());
        drop(st);
        if known.as_ref() != Some(&call) {
            self.emit(ChatEvent::ToolStarted { item_id: id.clone(), tool: call.clone() });
        }
        match u["status"].as_str() {
            Some("completed") | Some("failed") => {
                let ok = u["status"] == "completed";
                let diff = diffs_of(&u["content"]);
                let output = clip(output_of(u));
                let launched = if ok && call.kind == ToolKind::Task { parse_subagent_launch(&output) } else { None };
                if let Some((conv_id, log_path)) = launched {
                    let (tasks, location, should_watch) = {
                        let mut st = self.state.lock().unwrap();
                        st.open.remove(&id);
                        let loading = st.loading;
                        if !loading {
                            let mut t = BackgroundTask::running(id.clone(), TaskKind::Agent, subagent_title(&call));
                            t.item_id = Some(id.clone());
                            t.session_id = Some(conv_id.clone());
                            t.interactive = true;
                            st.tasks.add(t);
                            st.tasks.promote_to_background(&id);
                            st.tasks.set_subagent_session(&id, conv_id.clone(), true);
                        }
                        let first_seen = st.subagent_sessions.insert(id.clone(), conv_id.clone()).is_none();
                        ((!loading).then(|| st.tasks.event()), st.location.clone(), !loading && first_seen)
                    };
                    self.emit(ChatEvent::ToolFinished { item_id: id.clone(), ok, output, diff });
                    if let Some(tasks) = tasks {
                        self.emit(tasks);
                    }
                    if should_watch {
                        watch_antigravity_subagent(id, conv_id, log_path, location, self.state.clone(), self.sink.clone());
                    }
                } else {
                    let (tasks, notice) = {
                        let mut st = self.state.lock().unwrap();
                        st.open.remove(&id);
                        let status = if ok { TaskStatus::Completed } else { TaskStatus::Failed };
                        let detail = last_line(&output);
                        if call.kind == ToolKind::Task && st.tasks.get(&id).is_none() && !st.loading {
                            let mut t = if st.turn_id.is_some() {
                                BackgroundTask::in_turn(id.clone(), TaskKind::Agent, subagent_title(&call))
                            } else {
                                BackgroundTask::running(id.clone(), TaskKind::Agent, subagent_title(&call))
                            };
                            t.item_id = Some(id.clone());
                            st.tasks.add(t);
                        }
                        match st.tasks.finish(&id, status, detail) {
                            Some(t) => {
                                let notice = t.background.then(|| format!("Background task ended: {}", t.title));
                                (Some(st.tasks.event()), notice)
                            }
                            None => (None, None),
                        }
                    };
                    self.emit(ChatEvent::ToolFinished { item_id: id, ok, output, diff });
                    if let Some(tasks) = tasks {
                        self.emit(tasks);
                    }
                    if let Some(text) = notice {
                        self.emit(ChatEvent::Notice { level: NoticeLevel::Info, text });
                    }
                }
            }
            _ => {
                let tasks = {
                    let mut st = self.state.lock().unwrap();
                    if st.busy && !st.loading {
                        st.open.insert(id.clone(), call.clone());
                    }
                    if call.kind == ToolKind::Task && !st.loading {
                        let title = subagent_title(&call);
                        let existing_title = st.tasks.get(&id).map(|t| t.title.clone());
                        let changed = match existing_title {
                            Some(prev) => {
                                let upgrade = is_bare_subagent_name(&prev) && !is_bare_subagent_name(&title);
                                let new_title = if upgrade { Some(title.as_str()) } else { None };
                                let detail = live_progress(u, if upgrade { &title } else { &prev });
                                st.tasks.progress(&id, new_title, detail.as_deref())
                            }
                            None => {
                                let mut t = if st.turn_id.is_some() {
                                    BackgroundTask::in_turn(id.clone(), TaskKind::Agent, title.clone())
                                } else {
                                    BackgroundTask::running(id.clone(), TaskKind::Agent, title.clone())
                                };
                                t.item_id = Some(id.clone());
                                t.detail = live_progress(u, &title);
                                st.tasks.add(t)
                            }
                        };
                        changed.then(|| st.tasks.event())
                    } else {
                        None
                    }
                };
                if let Some(tasks) = tasks {
                    self.emit(tasks);
                }
            }
        }
    }

    fn permission(&mut self, rpc_id: &Value, p: &Value) {
        let options: Vec<Value> = p["options"].as_array().cloned().unwrap_or_default();
        let tc = &p["toolCall"];
        let id = tc["toolCallId"].as_str().map(String::from);
        // Antigravity asks its questions this way: each option is an answer. Never answered by
        // the mode, whatever it allows.
        if id.as_deref().is_some_and(is_question) && self.state.lock().unwrap().kind == Some(AgentKind::Antigravity) {
            let request_id = format!("acpq-{rpc_id}");
            let question = Question {
                id: "answer".into(),
                header: String::new(),
                question: tc["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("The agent asks:").to_string(),
                options: options
                    .iter()
                    .filter_map(|o| o["name"].as_str())
                    .map(|n| QuestionOption { label: n.to_string(), description: String::new() })
                    .collect(),
                multi: false,
                other: false,
                secret: false,
            };
            self.state.lock().unwrap().choices.insert(request_id.clone(), (rpc_id.clone(), options));
            self.emit(ChatEvent::Question { request: QuestionRequest { request_id, questions: vec![question] } });
            return;
        }
        let known = id.as_ref().and_then(|i| self.state.lock().unwrap().tools.get(i).cloned());
        let kind = tc["kind"].as_str().map(tool_kind).or(known.as_ref().map(|k| k.kind)).unwrap_or(ToolKind::Other);
        // Full access answers everything, Accept edits answers edits: allow once, never always.
        let mode = self.state.lock().unwrap().permission;
        let edit = matches!(kind, ToolKind::Edit | ToolKind::Write);
        if mode == PermissionMode::FullAccess || (mode == PermissionMode::AcceptEdits && edit) {
            let _ = self.rpc.reply(rpc_id, permission_outcome(&options, Decision::Allow));
            return;
        }
        let title = tc["title"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(String::from)
            .or(known.as_ref().map(|k| k.title.clone()))
            .unwrap_or_else(|| "Use a tool".into());
        let input =
            if tc["rawInput"].is_null() { known.as_ref().map(|k| k.input.clone()).unwrap_or(Value::Null) } else { tc["rawInput"].clone() };
        let (akind, detail) = match kind {
            ToolKind::Bash => (ApprovalKind::Command, command_of(&input).map(String::from).or(Some(title.clone()))),
            ToolKind::Edit | ToolKind::Write => (ApprovalKind::Edit, None),
            _ => {
                let only_desc = input.as_object().is_some_and(|m| m.len() == 1 && m.contains_key("description"));
                let detail = if only_desc {
                    input["description"].as_str().filter(|d| !d.trim().is_empty()).map(String::from)
                } else {
                    input_summary(&input)
                };
                (ApprovalKind::Other, detail)
            }
        };
        // Antigravity warns about risky "Allow always" choices.
        let warning = options.iter().find_map(|o| {
            let w = &o["_meta"]["agy.security.warning"];
            let text = [w["title"].as_str(), w["message"].as_str()].into_iter().flatten().collect::<Vec<_>>().join(": ");
            Some(text.chars().take(512).collect::<String>()).filter(|t| !t.is_empty())
        });
        let detail = match (detail, warning) {
            (Some(d), Some(w)) => Some(format!("{d}\n\nWarning. {w}")),
            (None, Some(w)) => Some(format!("Warning. {w}")),
            (d, None) => d,
        };
        let title = match akind {
            ApprovalKind::Command => "Run a command".to_string(),
            _ => title,
        };
        let can_remember = options.iter().any(|o| o["kind"] == "allow_always");
        let request_id = format!("acp-{rpc_id}");
        self.state.lock().unwrap().pending.insert(request_id.clone(), (rpc_id.clone(), options));
        self.emit(ChatEvent::Approval {
            request: ApprovalRequest { request_id, item_id: id, kind: akind, title, detail, diff: diffs_of(&tc["content"]), can_remember },
        });
    }

    fn elicitation(&mut self, rpc_id: &Value, p: &Value) {
        if p["mode"] == "url" {
            if let Some(url) = p["url"].as_str() {
                self.emit(ChatEvent::Notice {
                    level: NoticeLevel::Info,
                    text: format!("{} {url}", p["message"].as_str().unwrap_or("The agent asks you to open:")),
                });
            }
            let _ = self.rpc.reply(rpc_id, json!({"action": "cancel"}));
            return;
        }
        let (questions, fields) = form_questions(p);
        if questions.is_empty() {
            let _ = self.rpc.reply(rpc_id, json!({"action": "accept", "content": {}}));
            return;
        }
        let request_id = format!("acpq-{rpc_id}");
        self.state.lock().unwrap().questions.insert(request_id.clone(), (rpc_id.clone(), fields));
        self.emit(ChatEvent::Question { request: QuestionRequest { request_id, questions } });
    }
}

/// Effort levels from lowest to highest (unknown ones last).
fn effort_rank(e: &str) -> usize {
    ["none", "minimal", "low", "medium", "high", "xhigh", "max"].iter().position(|x| *x == e).unwrap_or(99)
}

// ---- Helpers ---------------------------------------------------------------------------------

/// "Authentication required" (-32000), or the provider's 401 passed through.
fn is_auth_error(e: &str) -> bool {
    let e = e.to_lowercase();
    e.contains("auth") || e.contains("401") || e.contains("sign in") || e.contains("log in") || e.contains("login")
}

fn not_signed_in(kind: AgentKind, location: &Location) -> String {
    if kind == AgentKind::Antigravity {
        crate::antigravity::not_signed_in(location)
    } else {
        "Sign in from Settings under Providers.".to_string()
    }
}

fn acp_wire_mode(kind: Option<AgentKind>, mode: PermissionMode) -> &'static str {
    match (kind, mode) {
        (_, PermissionMode::Plan) => "plan",
        (Some(AgentKind::Antigravity), PermissionMode::AcceptEdits) => "accept-edits",
        _ => "default",
    }
}

fn known_efforts(models: &mut [ModelOption], known: &HashMap<String, (Vec<String>, Option<String>)>) {
    for m in models.iter_mut().filter(|m| m.efforts.is_empty()) {
        if let Some((e, default)) = known.get(&m.id) {
            m.efforts = e.clone();
            m.default_effort = m.default_effort.clone().or(default.clone());
        }
    }
}

/// Antigravity lists each model once per effort (`claude-opus-5-5-high`, unstable `models`) next
/// to its config option's base models (`claude-opus-5-5`): the suffixes are that model's efforts.
fn variant_efforts(models: &mut [ModelOption], listed: &Value) {
    let ids: Vec<&str> = listed["availableModels"].as_array().into_iter().flatten().filter_map(|m| m["modelId"].as_str()).collect();
    for m in models.iter_mut().filter(|m| m.efforts.is_empty()) {
        let mut efforts: Vec<String> = ids
            .iter()
            .filter_map(|id| id.strip_prefix(m.id.as_str())?.strip_prefix('-'))
            .filter(|e| effort_rank(e) < 99)
            .map(String::from)
            .collect();
        efforts.sort_by_key(|e| effort_rank(e));
        if efforts.len() > 1 {
            m.efforts = efforts;
        }
    }
}

/// `availableCommands: [{name, description, input: {hint}}]`.
pub(crate) fn acp_commands(v: &Value) -> Vec<SlashCommand> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some(SlashCommand {
                name: c["name"].as_str()?.trim_start_matches('/').to_string(),
                description: c["description"].as_str().unwrap_or("").into(),
                hint: c["input"]["hint"].as_str().unwrap_or("").into(),
                skill: false,
            })
        })
        .filter(|c| !c.name.is_empty())
        .collect()
}

fn finish_turn_tasks(s: &mut State, ended_well: bool) -> Option<ChatEvent> {
    // Tool calls the turn ended without: left running in the background. Antigravity
    // never says when those end; others may, with a later `tool_call_update`.
    let left: Vec<(String, ToolCall)> = s.open.drain().collect();
    let tracked = s.kind != Some(AgentKind::Antigravity);
    let mut changed = false;
    if ended_well {
        for (id, call) in left {
            if s.tasks.get(&id).is_some() {
                continue;
            }
            let kind = match call.kind {
                ToolKind::Bash => TaskKind::Command,
                ToolKind::Task => TaskKind::Agent,
                _ => TaskKind::Other,
            };
            let mut t = BackgroundTask::running(id.clone(), kind, call.title);
            t.item_id = Some(id);
            t.tracked = tracked;
            changed |= s.tasks.add(t);
        }
        changed |= s.tasks.promote_running_agents_with(tracked);
    } else {
        changed |= s.tasks.stop_in_turn();
    }
    changed.then(|| s.tasks.event())
}

fn tool_kind(k: &str) -> ToolKind {
    match k {
        "read" => ToolKind::Read,
        "edit" | "delete" | "move" => ToolKind::Edit,
        "write" => ToolKind::Write,
        "search" => ToolKind::Search,
        "execute" => ToolKind::Bash,
        "fetch" => ToolKind::Web,
        "task" | "agent" | "subagent" => ToolKind::Task,
        _ => ToolKind::Other,
    }
}

fn is_bare_subagent_name(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "invoke_subagent" | "swarm" | "subagent" | "agent" | "task")
}

fn is_subagent_name(s: &str) -> bool {
    let lower = s.trim().to_ascii_lowercase();
    let head = lower.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-').next().unwrap_or("");
    matches!(head, "invoke_subagent" | "swarm" | "subagent" | "agent" | "task")
}

fn classify_tool(u: &Value, call: &ToolCall) -> ToolKind {
    if call.kind != ToolKind::Other {
        return call.kind;
    }
    let candidates =
        [u["name"].as_str(), u["toolName"].as_str(), u["_meta"]["toolName"].as_str(), Some(call.name.as_str()), Some(call.title.as_str())];
    if candidates.into_iter().flatten().any(is_subagent_name) {
        ToolKind::Task
    } else {
        ToolKind::Other
    }
}

fn subagent_title(call: &ToolCall) -> String {
    if !call.title.trim().is_empty() && !is_bare_subagent_name(&call.title) {
        return first_line(&call.title);
    }
    ["description", "task", "prompt", "name", "subagent_name", "brief"]
        .iter()
        .find_map(|k| call.input[*k].as_str().map(first_line).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| if call.title.trim().is_empty() { "Subagent".into() } else { first_line(&call.title) })
}

fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    match line.char_indices().nth(160) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
    }
}

fn last_line(text: &str) -> Option<String> {
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?.trim();
    Some(match line.char_indices().nth(160) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
    })
}

fn live_progress(u: &Value, existing_title: &str) -> Option<String> {
    let out = output_of(u);
    if let Some(line) = last_line(&out) {
        return Some(line);
    }
    u["title"].as_str().map(first_line).filter(|t| !t.is_empty() && t != existing_title && !is_bare_subagent_name(t))
}

fn parse_subagent_launch(output: &str) -> Option<(String, Option<String>)> {
    let start = output.find('{')?;
    let end = output.rfind('}')?;
    if end <= start {
        return None;
    }
    let v: Value = serde_json::from_str(&output[start..=end]).ok()?;
    let cid = v["conversationId"].as_str().map(str::trim).filter(|s| !s.is_empty())?.to_string();
    let log_path = v["logAbsoluteUri"].as_str().and_then(file_uri_to_path);
    Some((cid, log_path))
}

fn file_uri_to_path(uri: &str) -> Option<String> {
    let raw = uri.strip_prefix("file://")?;
    let decoded = percent_decode(raw);
    let bytes = decoded.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        Some(decoded[1..].to_string())
    } else {
        Some(decoded)
    }
}

fn percent_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn sh_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        format!("\"$HOME\"/{}", sh_quote(rest))
    } else {
        sh_quote(path)
    }
}

fn clean_system_prompt(raw: &str) -> String {
    if let Some(after) = raw.split_once("content=").map(|(_, rest)| rest) {
        let trimmed = after.split("</SYSTEM_MESSAGE>").next().unwrap_or(after).trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    raw.trim().to_string()
}

fn unquote_arg(v: &Value) -> Option<String> {
    let s = v.as_str()?;
    let trimmed = s.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        if let Ok(parsed) = serde_json::from_str::<String>(trimmed) {
            return Some(parsed);
        }
    }
    Some(trimmed.to_string())
}

fn subagent_tool_call(tc: &Value) -> ToolCall {
    let name = tc["name"].as_str().unwrap_or("tool").to_string();
    let args = tc["args"].clone();
    let kind = match name.as_str() {
        "read_file" | "view_file" => ToolKind::Read,
        "edit_file" | "replace_file_content" | "multi_replace_file_content" => ToolKind::Edit,
        "write_file" | "write_to_file" => ToolKind::Write,
        "run_command" => ToolKind::Bash,
        "search_web" => ToolKind::Web,
        "grep_search" | "find_by_name" => ToolKind::Search,
        "invoke_subagent" | "swarm" => ToolKind::Task,
        _ => ToolKind::Other,
    };
    let path = ["Path", "path", "filePath", "file_path"].iter().find_map(|k| unquote_arg(&args[*k])).filter(|s| !s.is_empty());
    let title = path
        .clone()
        .or_else(|| {
            ["CommandLine", "command", "query", "Query", "Message", "Recipient", "Role"]
                .iter()
                .find_map(|k| unquote_arg(&args[*k]))
                .filter(|s| !s.is_empty())
        })
        .map(|s| first_line(&s))
        .unwrap_or_else(|| name.clone());
    ToolCall { kind, name, title, input: args, path }
}

fn read_transcript_lines(location: &Location, path: &str, seen: usize) -> Option<Vec<String>> {
    match location {
        Location::Local => {
            let text = std::fs::read_to_string(path).ok()?;
            Some(text.lines().skip(seen).map(String::from).collect())
        }
        remote => {
            let p = sh_path(path);
            let cmd = format!("if [ -f {p} ]; then tail -n +{} {p}; fi", seen + 1);
            let out = moonfish_transport::run(
                &Spec { location: remote, cwd: "~", program: "sh", args: vec!["-c".into(), cmd], env: &[] },
                None,
                Duration::from_secs(10),
            )
            .ok()?;
            if !out.ok() {
                return None;
            }
            Some(out.stdout.lines().map(String::from).collect())
        }
    }
}

fn watch_antigravity_subagent(
    task_id: String,
    conversation_id: String,
    log_path: Option<String>,
    location: Location,
    state: Arc<Mutex<State>>,
    sink: Sink,
) {
    let path = log_path.unwrap_or_else(|| match &location {
        Location::Local => moonfish_core::paths::home_dir()
            .join(".gemini")
            .join("antigravity-cli")
            .join("brain")
            .join(&conversation_id)
            .join(".system_generated")
            .join("logs")
            .join("transcript.jsonl")
            .to_string_lossy()
            .into_owned(),
        _ => format!("~/.gemini/antigravity-cli/brain/{conversation_id}/.system_generated/logs/transcript.jsonl"),
    });
    thread::spawn(move || {
        sink(ChatEvent::SubagentEvent {
            task_id: task_id.clone(),
            event: Box::new(ChatEvent::TurnStarted { turn_id: format!("sub-{conversation_id}") }),
        });
        let mut seen = 0usize;
        let mut pending_tools: VecDeque<String> = VecDeque::new();
        let mut missing_ticks = 0u32;
        loop {
            {
                let st = state.lock().unwrap();
                if !st.alive || st.direct_subagents.contains(&task_id) {
                    break;
                }
            }
            let Some(lines) = read_transcript_lines(&location, &path, seen) else {
                missing_ticks += 1;
                if missing_ticks > 60 {
                    break;
                }
                thread::sleep(Duration::from_millis(500));
                continue;
            };
            missing_ticks = 0;
            let mut finished: Option<(TaskStatus, Option<String>)> = None;
            for line in lines {
                let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                    break;
                };
                seen += 1;
                let step = v["step_index"].as_u64().unwrap_or(seen as u64);
                let kind = v["type"].as_str().unwrap_or("");
                let source = v["source"].as_str().unwrap_or("");
                let status = v["status"].as_str().unwrap_or("");
                match kind {
                    "SYSTEM_MESSAGE" if step == 0 => {
                        if let Some(raw) = v["content"].as_str().filter(|s| !s.trim().is_empty()) {
                            let text = clean_system_prompt(raw);
                            sink(ChatEvent::SubagentEvent {
                                task_id: task_id.clone(),
                                event: Box::new(ChatEvent::UserMessage {
                                    id: format!("s{step}"),
                                    text,
                                    images: vec![],
                                    files: vec![],
                                }),
                            });
                        }
                    }
                    "USER_INPUT" => {
                        if let Some(raw) = v["content"].as_str().filter(|s| !s.trim().is_empty()) {
                            sink(ChatEvent::SubagentEvent {
                                task_id: task_id.clone(),
                                event: Box::new(ChatEvent::UserMessage {
                                    id: format!("s{step}"),
                                    text: raw.trim().to_string(),
                                    images: vec![],
                                    files: vec![],
                                }),
                            });
                        }
                    }
                    "PLANNER_RESPONSE" => {
                        if let Some(thinking) = v["thinking"].as_str().filter(|s| !s.trim().is_empty()) {
                            sink(ChatEvent::SubagentEvent {
                                task_id: task_id.clone(),
                                event: Box::new(ChatEvent::Reasoning { item_id: format!("s{step}:r"), text: thinking.trim().to_string() }),
                            });
                        }
                        let calls = v["tool_calls"].as_array();
                        if let Some(calls) = calls.filter(|a| !a.is_empty()) {
                            let mut last_detail = None;
                            for (j, tc) in calls.iter().enumerate() {
                                let item_id = format!("s{step}:c{j}");
                                let tool = subagent_tool_call(tc);
                                last_detail = Some(format!("{} {}", tool.name, tool.title));
                                pending_tools.push_back(item_id.clone());
                                sink(ChatEvent::SubagentEvent {
                                    task_id: task_id.clone(),
                                    event: Box::new(ChatEvent::ToolStarted { item_id, tool }),
                                });
                            }
                            if let Some(detail) = last_detail {
                                let mut st = state.lock().unwrap();
                                if st.tasks.progress(&task_id, None, Some(&first_line(&detail))) {
                                    let ev = st.tasks.event();
                                    drop(st);
                                    sink(ev);
                                }
                            }
                        }
                        let content = v["content"].as_str().map(str::trim).filter(|s| !s.is_empty());
                        if let Some(text) = content {
                            sink(ChatEvent::SubagentEvent {
                                task_id: task_id.clone(),
                                event: Box::new(ChatEvent::Text { item_id: format!("s{step}:t"), text: text.to_string() }),
                            });
                        }
                        let no_calls = calls.is_none_or(|a| a.is_empty());
                        if no_calls && matches!(status, "DONE" | "ERROR" | "CANCELLED") {
                            let task_status = match status {
                                "ERROR" => TaskStatus::Failed,
                                "CANCELLED" => TaskStatus::Stopped,
                                _ => TaskStatus::Completed,
                            };
                            finished = Some((task_status, content.map(first_line)));
                        }
                    }
                    "GENERIC" if source == "MODEL" => {
                        if let Some(item_id) = pending_tools.pop_front() {
                            let output = clip(v["content"].as_str().unwrap_or("").to_string());
                            let ok = status != "ERROR";
                            sink(ChatEvent::SubagentEvent {
                                task_id: task_id.clone(),
                                event: Box::new(ChatEvent::ToolFinished { item_id, ok, output, diff: None }),
                            });
                        }
                    }
                    _ => {}
                }
            }
            if let Some((status, detail)) = finished {
                sink(ChatEvent::SubagentEvent {
                    task_id: task_id.clone(),
                    event: Box::new(ChatEvent::TurnFinished {
                        turn_id: format!("sub-{conversation_id}"),
                        usage: None,
                        cost_usd: None,
                        duration_ms: None,
                        interrupted: status == TaskStatus::Stopped,
                    }),
                });
                let ev = {
                    let mut st = state.lock().unwrap();
                    if st.direct_subagents.contains(&task_id) {
                        None
                    } else {
                        st.tasks.finish(&task_id, status, detail).is_some().then(|| st.tasks.event())
                    }
                };
                if let Some(ev) = ev {
                    sink(ev);
                }
                break;
            }
            thread::sleep(Duration::from_millis(600));
        }
    });
}

fn input_title(input: &Value) -> String {
    command_of(input)
        .or_else(|| ["filePath", "file_path", "path", "pattern", "query", "url", "description"].iter().find_map(|k| input[*k].as_str()))
        .unwrap_or("")
        .to_string()
}

/// A shell tool's command line from its raw input.
fn command_of(input: &Value) -> Option<&str> {
    ["command", "CommandLine", "commandLine", "command_line"].iter().find_map(|k| input[*k].as_str())
}

/// Antigravity's question tool calls (`interaction_…`).
fn is_question(tool_call_id: &str) -> bool {
    tool_call_id.starts_with("interaction_")
}

/// The answer to a choice question: the option whose name was picked (a picked option is the only
/// answer Antigravity takes), or cancelled.
fn choice_reply(options: &[Value], answers: &std::collections::BTreeMap<String, Vec<String>>) -> Value {
    let picked = answers.values().flatten().next().map(|a| a.trim_end_matches(" (Recommended)").to_string());
    match picked.and_then(|p| options.iter().find(|o| o["name"].as_str() == Some(p.as_str()))).and_then(|o| o["optionId"].as_str()) {
        Some(id) => json!({"outcome": {"outcome": "selected", "optionId": id}}),
        None => json!({"outcome": {"outcome": "cancelled"}}),
    }
}

/// A tool's output from its text content or `rawOutput`.
fn output_of(u: &Value) -> String {
    let mut parts: Vec<String> = u["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["type"] == "content")
        .filter_map(|c| c["content"]["text"].as_str().map(String::from))
        .collect();
    if parts.is_empty() {
        let raw = &u["rawOutput"];
        if let Some(t) = raw["metadata"]["output"]
            .as_str()
            .or(raw["output"].as_str())
            .or(raw["combinedOutput"].as_str())
            .or(raw["combined_output"].as_str())
            .or(raw.as_str())
        {
            parts.push(t.to_string());
        }
    }
    parts.join("\n")
}

fn diffs_of(content: &Value) -> Option<FileDiff> {
    let diffs: Vec<FileDiff> = content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["type"] == "diff")
        .filter_map(|c| {
            Some(FileDiff::replace(c["path"].as_str()?, c["oldText"].as_str().unwrap_or(""), c["newText"].as_str().unwrap_or("")))
        })
        .collect();
    match diffs.len() {
        0 => None,
        1 => diffs.into_iter().next(),
        n => {
            let path = format!("{} (+{} more)", diffs[0].path, n - 1);
            Some(FileDiff::from_patch(path, diffs.iter().map(|d| format!("@@ {} @@\n{}", d.path, d.patch)).collect::<String>()))
        }
    }
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

fn usage_of(u: &Value) -> Option<Usage> {
    if u.is_null() {
        return None;
    }
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    Some(Usage {
        input_tokens: n("inputTokens"),
        output_tokens: n("outputTokens"),
        cache_read_tokens: n("cachedReadTokens"),
        cache_write_tokens: n("cachedWriteTokens").max(n("cacheCreationTokens")),
    })
}

/// The option to answer with: Allow → allow once, Always → allow always (else once), Deny →
/// reject once (else always). No matching option cancels.
fn permission_outcome(options: &[Value], decision: Decision) -> Value {
    let find = |kinds: &[&str]| {
        kinds.iter().find_map(|k| options.iter().find(|o| o["kind"] == *k)).and_then(|o| o["optionId"].as_str().map(String::from))
    };
    let id = match decision {
        Decision::Allow => find(&["allow_once", "allow_always"]),
        Decision::AllowAlways => find(&["allow_always", "allow_once"]),
        Decision::Deny => find(&["reject_once", "reject_always"]),
    };
    match id {
        Some(id) => json!({"outcome": {"outcome": "selected", "optionId": id}}),
        None => json!({"outcome": {"outcome": "cancelled"}}),
    }
}

/// A select config option's values (flat or grouped).
fn select_values(options: &Value) -> Vec<ModelOption> {
    let mut out = Vec::new();
    for o in options.as_array().into_iter().flatten() {
        if let Some(group) = o["options"].as_array() {
            out.extend(group.iter().filter_map(model_option));
        } else if let Some(m) = model_option(o) {
            out.push(m);
        }
    }
    out
}

fn model_option(o: &Value) -> Option<ModelOption> {
    let id = o["value"].as_str().or(o["modelId"].as_str())?.to_string();
    Some(ModelOption {
        name: o["name"].as_str().unwrap_or(&id).to_string(),
        description: o["description"].as_str().unwrap_or("").to_string(),
        id,
        ..Default::default()
    })
}

/// The model choices in a session (config option with category `model`, or unstable `models`):
/// (options, current, the config option id when there is one).
fn models_from(v: &Value) -> (Vec<ModelOption>, Option<String>, Option<String>) {
    if let Some(opt) = v["configOptions"].as_array().and_then(|a| a.iter().find(|o| o["category"] == "model" || o["id"] == "model")) {
        return (select_values(&opt["options"]), opt["currentValue"].as_str().map(String::from), opt["id"].as_str().map(String::from));
    }
    let m = &v["models"];
    let list = m["availableModels"].as_array().into_iter().flatten().filter_map(model_option).collect();
    (list, m["currentModelId"].as_str().map(String::from), None)
}

/// The session's reasoning effort option and its values.
fn read_effort(st: &mut State, v: &Value) {
    let Some(opts) = v["configOptions"].as_array() else { return };
    match opts.iter().find(|o| o["category"] == "thought_level" || o["id"] == "effort") {
        Some(o) => {
            st.effort_option = o["id"].as_str().map(String::from);
            st.efforts = select_values(&o["options"]).into_iter().map(|m| m.id).collect();
        }
        None => {
            st.effort_option = None;
            st.efforts.clear();
        }
    }
}

/// Gives the current model the session's effort levels (`default` is "none picked", not one).
fn add_efforts(models: &mut [ModelOption], current: Option<&str>, efforts: &[String]) {
    if let Some(m) = models.iter_mut().find(|m| Some(m.id.as_str()) == current) {
        if m.efforts.is_empty() {
            m.efforts = efforts.iter().filter(|e| *e != "default").cloned().collect();
        }
    }
}

pub(crate) fn input_summary(input: &Value) -> Option<String> {
    const MAX_KEYS: usize = 8;
    const MAX_VAL: usize = 160;
    const MAX_TOTAL: usize = 600;
    fn preview(s: &str, max: usize) -> String {
        let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
        match flat.char_indices().nth(max) {
            Some((i, _)) => format!("{}…", &flat[..i]),
            None => flat,
        }
    }
    match input {
        Value::Null => None,
        Value::Object(map) => {
            let entries: Vec<String> = map
                .iter()
                .filter_map(|(k, v)| {
                    let val = match v {
                        Value::Null => return None,
                        Value::String(s) => {
                            let p = preview(s, MAX_VAL);
                            if p.is_empty() {
                                return None;
                            }
                            p
                        }
                        Value::Bool(b) => b.to_string(),
                        Value::Number(n) => n.to_string(),
                        _ => preview(&v.to_string(), MAX_VAL),
                    };
                    Some(format!("{k}: {val}"))
                })
                .collect();
            if entries.is_empty() {
                return None;
            }
            let mut lines = Vec::new();
            let mut total = 0usize;
            for line in &entries {
                if lines.len() >= MAX_KEYS || (total > 0 && total + line.len() > MAX_TOTAL) {
                    break;
                }
                total += line.len() + 1;
                lines.push(line.clone());
            }
            if lines.len() < entries.len() {
                lines.push(format!("… (+{} more)", entries.len() - lines.len()));
            }
            Some(lines.join("\n"))
        }
        Value::String(s) => {
            let p = preview(s, MAX_TOTAL);
            (!p.is_empty()).then_some(p)
        }
        Value::Array(a) if a.is_empty() => None,
        other => Some(preview(&other.to_string(), MAX_TOTAL)),
    }
}

/// An elicitation form → question cards, and how to read the answers back.
pub(crate) fn form_questions(p: &Value) -> (Vec<Question>, Vec<Field>) {
    let schema = &p["requestedSchema"];
    let Some(props) = schema["properties"].as_object() else { return (vec![], vec![]) };
    let message = p["message"].as_str().unwrap_or("").to_string();
    let others: Vec<&String> = props.keys().filter(|k| k.ends_with("__other") || k.ends_with("_custom")).collect();
    let mut questions = Vec::new();
    let mut fields = Vec::new();
    for (key, prop) in props.iter().filter(|(k, _)| !others.contains(k)) {
        let other_key =
            others.iter().find(|o| o.as_str() == format!("{key}__other") || o.as_str() == format!("{key}_custom")).map(|s| s.to_string());
        let default = prop["default"].as_str().map(String::from);
        // (value, label, description): a choice is sent back by its `const`, shown by its title.
        let choices = |p: &Value| -> Vec<(String, String, String)> {
            if let Some(e) = p["enum"].as_array() {
                return e.iter().filter_map(|x| x.as_str().map(|s| (s.to_string(), s.to_string(), String::new()))).collect();
            }
            p["oneOf"]
                .as_array()
                .or(p["anyOf"].as_array())
                .into_iter()
                .flatten()
                .filter_map(|x| {
                    let value = x["const"].as_str()?.to_string();
                    let label = x["title"].as_str().unwrap_or(&value).to_string();
                    let description = x["description"].as_str().filter(|d| *d != label).unwrap_or("").to_string();
                    Some((value, label, description))
                })
                .collect()
        };
        let (kind, offered) = match prop["type"].as_str() {
            Some("boolean") => {
                (FieldKind::Bool, vec![("true".into(), "Yes".into(), String::new()), ("false".into(), "No".into(), String::new())])
            }
            Some("array") => (FieldKind::Multi, choices(&prop["items"])),
            _ => {
                let c = choices(prop);
                (if c.is_empty() { FieldKind::Text } else { FieldKind::Choice }, c)
            }
        };
        let options = offered
            .iter()
            .map(|(v, t, d)| QuestionOption {
                label: if Some(v) == default.as_ref() { format!("{t} (Recommended)") } else { t.clone() },
                description: d.clone(),
            })
            .collect();
        let title = prop["title"].as_str().or(prop["description"].as_str()).unwrap_or(key).to_string();
        let ask = prop["description"]
            .as_str()
            .filter(|d| prop["title"].is_string() && *d != title)
            .map(|d| format!("{title}: {d}"))
            .unwrap_or(title);
        questions.push(Question {
            id: key.clone(),
            header: if questions.is_empty() && !message.is_empty() { String::new() } else { key.clone() },
            question: if questions.is_empty() && !message.is_empty() { format!("{message}\n{ask}") } else { ask },
            options,
            multi: kind == FieldKind::Multi,
            other: kind == FieldKind::Text || other_key.is_some(),
            secret: prop["format"] == "password",
        });
        fields.push(Field { key: key.clone(), kind, other_key, choices: offered.into_iter().map(|(v, t, _)| (v, t)).collect() });
    }
    (questions, fields)
}

/// The answers → `{action, content}`. A picked label goes back as its choice's value; typed
/// text goes to the field's `_custom` / `__other` key when it has one. Nothing answered cancels.
pub(crate) fn elicitation_reply(fields: &[Field], answers: &std::collections::BTreeMap<String, Vec<String>>) -> Value {
    if answers.values().all(|a| a.is_empty()) {
        return json!({"action": "cancel"});
    }
    let strip = |s: &str| s.trim_end_matches(" (Recommended)").to_string();
    let mut content = serde_json::Map::new();
    for f in fields {
        let Some(a) = answers.get(&f.key).filter(|a| !a.is_empty()) else { continue };
        let mut picked = Vec::new();
        let mut typed = Vec::new();
        for answer in a {
            match f.choices.iter().find(|(_, label)| *label == strip(answer)) {
                Some((value, _)) => picked.push(value.clone()),
                None => typed.push(answer.clone()),
            }
        }
        let typed = Some(typed.join(", ")).filter(|t| !t.trim().is_empty());
        match f.kind {
            FieldKind::Bool => {
                if let Some(v) = picked.first() {
                    content.insert(f.key.clone(), json!(v == "true"));
                }
            }
            FieldKind::Multi => {
                content.insert(f.key.clone(), json!(picked));
            }
            FieldKind::Choice => {
                if let Some(v) = picked.first() {
                    content.insert(f.key.clone(), json!(v));
                }
            }
            FieldKind::Text => {
                if let Some(t) = &typed {
                    content.insert(f.key.clone(), json!(t));
                }
                continue;
            }
        }
        if let Some(t) = typed {
            match &f.other_key {
                Some(k) => content.insert(k.clone(), json!(t)),
                // No field for it: the typed answer stands in for a choice.
                None => content.insert(f.key.clone(), json!(t)),
            };
        }
    }
    json!({"action": "accept", "content": content})
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonfish_core::settings::AgentKind;

    fn run(lines: &[Value], permission: PermissionMode) -> (Vec<ChatEvent>, Arc<Mutex<State>>) {
        run_as(None, lines, permission)
    }

    fn run_as(kind: Option<AgentKind>, lines: &[Value], permission: PermissionMode) -> (Vec<ChatEvent>, Arc<Mutex<State>>) {
        let out = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let sink: Sink = Arc::new(move |e| o.lock().unwrap().push(e));
        let state =
            Arc::new(Mutex::new(State { alive: true, turn_id: Some("turn-1".into()), stretch: 1, permission, kind, ..Default::default() }));
        let mut tr = Translator { sink, state: state.clone(), rpc: Rpc::new(None, "ACP") };
        for l in lines {
            tr.handle(l);
        }
        let v = out.lock().unwrap().clone();
        (v, state)
    }

    fn upd(u: Value) -> Value {
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s", "update": u}})
    }

    #[test]
    fn explains_how_to_sign_in() {
        assert!(is_auth_error("Authentication required: no auth method id provided"));
        assert!(is_auth_error("provider returned 401"));
        assert!(!is_auth_error("model not found"));
    }

    #[test]
    fn reads_available_commands() {
        let (ev, state) = run(
            &[upd(json!({"sessionUpdate": "available_commands_update", "availableCommands": [
                {"name": "init", "description": "create/update AGENTS.md"},
                {"name": "review", "description": "review changes", "input": {"hint": "commit|branch|pr"}}
            ]}))],
            PermissionMode::Ask,
        );
        let ChatEvent::Commands { commands } = &ev[0] else { panic!("{ev:?}") };
        assert_eq!(commands.len(), 2);
        assert_eq!((commands[1].name.as_str(), commands[1].hint.as_str()), ("review", "commit|branch|pr"));
        // While a reopened session replays, history is skipped but commands still arrive.
        state.lock().unwrap().loading = true;
        let out = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let mut tr = Translator { sink: Arc::new(move |e| o.lock().unwrap().push(e)), state, rpc: Rpc::new(None, "ACP") };
        tr.handle(&upd(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "old"}})));
        tr.handle(&upd(json!({"sessionUpdate": "available_commands_update", "availableCommands": [{"name": "init", "description": ""}]})));
        assert!(matches!(&out.lock().unwrap()[..], [ChatEvent::Commands { .. }]));
    }

    #[test]
    fn translates_updates() {
        let (ev, _) = run(
            &[
                upd(json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hmm"}})),
                upd(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Let me look."}})),
                upd(
                    json!({"sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Read src/cart.js", "kind": "read", "status": "pending", "locations": [{"path": "src/cart.js"}]}),
                ),
                upd(
                    json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed", "content": [{"type": "content", "content": {"type": "text", "text": "export function subtotal"}}]}),
                ),
                upd(
                    json!({"sessionUpdate": "tool_call", "toolCallId": "c2", "title": "Edit src/cart.js", "kind": "edit", "status": "in_progress"}),
                ),
                upd(
                    json!({"sessionUpdate": "tool_call_update", "toolCallId": "c2", "status": "completed", "content": [{"type": "diff", "path": "src/cart.js", "oldText": "i = 1", "newText": "i = 0"}]}),
                ),
                upd(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Fixed."}})),
                upd(json!({"sessionUpdate": "plan", "entries": [{"content": "Fix loop", "priority": "high", "status": "completed"}]})),
                upd(json!({"sessionUpdate": "usage_update", "used": 12000, "size": 200000, "cost": {"amount": 0.02, "currency": "USD"}})),
            ],
            PermissionMode::Ask,
        );
        assert!(matches!(&ev[0], ChatEvent::ReasoningDelta { text, .. } if text == "hmm"));
        let a1 = match &ev[1] {
            ChatEvent::TextDelta { item_id, .. } => item_id.clone(),
            e => panic!("{e:?}"),
        };
        assert!(
            matches!(&ev[2], ChatEvent::ToolStarted { tool, .. } if tool.kind == ToolKind::Read && tool.path.as_deref() == Some("src/cart.js"))
        );
        assert!(matches!(&ev[3], ChatEvent::ToolFinished { ok: true, output, .. } if output.contains("subtotal")));
        assert!(matches!(&ev[5], ChatEvent::ToolFinished { diff: Some(d), .. } if d.added == 1 && d.removed == 1));
        match &ev[6] {
            ChatEvent::TextDelta { item_id, .. } => assert_ne!(item_id, &a1, "text after a tool is a new message"),
            e => panic!("{e:?}"),
        }
        assert!(matches!(&ev[7], ChatEvent::Plan { items } if items[0].status == PlanStatus::Completed));
        assert!(matches!(&ev[8], ChatEvent::Context { used_tokens: 12000, window_tokens: 200000, .. }));
    }

    /// Antigravity's question, as captured from 1.x: a tool call, then a permission request whose
    /// options are the answers.
    #[test]
    fn antigravity_questions_become_cards_even_with_full_access() {
        let call = upd(
            json!({"toolCallId": "interaction_28ffdbf7", "title": "Which color do you prefer?", "status": "pending", "rawInput": {}, "sessionUpdate": "tool_call"}),
        );
        let req = json!({"jsonrpc": "2.0", "id": 2, "method": "session/request_permission", "params": {"sessionId": "s",
            "toolCall": {"toolCallId": "interaction_28ffdbf7", "status": "pending", "title": "Which color do you prefer?", "rawInput": {}},
            "options": [{"optionId": "1", "name": "red", "kind": "allow_once"}, {"optionId": "2", "name": "blue", "kind": "allow_once"}]}});
        let done = upd(
            json!({"toolCallId": "interaction_28ffdbf7", "status": "completed", "rawOutput": "Response received", "sessionUpdate": "tool_call_update"}),
        );
        let (ev, state) = run_as(Some(AgentKind::Antigravity), &[call, req, done], PermissionMode::FullAccess);
        // Only the card: no tool row for the question, and full access doesn't answer it.
        match &ev[..] {
            [ChatEvent::Question { request }] => {
                assert_eq!(request.request_id, "acpq-2");
                assert_eq!(request.questions[0].question, "Which color do you prefer?");
                let labels: Vec<&str> = request.questions[0].options.iter().map(|o| o.label.as_str()).collect();
                assert_eq!(labels, ["red", "blue"]);
                assert!(!request.questions[0].other);
            }
            e => panic!("{e:?}"),
        }
        let (_, options) = state.lock().unwrap().choices.get("acpq-2").cloned().unwrap();
        let answer = |a: &str| choice_reply(&options, &[("answer".to_string(), vec![a.to_string()])].into_iter().collect());
        assert_eq!(answer("blue"), json!({"outcome": {"outcome": "selected", "optionId": "2"}}));
        assert_eq!(answer("green")["outcome"]["outcome"], "cancelled");
    }

    #[test]
    fn antigravity_commands_and_shell_tools() {
        let cmds = upd(
            json!({"sessionUpdate": "available_commands_update", "availableCommands": [{"name": "goal", "description": "Run until done."}, {"name": "logout", "description": "Sign out."}]}),
        );
        let tool = upd(
            json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "kind": "execute", "status": "in_progress", "rawInput": {"CommandLine": "cargo test", "Cwd": "/w"}}),
        );
        let end = upd(
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed", "rawOutput": {"combinedOutput": "ok: 9 passed", "exitCode": 0}}),
        );
        let (ev, _) = run_as(Some(AgentKind::Antigravity), &[cmds, tool, end], PermissionMode::Ask);
        match &ev[0] {
            ChatEvent::Commands { commands } => assert_eq!(commands.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["goal"]),
            e => panic!("{e:?}"),
        }
        assert!(matches!(&ev[1], ChatEvent::ToolStarted { tool, .. } if tool.title == "cargo test" && tool.kind == ToolKind::Bash));
        assert!(matches!(&ev[2], ChatEvent::ToolFinished { ok: true, output, .. } if output == "ok: 9 passed"));
        // Asking to run it shows the command.
        let req = json!({"jsonrpc": "2.0", "id": 9, "method": "session/request_permission", "params": {"sessionId": "s",
            "toolCall": {"toolCallId": "t2", "kind": "execute", "title": "rm -rf build", "rawInput": {"CommandLine": "rm -rf build"}},
            "options": [{"optionId": "a", "kind": "allow_once", "name": "Allow"},
                        {"optionId": "b", "kind": "allow_always", "name": "Always", "_meta": {"agy.security.warning": {"title": "Destructive", "message": "Deletes files."}}}]}});
        let (ev, _) = run_as(Some(AgentKind::Antigravity), &[req], PermissionMode::Ask);
        match &ev[0] {
            ChatEvent::Approval { request } => {
                assert_eq!(request.kind, ApprovalKind::Command);
                assert_eq!(request.detail.as_deref(), Some("rm -rf build\n\nWarning. Destructive: Deletes files."));
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn antigravity_modes_and_model_efforts() {
        assert_eq!(acp_wire_mode(Some(AgentKind::Antigravity), PermissionMode::Ask), "default");
        assert_eq!(acp_wire_mode(Some(AgentKind::Antigravity), PermissionMode::AcceptEdits), "accept-edits");
        assert_eq!(acp_wire_mode(Some(AgentKind::Antigravity), PermissionMode::FullAccess), "default");
        assert_eq!(acp_wire_mode(Some(AgentKind::Antigravity), PermissionMode::Plan), "plan");
        // `session/new` from agy 1.5.3: base models in the config option, efforts in the ids.
        let session = json!({
            "models": {"availableModels": [
                {"modelId": "gemini-4-argon-medium"}, {"modelId": "gemini-4-argon-high"}, {"modelId": "gemini-4-argon-low"},
                {"modelId": "claude-opus-5-5-low"}, {"modelId": "claude-opus-5-5-high"}, {"modelId": "gpt-oss-120b-medium"}],
                "currentModelId": "gemini-4-argon-medium"},
            "configOptions": [
                {"id": "model", "category": "model", "currentValue": "gemini-4-argon", "options": [
                    {"value": "gemini-4-argon", "name": "Gemini 4 Argon"}, {"value": "claude-opus-5-5", "name": "Claude Opus 5.5"},
                    {"value": "gpt-oss-120b-medium", "name": "GPT-OSS 120B (Medium)"}]},
                {"id": "thought_level", "category": "thought_level", "currentValue": "medium", "options": [{"value": "low"}, {"value": "medium"}, {"value": "high"}]}]
        });
        let (mut models, current, option) = models_from(&session);
        assert_eq!((current.as_deref(), option.as_deref()), (Some("gemini-4-argon"), Some("model")));
        variant_efforts(&mut models, &session["models"]);
        assert_eq!(models[0].efforts, ["low", "medium", "high"]);
        assert_eq!(models[1].efforts, ["low", "high"]);
        assert!(models[2].efforts.is_empty());
    }

    #[test]
    fn asks_permission_and_answers_with_options() {
        let req = json!({"jsonrpc": "2.0", "id": 5, "method": "session/request_permission", "params": {"sessionId": "s",
            "toolCall": {"toolCallId": "c3", "title": "npm test", "kind": "execute", "rawInput": {"command": "npm test"}},
            "options": [{"optionId": "once", "name": "Allow", "kind": "allow_once"}, {"optionId": "always", "name": "Always", "kind": "allow_always"}, {"optionId": "reject", "name": "Reject", "kind": "reject_once"}]}});
        let (ev, state) = run(std::slice::from_ref(&req), PermissionMode::Ask);
        match &ev[0] {
            ChatEvent::Approval { request } => {
                assert_eq!(request.kind, ApprovalKind::Command);
                assert_eq!(request.detail.as_deref(), Some("npm test"));
                assert!(request.can_remember);
            }
            e => panic!("{e:?}"),
        }
        let (_, options) = state.lock().unwrap().pending.get("acp-5").cloned().unwrap();
        assert_eq!(permission_outcome(&options, Decision::AllowAlways)["outcome"]["optionId"], "always");
        assert_eq!(permission_outcome(&options, Decision::Deny)["outcome"]["optionId"], "reject");
        assert_eq!(permission_outcome(&[], Decision::Allow)["outcome"]["outcome"], "cancelled");
        let (ev, _) = run(std::slice::from_ref(&req), PermissionMode::FullAccess);
        assert!(ev.is_empty(), "full access answers by itself");
        // Accept edits answers edits by itself, and still asks before a command.
        let (ev, _) = run(std::slice::from_ref(&req), PermissionMode::AcceptEdits);
        assert!(matches!(&ev[..], [ChatEvent::Approval { .. }]));
        let mut edit = req.clone();
        edit["params"]["toolCall"]["kind"] = json!("edit");
        let (ev, _) = run(&[edit], PermissionMode::AcceptEdits);
        assert!(ev.is_empty(), "accept edits answers edits");
    }

    #[test]
    fn elicitation_forms_become_questions() {
        let req = json!({"jsonrpc": "2.0", "id": 8, "method": "elicitation/create", "params": {"mode": "form", "message": "Pick a setup",
            "requestedSchema": {"type": "object", "properties": {
                "db": {"type": "string", "title": "Database", "enum": ["postgres", "sqlite"], "default": "sqlite"},
                "db__other": {"type": "string"},
                "extras": {"type": "array", "items": {"enum": ["auth", "jobs"]}},
                "tests": {"type": "boolean", "title": "Add tests?"}
            }, "required": ["db"]}}});
        let (ev, state) = run(&[req], PermissionMode::Ask);
        let q = match &ev[0] {
            ChatEvent::Question { request } => request.clone(),
            e => panic!("{e:?}"),
        };
        assert_eq!(q.questions.len(), 3);
        let db = q.questions.iter().find(|x| x.id == "db").unwrap();
        assert!(db.other);
        assert!(db.options.iter().any(|o| o.label == "sqlite (Recommended)"));
        assert!(q.questions.iter().find(|x| x.id == "extras").unwrap().multi);
        let (_, fields) = state.lock().unwrap().questions.get("acpq-8").cloned().unwrap();
        let mut a = std::collections::BTreeMap::new();
        a.insert("db".to_string(), vec!["sqlite (Recommended)".to_string()]);
        a.insert("extras".to_string(), vec!["auth".to_string(), "jobs".to_string()]);
        a.insert("tests".to_string(), vec!["Yes".to_string()]);
        let r = elicitation_reply(&fields, &a);
        assert_eq!(r["action"], "accept");
        assert_eq!(r["content"]["db"], "sqlite");
        assert_eq!(r["content"]["extras"], json!(["auth", "jobs"]));
        assert_eq!(r["content"]["tests"], true);
        assert_eq!(elicitation_reply(&fields, &Default::default())["action"], "cancel");
    }

    /// A choice uses its schema value, and typed text goes in the companion field.
    #[test]
    fn answers_forms_with_schema_values() {
        let ask = |props: Value| {
            let req = json!({"jsonrpc": "2.0", "id": 9, "method": "elicitation/create", "params": {"mode": "form", "message": "Questions",
                "requestedSchema": {"type": "object", "properties": props}}});
            let (ev, state) = run(&[req], PermissionMode::Ask);
            let q = match &ev[0] {
                ChatEvent::Question { request } => request.clone(),
                e => panic!("{e:?}"),
            };
            let (_, fields) = state.lock().unwrap().questions.get("acpq-9").cloned().unwrap();
            (q, fields)
        };
        let answer = |fields: &[Field], key: &str, a: &[&str]| {
            let mut m = std::collections::BTreeMap::new();
            m.insert(key.to_string(), a.iter().map(|s| s.to_string()).collect());
            elicitation_reply(fields, &m)["content"].clone()
        };

        let (q, f) = ask(json!({"choice": {"type": "string", "description": "Allow web search?", "oneOf": [
            {"const": "allow", "title": "Allow search"}, {"const": "disable", "title": "Disable web search"}]}}));
        assert_eq!(q.questions[0].options[0].label, "Allow search");
        assert_eq!(answer(&f, "choice", &["Disable web search"]), json!({"choice": "disable"}));

        let storage = json!({
            "q0": {"type": "string", "title": "Storage", "description": "Which storage?", "oneOf": [
                {"const": "SQLite", "title": "SQLite", "description": "SQLite"}, {"const": "Postgres", "title": "Postgres", "description": "Small and SQL"}]},
            "q0_custom": {"type": "string", "title": "Storage (other)"}});
        let (q, f) = ask(storage);
        assert_eq!(q.questions.len(), 1, "the _custom field is the question's typed answer, not a question");
        assert!(q.questions[0].other);
        assert_eq!(q.questions[0].options[0].description, "", "a description repeating the title is left out");
        assert_eq!(q.questions[0].options[1].description, "Small and SQL");
        assert_eq!(answer(&f, "q0", &["Postgres"]), json!({"q0": "Postgres"}));
        assert_eq!(answer(&f, "q0", &["DuckDB"]), json!({"q0_custom": "DuckDB"}));
        assert_eq!(answer(&f, "q0", &["SQLite", "and a cache"]), json!({"q0": "SQLite", "q0_custom": "and a cache"}));

        let (_, f) = ask(json!({
            "q1": {"type": "array", "title": "Features", "items": {"anyOf": [{"const": "Search", "title": "Search"}, {"const": "Sync", "title": "Sync"}]}},
            "q1_custom": {"type": "string"}}));
        assert_eq!(answer(&f, "q1", &["Search", "Sync", "Offline"]), json!({"q1": ["Search", "Sync"], "q1_custom": "Offline"}));

        // No choices offered: the typed answer is the answer.
        let (q, f) = ask(json!({"q0": {"type": "string", "title": "Name", "oneOf": []}, "q0_custom": {"type": "string"}}));
        assert!(q.questions[0].options.is_empty() && q.questions[0].other);
        assert_eq!(answer(&f, "q0", &["moonfish"]), json!({"q0": "moonfish"}));
    }

    /// A reopened session's replay says what it cost so far; the next turn is charged only its own.
    #[test]
    fn cost_after_a_reopen_counts_only_new_turns() {
        let (_, state) = run(
            &[upd(json!({"sessionUpdate": "usage_update", "used": 10, "size": 100, "cost": {"amount": 1.5, "currency": "USD"}}))],
            PermissionMode::Ask,
        );
        assert_eq!(state.lock().unwrap().cost, Some(1.5));
        // Unknown before the turn: no share is claimed.
        let st = State::default();
        assert_eq!(st.cost, None);
    }

    #[test]
    fn compact_summaries_are_not_shown() {
        let state =
            Arc::new(Mutex::new(State { alive: true, turn_id: Some("t".into()), stretch: 1, compacting: true, ..Default::default() }));
        let out = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let mut tr = Translator { sink: Arc::new(move |e| o.lock().unwrap().push(e)), state, rpc: Rpc::new(None, "ACP") };
        tr.handle(&upd(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Summary of the chat…"}})));
        tr.handle(&upd(json!({"sessionUpdate": "usage_update", "used": 900, "size": 200000})));
        let ev = out.lock().unwrap().clone();
        assert!(!ev.iter().any(|e| matches!(e, ChatEvent::TextDelta { .. })));
        assert!(ev.iter().any(|e| matches!(e, ChatEvent::Context { used_tokens: 900, .. })));
    }

    #[test]
    fn reads_models_from_config_options() {
        let s = json!({"sessionId": "s", "configOptions": [
            {"id": "model", "category": "model", "type": "select", "currentValue": "anthropic/claude-sonnet-5",
             "options": [{"group": "anthropic", "name": "Anthropic", "options": [{"value": "anthropic/claude-sonnet-5", "name": "Claude Sonnet 5"}]},
                         {"group": "openai", "name": "OpenAI", "options": [{"value": "openai/gpt-6", "name": "GPT-6"}]}]},
            {"id": "mode", "category": "mode", "type": "select", "currentValue": "build", "options": [{"value": "build", "name": "Build"}, {"value": "plan", "name": "Plan"}]}
        ]});
        let (models, current, option) = models_from(&s);
        assert_eq!(models.len(), 2);
        assert_eq!(current.as_deref(), Some("anthropic/claude-sonnet-5"));
        assert_eq!(option.as_deref(), Some("model"));
        let (m2, c2, o2) = models_from(&json!({"models": {"availableModels": [{"modelId": "x", "name": "X"}], "currentModelId": "x"}}));
        assert_eq!((m2.len(), c2.as_deref(), o2), (1, Some("x"), None));
    }

    #[test]
    fn reads_the_effort_option() {
        let session = json!({"configOptions": [
            {"id": "model", "category": "model", "currentValue": "provider/standard", "options": [{"value": "provider/standard", "name": "Standard"}, {"value": "provider/advanced", "name": "Advanced"}]},
            {"id": "effort", "category": "thought_level", "currentValue": "low", "options": [{"value": "low", "name": "Low"}, {"value": "max", "name": "Max"}, {"value": "default", "name": "Default"}]}
        ]});
        let mut st = State::default();
        read_effort(&mut st, &session);
        assert_eq!(st.effort_option.as_deref(), Some("effort"));
        let (mut models, current, _) = models_from(&session);
        add_efforts(&mut models, current.as_deref(), &st.efforts);
        assert_eq!(models[0].efforts, ["low", "max"], "default isn't a level");
        assert!(models[1].efforts.is_empty());
        read_effort(&mut st, &json!({"configOptions": []}));
        assert!(st.effort_option.is_none());
    }

    #[test]
    #[ignore]
    fn real_antigravity() {
        use crate::driver::{Driver, Prompt};
        let dir = std::env::var("MOONFISH_TEST_DIR").expect("MOONFISH_TEST_DIR");
        let events = Arc::new(Mutex::new(Vec::<ChatEvent>::new()));
        let e = events.clone();
        let sink: Sink = Arc::new(move |ev| e.lock().unwrap().push(ev));
        let opts = StartOptions {
            kind: AgentKind::Antigravity,
            cwd: dir.clone(),
            location: moonfish_core::settings::Location::Local,
            binary: String::new(),
            model: Some("gemini-3.8-flash".into()),
            effort: Some("low".into()),
            fast_mode: None,
            permission: PermissionMode::Ask,
            resume: std::env::var("MOONFISH_TEST_RESUME").ok(),
            env: vec![],
        };
        let mut d = AcpDriver::start(opts, sink).expect("start agy");
        thread::sleep(Duration::from_millis(800));
        {
            let evs = events.lock().unwrap();
            for e in evs.iter() {
                match e {
                    ChatEvent::Started { session_id, model, .. } => eprintln!("started session={session_id:?} model={model:?}"),
                    ChatEvent::Models { models, current } => eprintln!(
                        "models (current {current:?}): {}",
                        models.iter().map(|m| format!("{} {:?}", m.id, m.efforts)).collect::<Vec<_>>().join(", ")
                    ),
                    ChatEvent::Commands { commands } => {
                        eprintln!("commands: {}", commands.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(" "))
                    }
                    ChatEvent::Notice { text, .. } => eprintln!("notice: {text}"),
                    _ => {}
                }
            }
            let models = evs
                .iter()
                .rev()
                .find_map(|e| if let ChatEvent::Models { models, .. } = e { Some(models.clone()) } else { None })
                .expect("models");
            assert!(models.iter().any(|m| m.efforts.len() > 1), "efforts per model");
            let current = evs.iter().rev().find_map(|e| if let ChatEvent::Models { current, .. } = e { current.clone() } else { None });
            assert_eq!(current.as_deref(), Some("gemini-3.8-flash"));
        }
        let st = || {
            let s = d.state.lock().unwrap();
            (s.efforts.clone(), s.resumable)
        };
        eprintln!("after start: {:?}", st());
        assert!(st().1, "resumable");
        d.set_permission(PermissionMode::Plan).expect("plan mode");
        d.set_permission(PermissionMode::AcceptEdits).expect("accept-edits mode");
        d.set_permission(PermissionMode::Ask).expect("default mode");

        let from = events.lock().unwrap().len();
        d.send(Prompt {
            id: "p1".into(),
            text: "Run the shell command `echo moonfish-agy-check` and tell me exactly what it printed. Nothing else.".into(),
            images: vec![],
            files: vec![],
        })
        .unwrap();
        let t0 = std::time::Instant::now();
        let mut approvals = 0;
        loop {
            assert!(t0.elapsed() < Duration::from_secs(240), "turn took too long");
            thread::sleep(Duration::from_millis(200));
            let evs: Vec<ChatEvent> = events.lock().unwrap()[from..].to_vec();
            for e in &evs {
                if let ChatEvent::Approval { request } = e {
                    if d.state.lock().unwrap().pending.contains_key(&request.request_id) {
                        approvals += 1;
                        eprintln!("  approval: {:?} {} — {:?}", request.kind, request.title, request.detail);
                        d.answer(&request.request_id, Decision::Allow).unwrap();
                    }
                }
                if let ChatEvent::Question { request } = e {
                    if d.state.lock().unwrap().choices.contains_key(&request.request_id) {
                        eprintln!("  question: {:?}", request.questions[0]);
                        let first = request.questions[0].options[0].label.clone();
                        d.answer_question(&request.request_id, [("answer".to_string(), vec![first])].into_iter().collect()).unwrap();
                    }
                }
            }
            if evs.iter().any(|e| matches!(e, ChatEvent::TurnFinished { .. })) {
                let mut text = String::new();
                for e in &evs {
                    match e {
                        ChatEvent::TextDelta { text: t, .. } => text.push_str(t),
                        ChatEvent::ReasoningDelta { .. } => {}
                        ChatEvent::ToolStarted { tool, .. } => eprintln!("  tool: {:?} {} {}", tool.kind, tool.title, tool.input),
                        ChatEvent::ToolFinished { ok, output, .. } => eprintln!("  tool done ok={ok} output={output:?}"),
                        ChatEvent::Context { used_tokens, window_tokens, .. } => eprintln!("  context {used_tokens}/{window_tokens}"),
                        ChatEvent::TurnFinished { usage, cost_usd, interrupted, .. } => {
                            eprintln!("  finished usage={usage:?} cost={cost_usd:?} interrupted={interrupted}")
                        }
                        ChatEvent::Error { message } => eprintln!("  error: {message}"),
                        ChatEvent::Notice { text, .. } => eprintln!("  notice: {text}"),
                        _ => {}
                    }
                }
                eprintln!("answer: {text}");
                assert!(text.contains("moonfish-agy-check"), "the command's output comes back: {text}");
                break;
            }
        }
        eprintln!("approvals asked: {approvals}");
        d.stop();
    }

    #[test]
    fn other_tool_approvals_show_formatted_input_details() {
        let req = json!({"jsonrpc": "2.0", "id": 11, "method": "session/request_permission", "params": {"sessionId": "s",
            "toolCall": {"toolCallId": "m1", "kind": "other", "title": "github · create_issue",
                "rawInput": {"owner": "moonfish", "repo": "app", "title": "Fix bug", "draft": false}},
            "options": [{"optionId": "once", "name": "Allow", "kind": "allow_once"}]}});
        let (ev, _) = run_as(Some(AgentKind::Antigravity), &[req], PermissionMode::Ask);
        match &ev[0] {
            ChatEvent::Approval { request } => {
                assert_eq!(request.kind, ApprovalKind::Other);
                assert_eq!(request.title, "github · create_issue");
                assert_eq!(request.detail.as_deref(), Some("draft: false\nowner: moonfish\nrepo: app\ntitle: Fix bug"));
            }
            e => panic!("{e:?}"),
        }
        let desc_only = json!({"jsonrpc": "2.0", "id": 12, "method": "session/request_permission", "params": {"sessionId": "s",
            "toolCall": {"toolCallId": "m2", "kind": "other", "title": "Fetch docs", "rawInput": {"description": "Read API reference"}},
            "options": [{"optionId": "once", "name": "Allow", "kind": "allow_once"}]}});
        let (ev, _) = run_as(Some(AgentKind::Antigravity), &[desc_only], PermissionMode::Ask);
        assert!(matches!(&ev[0], ChatEvent::Approval { request } if request.detail.as_deref() == Some("Read API reference")));
        assert_eq!(input_summary(&json!({})), None);
        assert_eq!(input_summary(&Value::Null), None);
    }

    #[test]
    fn tool_calls_left_open_end_as_background_tasks() {
        let lines = [
            upd(
                json!({"sessionUpdate": "tool_call", "toolCallId": "c1", "kind": "execute", "title": "npm run dev", "status": "in_progress"}),
            ),
            upd(json!({"sessionUpdate": "tool_call", "toolCallId": "c2", "kind": "read", "title": "a.txt", "status": "in_progress"})),
            upd(json!({"sessionUpdate": "tool_call_update", "toolCallId": "c2", "status": "completed"})),
        ];
        let out = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let sink: Sink = Arc::new(move |e| o.lock().unwrap().push(e));
        let state =
            Arc::new(Mutex::new(State { alive: true, busy: true, turn_id: Some("turn-1".into()), stretch: 1, ..Default::default() }));
        let mut tr = Translator { sink, state: state.clone(), rpc: Rpc::new(None, "ACP") };
        for l in &lines {
            tr.handle(l);
        }
        // Only the one still running is open.
        assert_eq!(state.lock().unwrap().open.keys().collect::<Vec<_>>(), ["c1"]);
        // Say the turn ended with it open: it's a task, and its later update ends it.
        {
            let mut s = state.lock().unwrap();
            let call = s.open.remove("c1").unwrap();
            let mut t = BackgroundTask::running("c1", TaskKind::Command, call.title);
            t.item_id = Some("c1".into());
            s.tasks.add(t);
        }
        tr.handle(&upd(json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed", "rawOutput": {"combinedOutput": "ready\nlistening on :3000\n"}})));
        let ev = out.lock().unwrap().clone();
        let Some(ChatEvent::Tasks { tasks }) = ev.iter().rev().find(|e| matches!(e, ChatEvent::Tasks { .. })) else { panic!("no tasks") };
        assert_eq!((tasks[0].status, tasks[0].detail.as_deref()), (TaskStatus::Completed, Some("listening on :3000")));
        assert!(matches!(ev.last(), Some(ChatEvent::Notice { text, .. }) if text.contains("npm run dev")));
    }

    #[test]
    #[ignore]
    fn real_antigravity_background() {
        use crate::driver::{Driver, Prompt};
        let dir = std::env::var("MOONFISH_TEST_DIR").expect("MOONFISH_TEST_DIR");
        let events = Arc::new(Mutex::new(Vec::<ChatEvent>::new()));
        let e = events.clone();
        let sink: Sink = Arc::new(move |ev| e.lock().unwrap().push(ev));
        let opts = StartOptions {
            kind: AgentKind::Antigravity,
            cwd: dir,
            location: moonfish_core::settings::Location::Local,
            binary: String::new(),
            model: Some("gemini-3.8-flash".into()),
            effort: Some("low".into()),
            fast_mode: None,
            permission: PermissionMode::FullAccess,
            resume: None,
            env: vec![],
        };
        let mut d = AcpDriver::start(opts, sink).expect("start agy");
        let text = "Run this shell command in the background (don't wait for it): sleep 30; echo bg-finished > done.txt . Then immediately reply 'started' and end your turn. Do not check on it.";
        d.send(Prompt { id: "p1".into(), text: text.into(), images: vec![], files: vec![] }).unwrap();
        for _ in 0..240 {
            if events.lock().unwrap().iter().any(|e| matches!(e, ChatEvent::TurnFinished { .. })) {
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
        let evs = events.lock().unwrap().clone();
        for e in &evs {
            if matches!(
                e,
                ChatEvent::Tasks { .. } | ChatEvent::TurnFinished { .. } | ChatEvent::ToolStarted { .. } | ChatEvent::ToolFinished { .. }
            ) {
                eprintln!("{}", serde_json::to_string(e).unwrap());
            }
        }
        let tasks = evs.iter().find_map(|e| if let ChatEvent::Tasks { tasks } = e { Some(tasks.clone()) } else { None }).expect("a task");
        assert_eq!((tasks[0].kind, tasks[0].status, tasks[0].tracked), (TaskKind::Command, TaskStatus::Running, false));
        assert_eq!(d.background(), 0, "an untracked task doesn't keep the chat awake");
        d.stop();
    }

    #[test]
    fn invoke_subagent_emits_live_tasks_and_promotes_if_outliving_turn() {
        let lines = [
            upd(json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "sa1",
                "kind": "other",
                "title": "invoke_subagent",
                "status": "in_progress",
                "rawInput": {"description": "Explore auth flow", "prompt": "Check src/auth.rs"}
            })),
            upd(json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "sa1",
                "status": "in_progress",
                "content": [{"type": "content", "content": {"type": "text", "text": "Reading src/auth.rs"}}]
            })),
            upd(json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "sa1",
                "status": "completed",
                "content": [{"type": "content", "content": {"type": "text", "text": "Checked 4 files\nAuth looks solid"}}]
            })),
            upd(json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "sa2",
                "kind": "task",
                "title": "Watch CI pipeline",
                "status": "in_progress"
            })),
        ];
        let out = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let sink: Sink = Arc::new(move |e| o.lock().unwrap().push(e));
        let state = Arc::new(Mutex::new(State {
            alive: true,
            busy: true,
            turn_id: Some("turn-1".into()),
            stretch: 1,
            kind: Some(AgentKind::Antigravity),
            ..Default::default()
        }));
        let mut tr = Translator { sink, state: state.clone(), rpc: Rpc::new(None, "ACP") };
        for l in &lines {
            tr.handle(l);
        }
        assert_eq!(state.lock().unwrap().tasks.running(), 0);

        let ev = out.lock().unwrap().clone();
        let started_task =
            ev.iter().any(|e| matches!(e, ChatEvent::ToolStarted { item_id, tool } if item_id == "sa1" && tool.kind == ToolKind::Task));
        assert!(started_task);
        let lists: Vec<&Vec<BackgroundTask>> =
            ev.iter().filter_map(|e| if let ChatEvent::Tasks { tasks } = e { Some(tasks) } else { None }).collect();
        assert_eq!(lists.len(), 4);
        assert_eq!(
            (lists[0][0].id.as_str(), lists[0][0].kind, lists[0][0].title.as_str(), lists[0][0].background, lists[0][0].item_id.as_deref()),
            ("sa1", TaskKind::Agent, "Explore auth flow", false, Some("sa1"))
        );
        assert_eq!(lists[1][0].detail.as_deref(), Some("Reading src/auth.rs"));
        assert_eq!(
            (lists[2][0].status, lists[2][0].detail.as_deref(), lists[2][0].background),
            (TaskStatus::Completed, Some("Auth looks solid"), false)
        );
        assert!(!ev.iter().any(|e| matches!(e, ChatEvent::Notice { .. })));

        // When the turn finishes, `sa2` is still running and gets promoted (untracked for Antigravity).
        let promoted = finish_turn_tasks(&mut state.lock().unwrap(), true);
        let Some(ChatEvent::Tasks { tasks }) = promoted else { panic!("expected promoted tasks") };
        let sa2 = tasks.iter().find(|t| t.id == "sa2").unwrap();
        assert_eq!((sa2.kind, sa2.status, sa2.background, sa2.tracked), (TaskKind::Agent, TaskStatus::Running, true, false));
        assert_eq!(state.lock().unwrap().tasks.running(), 0, "untracked Antigravity task does not block sleep");
    }

    #[test]
    fn refuses_file_system_requests() {
        let (ev, _) = run(
            &[json!({"jsonrpc": "2.0", "id": 3, "method": "fs/read_text_file", "params": {"path": "/etc/passwd"}})],
            PermissionMode::Ask,
        );
        assert!(ev.is_empty());
    }
}
