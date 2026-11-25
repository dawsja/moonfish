//! What a chat shows, whichever CLI is behind it. Each driver translates its CLI's protocol into
//! these events; the UI folds them into a transcript, and the app appends them to the chat's
//! `events.jsonl`, so reopening a chat is replaying its events.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ChatEvent {
    /// The CLI is up. `session_id` is the CLI's own id, used to resume the conversation later.
    Started {
        session_id: Option<String>,
        model: Option<String>,
        cwd: Option<String>,
    },
    /// What the user sent (echoed so the transcript and the log hold it).
    UserMessage {
        id: String,
        text: String,
        /// The attached images' types.
        images: Vec<String>,
        /// The same images saved with the chat (`chat_image` reads them back), in order.
        #[serde(default)]
        files: Vec<String>,
    },
    TurnStarted {
        turn_id: String,
    },
    GoalChanged {
        goal: Option<moonfish_core::chats::Goal>,
    },
    /// Streaming assistant text for `item_id`, appended.
    TextDelta {
        item_id: String,
        text: String,
    },
    /// The whole assistant text for `item_id` (replaces what streamed).
    Text {
        item_id: String,
        text: String,
    },
    ReasoningDelta {
        item_id: String,
        text: String,
    },
    Reasoning {
        item_id: String,
        text: String,
    },
    ToolStarted {
        item_id: String,
        tool: ToolCall,
    },
    ToolFinished {
        item_id: String,
        ok: bool,
        output: String,
        diff: Option<FileDiff>,
    },
    /// The agent asks before doing something.
    Approval {
        request: ApprovalRequest,
    },
    ApprovalResolved {
        request_id: String,
        decision: Decision,
    },
    /// The agent asks the user to choose or type something.
    Question {
        request: QuestionRequest,
    },
    /// Answers by question id (several for multi-select; empty when skipped).
    QuestionResolved {
        request_id: String,
        answers: std::collections::BTreeMap<String, Vec<String>>,
    },
    /// The models this agent offers, and the one in use.
    Models {
        models: Vec<ModelOption>,
        current: Option<String>,
    },
    /// The agent switched permission mode itself (out of plan mode once a plan is approved).
    PermissionChanged {
        mode: moonfish_core::settings::PermissionMode,
    },
    /// The slash commands this agent offers (replaced whole).
    Commands {
        commands: Vec<SlashCommand>,
    },
    /// The agent's plan / todo list, replaced whole.
    Plan {
        items: Vec<PlanItem>,
    },
    /// Work the agent runs beside the conversation (background commands, subagents, watchers),
    /// replaced whole: running ones, and ones that ended since the user last wrote.
    Tasks {
        tasks: Vec<BackgroundTask>,
    },
    /// An event from a subagent's own thread (identified by `task_id`).
    SubagentEvent {
        task_id: String,
        event: Box<ChatEvent>,
    },
    TurnFinished {
        turn_id: String,
        usage: Option<Usage>,
        cost_usd: Option<f64>,
        duration_ms: Option<u64>,
        interrupted: bool,
    },
    /// Context window fill after a turn, when the CLI reports it.
    Context {
        used_tokens: u64,
        window_tokens: u64,
        /// What the window holds, when the agent says (Claude Code's `/context`): largest first.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        parts: Vec<ContextPart>,
    },
    Notice {
        level: NoticeLevel,
        text: String,
    },
    Error {
        message: String,
    },
    Exited {
        code: Option<i32>,
        message: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    Read,
    Edit,
    Write,
    Bash,
    Search,
    Web,
    Task,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub kind: ToolKind,
    /// The CLI's own tool name (`Bash`, `apply_patch`…).
    pub name: String,
    /// One line: the command, the path, the pattern.
    pub title: String,
    /// The raw input, for the expanded view.
    pub input: serde_json::Value,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    /// Unified diff text (`@@` hunks, `+`/`-`/` ` lines).
    pub patch: String,
    pub added: u32,
    pub removed: u32,
}

impl FileDiff {
    /// Builds a diff from a patch, counting its added and removed lines.
    pub fn from_patch(path: impl Into<String>, patch: impl Into<String>) -> FileDiff {
        let patch = patch.into();
        let (mut added, mut removed) = (0, 0);
        for l in patch.lines() {
            if l.starts_with("+++") || l.starts_with("---") {
                continue;
            }
            if l.starts_with('+') {
                added += 1;
            } else if l.starts_with('-') {
                removed += 1;
            }
        }
        FileDiff { path: path.into(), patch, added, removed }
    }

    /// A diff of replacing `old` with `new` in `path` (a whole-hunk diff, no context search).
    pub fn replace(path: impl Into<String>, old: &str, new: &str) -> FileDiff {
        let mut patch = format!("@@ -1,{} +1,{} @@\n", old.lines().count(), new.lines().count());
        for l in old.lines() {
            patch.push('-');
            patch.push_str(l);
            patch.push('\n');
        }
        for l in new.lines() {
            patch.push('+');
            patch.push_str(l);
            patch.push('\n');
        }
        FileDiff::from_patch(path, patch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalKind {
    Command,
    Edit,
    /// Leave plan mode and start on the plan in `detail` (Markdown).
    Plan,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub request_id: String,
    pub item_id: Option<String>,
    pub kind: ApprovalKind,
    /// "Run a command", "Edit src/main.rs".
    pub title: String,
    /// The command line, or why.
    pub detail: Option<String>,
    pub diff: Option<FileDiff>,
    /// Whether "allow for this chat" is offered.
    pub can_remember: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionRequest {
    pub request_id: String,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    /// A short label ("Database").
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    /// More than one option may be picked.
    pub multi: bool,
    /// A typed answer is allowed besides the options.
    pub other: bool,
    /// The answer shouldn't be shown or logged (a password).
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPart {
    /// "Messages", "System tools", "Free space"…
    pub name: String,
    pub tokens: u64,
    /// `used`, `deferred` (loaded only when needed), `buffer` (kept free for compaction) or `free`.
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    /// Without the slash: `compact`, `review`.
    pub name: String,
    pub description: String,
    /// What goes after the name, e.g. `<instructions>`.
    pub hint: String,
    /// A Codex skill: mentioned as `$name` in a message, not run as `/name`.
    #[serde(default)]
    pub skill: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    pub id: String,
    pub name: String,
    pub description: String,
    /// The reasoning effort levels this model takes (`low` … `max`), in order; empty when it has
    /// none to pick.
    #[serde(default)]
    pub efforts: Vec<String>,
    /// The level it uses when none is picked, when the agent says.
    #[serde(default)]
    pub default_effort: Option<String>,
    /// Inherited speed when selectable. Missing for unknown or unavailable Fast mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_mode: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Decision {
    Allow,
    /// Allow this and the same kind of request for the rest of the chat.
    AllowAlways,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub text: String,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskKind {
    /// A shell command left running (a dev server, a build, `sleep && …`).
    Command,
    /// A subagent working on its own.
    Agent,
    /// Something watched for a change (Claude Code's Monitor).
    Watch,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Running,
    Completed,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTask {
    /// The agent's id for it (Claude's task id, Codex's process id, an ACP tool call id).
    pub id: String,
    pub kind: TaskKind,
    /// What it is: the command, or the subagent's job.
    pub title: String,
    pub status: TaskStatus,
    /// The latest word on it: progress, or how it ended.
    #[serde(default)]
    pub detail: Option<String>,
    /// The tool call that started it, when there is one in the transcript.
    #[serde(default)]
    pub item_id: Option<String>,
    /// Unix ms.
    pub started_at: u64,
    #[serde(default)]
    pub ended_at: Option<u64>,
    /// `false`: the agent never says when it ends (Antigravity's background commands), so
    /// "running" means "started, and not heard of since".
    #[serde(default = "yes")]
    pub tracked: bool,
    /// Moonfish can stop it (`Driver::stop_task`).
    #[serde(default)]
    pub stoppable: bool,
    /// `true`: outlives a turn (or runs in the background), so it keeps the chat alive;
    /// `false`: running inside the current turn (e.g. an in-turn subagent).
    #[serde(default = "yes")]
    pub background: bool,
    /// The subagent's own session/thread id, when it can be watched or messaged directly.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Moonfish can send messages directly to this subagent (`Driver::send_subagent`).
    #[serde(default)]
    pub interactive: bool,
}

fn yes() -> bool {
    true
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl BackgroundTask {
    pub fn running(id: impl Into<String>, kind: TaskKind, title: impl Into<String>) -> BackgroundTask {
        BackgroundTask {
            id: id.into(),
            kind,
            title: title.into(),
            status: TaskStatus::Running,
            detail: None,
            item_id: None,
            started_at: now_ms(),
            ended_at: None,
            tracked: true,
            stoppable: false,
            background: true,
            session_id: None,
            interactive: false,
        }
    }

    pub fn in_turn(id: impl Into<String>, kind: TaskKind, title: impl Into<String>) -> BackgroundTask {
        let mut t = BackgroundTask::running(id, kind, title);
        t.background = false;
        t
    }

    pub fn is_running(&self) -> bool {
        self.status == TaskStatus::Running
    }
}

/// A driver's background tasks, in the order they started. Each change returns whether there
/// was one, so the driver sends [`ChatEvent::Tasks`] only then.
#[derive(Debug, Default)]
pub struct TaskList {
    tasks: Vec<BackgroundTask>,
}

impl TaskList {
    pub fn get(&self, id: &str) -> Option<&BackgroundTask> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// Adds a task, or (same id) fills in what it says that the one we have doesn't.
    pub fn add(&mut self, task: BackgroundTask) -> bool {
        match self.tasks.iter_mut().find(|t| t.id == task.id) {
            Some(t) => {
                let before = t.clone();
                t.title = task.title;
                t.detail = task.detail.or(t.detail.take());
                t.item_id = task.item_id.or(t.item_id.take());
                t.session_id = task.session_id.or(t.session_id.take());
                t.interactive |= task.interactive;
                if task.background {
                    t.background = true;
                    t.tracked = task.tracked;
                }
                *t != before
            }
            None => {
                self.tasks.push(task);
                true
            }
        }
    }

    /// Marks a running task as background work (it outlived its turn).
    pub fn promote_to_background(&mut self, id: &str) -> bool {
        let Some(t) = self.tasks.iter_mut().find(|t| t.id == id && t.is_running()) else { return false };
        if t.background {
            return false;
        }
        t.background = true;
        true
    }

    pub fn set_subagent_session(&mut self, id: &str, session_id: String, interactive: bool) -> bool {
        let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) else { return false };
        let changed = t.session_id.as_deref() != Some(session_id.as_str()) || t.interactive != interactive;
        t.session_id = Some(session_id);
        t.interactive = interactive;
        changed
    }

    pub fn reopen(&mut self, id: &str) -> bool {
        let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) else { return false };
        if t.status == TaskStatus::Running {
            return false;
        }
        t.status = TaskStatus::Running;
        t.ended_at = None;
        true
    }

    /// Marks every running in-turn subagent as background work (they outlived the turn).
    pub fn promote_running_agents(&mut self) -> bool {
        self.promote_running_agents_with(true)
    }

    /// Marks every running in-turn subagent as background work, setting whether the agent
    /// reports when they end.
    pub fn promote_running_agents_with(&mut self, tracked: bool) -> bool {
        let mut changed = false;
        for t in self.tasks.iter_mut().filter(|t| t.kind == TaskKind::Agent && t.is_running() && !t.background) {
            t.background = true;
            t.tracked = tracked;
            changed = true;
        }
        changed
    }

    /// Ends every running in-turn task (the turn was interrupted before they finished).
    pub fn stop_in_turn(&mut self) -> bool {
        let mut changed = false;
        for t in self.tasks.iter_mut().filter(|t| t.is_running() && !t.background) {
            t.status = TaskStatus::Stopped;
            t.ended_at = Some(now_ms());
            changed = true;
        }
        changed
    }

    /// New progress for a running task.
    pub fn progress(&mut self, id: &str, title: Option<&str>, detail: Option<&str>) -> bool {
        let Some(t) = self.tasks.iter_mut().find(|t| t.id == id && t.is_running()) else { return false };
        let mut changed = false;
        if let Some(title) = title.filter(|s| !s.is_empty() && *s != t.title) {
            t.title = title.to_string();
            changed = true;
        }
        if let Some(d) = detail.filter(|s| !s.is_empty()) {
            if t.detail.as_deref() != Some(d) {
                t.detail = Some(d.to_string());
                changed = true;
            }
        }
        changed
    }

    /// Ends a running task (`detail`: how, when the agent says). The ended task, if it was running.
    pub fn finish(&mut self, id: &str, status: TaskStatus, detail: Option<String>) -> Option<&BackgroundTask> {
        let t = self.tasks.iter_mut().find(|t| t.id == id && t.is_running())?;
        t.status = status;
        t.ended_at = Some(now_ms());
        if detail.as_deref().is_some_and(|d| !d.is_empty()) {
            t.detail = detail;
        }
        Some(t)
    }

    /// Replaces an ended task's detail (how it ended, said after it ended).
    pub fn set_detail(&mut self, id: &str, detail: Option<String>) -> bool {
        match self.tasks.iter_mut().find(|t| t.id == id) {
            Some(t) if detail.is_some() && t.detail != detail => {
                t.detail = detail;
                true
            }
            _ => false,
        }
    }

    /// Ends every running task (the agent stopped, so they went with it).
    pub fn stop_all(&mut self) -> bool {
        let mut changed = false;
        for t in self.tasks.iter_mut().filter(|t| t.is_running()) {
            t.status = TaskStatus::Stopped;
            t.ended_at = Some(now_ms());
            changed = true;
        }
        changed
    }

    /// Forgets the ended tasks (the user wrote again: they've been seen).
    pub fn clear_ended(&mut self) -> bool {
        let before = self.tasks.len();
        self.tasks.retain(|t| t.is_running());
        self.tasks.len() != before
    }

    /// Forgets the running tasks the agent never reports on.
    pub fn forget_untracked(&mut self) -> bool {
        let before = self.tasks.len();
        self.tasks.retain(|t| t.tracked || !t.is_running());
        self.tasks.len() != before
    }

    /// The running background tasks the agent reports on (the others are in-turn or untracked).
    pub fn running(&self) -> usize {
        self.tasks.iter().filter(|t| t.is_running() && t.tracked && t.background).count()
    }

    pub fn event(&self) -> ChatEvent {
        ChatEvent::Tasks { tasks: self.tasks.clone() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeLevel {
    Info,
    Warn,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl Usage {
    /// Everything the model read and wrote, cache included.
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }

    /// New tokens in and out: what a turn added, without the cached context it re-read (an
    /// agent re-reads the whole conversation from cache at every step, so counting that
    /// makes a one-cent turn look like 150k tokens).
    pub fn fresh(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_write_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_with_a_type_tag_in_camel_case() {
        let e = ChatEvent::TextDelta { item_id: "a1".into(), text: "hi".into() };
        assert_eq!(serde_json::to_string(&e).unwrap(), r#"{"type":"textDelta","itemId":"a1","text":"hi"}"#);
        let back: ChatEvent = serde_json::from_str(r#"{"type":"approvalResolved","requestId":"r","decision":"allowAlways"}"#).unwrap();
        assert_eq!(back, ChatEvent::ApprovalResolved { request_id: "r".into(), decision: Decision::AllowAlways });
    }

    #[test]
    fn counts_diff_lines() {
        let d = FileDiff::from_patch("a.rs", "--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n+more\n same\n");
        assert_eq!((d.added, d.removed), (2, 1));
        let r = FileDiff::replace("b.rs", "x\ny", "z");
        assert_eq!((r.added, r.removed), (1, 2));
    }

    #[test]
    fn background_task_defaults_and_in_turn_counting() {
        // Older `events.jsonl` entries without `background` deserialize as `true`.
        let legacy = r#"{"id":"b1","kind":"command","title":"npm run dev","status":"running","startedAt":1}"#;
        let parsed: BackgroundTask = serde_json::from_str(legacy).unwrap();
        assert!(parsed.background);
        assert!(parsed.tracked);

        let in_turn = BackgroundTask::in_turn("a1", TaskKind::Agent, "Explore repo");
        assert!(!in_turn.background);
        let json = serde_json::to_string(&in_turn).unwrap();
        assert!(json.contains(r#""background":false"#));
        let round_trip: BackgroundTask = serde_json::from_str(&json).unwrap();
        assert!(!round_trip.background);

        let mut list = TaskList::default();
        assert!(list.add(in_turn));
        assert_eq!(list.running(), 0, "in-turn subagents do not count as background work");
        assert!(list.progress("a1", None, Some("Using Grep")));
        assert_eq!(list.get("a1").and_then(|t| t.detail.as_deref()), Some("Using Grep"));

        assert!(list.add(BackgroundTask::running("c1", TaskKind::Command, "sleep 10")));
        assert_eq!(list.running(), 1);

        assert!(list.promote_to_background("a1"));
        assert!(!list.promote_to_background("a1"));
        assert!(list.get("a1").unwrap().background);
        assert_eq!(list.running(), 2);

        // `add` with `background: true` upgrades an existing in-turn entry.
        assert!(list.add(BackgroundTask::in_turn("a2", TaskKind::Agent, "Audit auth")));
        assert_eq!(list.running(), 2);
        assert!(list.add(BackgroundTask::running("a2", TaskKind::Agent, "Audit auth")));
        assert!(list.get("a2").unwrap().background);
        assert_eq!(list.running(), 3);

        assert!(list.finish("a1", TaskStatus::Completed, Some("done".into())).is_some());
        assert_eq!(list.running(), 2);

        // Untracked promoted agents do not keep the chat alive.
        assert!(list.add(BackgroundTask::in_turn("a3", TaskKind::Agent, "Watch logs")));
        assert!(list.promote_running_agents_with(false));
        assert!(list.get("a3").unwrap().background);
        assert!(!list.get("a3").unwrap().tracked);
        assert_eq!(list.running(), 2);

        // Stopping in-turn tasks leaves already-background tasks running.
        assert!(list.add(BackgroundTask::in_turn("a4", TaskKind::Agent, "Draft notes")));
        assert!(list.stop_in_turn());
        assert_eq!(list.get("a4").unwrap().status, TaskStatus::Stopped);
        assert_eq!(list.get("a2").unwrap().status, TaskStatus::Running);
    }
}
