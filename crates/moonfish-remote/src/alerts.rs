//! The phone's alerts: from what the desktop says is busy and waiting ([`Status`]), which
//! notifications to show, replace or take away, and when to keep the connection open in the
//! background. Nothing here touches Android: the phone app turns each [`Action`] into a
//! notification or a foreground service (phone/src-tauri/src/alerts.rs).
//!
//! - One notification per chat, replaced as the chat moves on (a new approval, the turn ending),
//!   and taken away once nothing about it needs you.
//! - "Needs you" (an approval, a plan, a question) always; "Done" only when [`Prefs::done`].
//! - The connection is kept while a chat is busy or waiting, and let go [`IDLE_GRACE_MS`] after
//!   the last one goes idle. While it's kept, its own quiet notification says whether the desktop
//!   is reachable, so silence never passes for progress.
//!
//! Times are wall-clock milliseconds, passed in.

use std::collections::{BTreeMap, BTreeSet};

use crate::proto::{Need, Status};

/// After the last chat goes idle, the connection stays this long: a queued message or the next
/// turn of a goal often starts right away.
pub const IDLE_GRACE_MS: u64 = 60_000;
/// Without the desktop for this long, the phone stops watching and says so, rather than keep
/// trying in the background for hours.
pub const GIVE_UP_MS: u64 = 10 * 60_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Prefs {
    /// Also when a chat finishes its turn (or stops with an error).
    pub done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Approval,
    Plan,
    Question,
    Done,
    Failed,
}

impl AlertKind {
    /// It waits on you (rather than telling you something ended).
    pub fn needs_you(self) -> bool {
        matches!(self, AlertKind::Approval | AlertKind::Plan | AlertKind::Question)
    }
}

/// A chat's notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub chat_id: String,
    pub kind: AlertKind,
    pub title: String,
    pub body: String,
    /// Shown when it's expanded: the command, the start of the plan.
    pub detail: Option<String>,
    /// An approval that Allow and Deny on the notification answer (not plans: those are read
    /// first).
    pub request_id: Option<String>,
    /// Sound and heads-up: something new. An update of what's shown stays quiet.
    pub loud: bool,
}

/// What the kept connection's own ongoing notification says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    pub title: String,
    pub text: String,
    /// The desktop isn't answering.
    pub lost: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Show the chat's notification, in place of the one it has.
    Alert(Alert),
    /// Take the chat's notification away.
    Clear(String),
    /// Keep the connection open in the background (the service starts), saying this.
    Watch(Watch),
    /// Stop keeping it open.
    Release,
    /// A quiet note that the phone stopped watching (or, `None`, take it away).
    Note(Option<String>),
}

/// What a chat's notification was about when it was shown.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Told {
    /// What it waits on (the request, or the kind from a desktop that doesn't say which), and
    /// the alert, to show again while an answer goes.
    Need { key: String, alert: Alert },
    /// Its turn ended.
    Ended,
}

/// Follows one desktop's chats across its sessions.
#[derive(Debug, Default)]
pub struct Tracker {
    desktop: String,
    told: BTreeMap<String, Told>,
    /// Busy or waiting at the last status.
    active: BTreeSet<String>,
    titles: BTreeMap<String, String>,
    /// While the connection is kept: what its notification says.
    watch: Option<Watch>,
    idle_since: Option<u64>,
    lost_since: Option<u64>,
    noted: bool,
}

impl Tracker {
    pub fn new(desktop: &str) -> Tracker {
        Tracker { desktop: if desktop.trim().is_empty() { "the desktop".into() } else { desktop.trim().to_string() }, ..Tracker::default() }
    }

    /// The connection is being kept open.
    pub fn watching(&self) -> bool {
        self.watch.is_some()
    }

    /// The desktop's latest status (`now`: when it came).
    pub fn status(&mut self, s: &Status, prefs: Prefs, now: u64) -> Vec<Action> {
        let mut out = Vec::new();
        self.lost_since = None;
        if self.noted {
            self.noted = false;
            out.push(Action::Note(None));
        }
        for (id, title) in &s.titles {
            if !title.trim().is_empty() {
                self.titles.insert(id.clone(), title.trim().to_string());
            }
        }
        let active: BTreeSet<String> = s.busy.iter().chain(s.waiting.keys()).cloned().collect();
        let chats: BTreeSet<String> = self.told.keys().chain(&self.active).chain(&active).cloned().collect();
        for id in chats {
            if let Some(need) = s.waiting.get(&id) {
                let key = need.request_id.clone().unwrap_or_else(|| need.kind.clone());
                if matches!(self.told.get(&id), Some(Told::Need { key: k, .. }) if *k == key) {
                    continue;
                }
                let alert = self.need(&id, need);
                self.told.insert(id, Told::Need { key, alert: alert.clone() });
                out.push(Action::Alert(alert));
            } else if self.active.contains(&id) && !active.contains(&id) {
                // The turn ended. Between two turns that start by themselves it isn't done.
                let ended = match s.ended.get(&id).map(String::as_str) {
                    Some("done") => Some(AlertKind::Done),
                    Some("error") => Some(AlertKind::Failed),
                    None if s.legacy => Some(AlertKind::Done),
                    _ => None,
                };
                match ended.filter(|_| prefs.done && !s.continuing.contains(&id)) {
                    Some(kind) => {
                        self.told.insert(id.clone(), Told::Ended);
                        out.push(Action::Alert(self.ended(&id, kind)));
                    }
                    None => {
                        if self.told.remove(&id).is_some() {
                            out.push(Action::Clear(id));
                        }
                    }
                }
            } else if active.contains(&id) || matches!(self.told.get(&id), Some(Told::Need { .. })) {
                // Busy again (a "Done" is old news), or answered and going on.
                if self.told.remove(&id).is_some() {
                    out.push(Action::Clear(id));
                }
            }
        }
        self.active = active;

        if !self.active.is_empty() {
            self.idle_since = None;
            let waiting = s.waiting.len();
            let working = self.active.len() - waiting;
            let mut text = Vec::new();
            if waiting > 0 {
                text.push(if waiting == 1 { "1 chat needs you".to_string() } else { format!("{waiting} chats need you") });
            }
            if working > 0 {
                text.push(if working == 1 { "1 chat working".to_string() } else { format!("{working} chats working") });
            }
            self.show_watch(Watch { title: format!("Following chats on {}", self.desktop), text: text.join(", "), lost: false }, &mut out);
        } else if self.watch.is_some() {
            self.idle_since.get_or_insert(now);
            self.show_watch(
                Watch { title: format!("Following chats on {}", self.desktop), text: "Nothing running now".into(), lost: false },
                &mut out,
            );
        }
        out
    }

    /// The session dropped (`now`: when). While watching, the ongoing notification says so.
    pub fn lost(&mut self, now: u64) -> Vec<Action> {
        if self.watch.is_none() {
            return Vec::new();
        }
        self.lost_since.get_or_insert(now);
        let mut out = Vec::new();
        self.show_watch(
            Watch { title: format!("Can't reach {}", self.desktop), text: "No alerts until it's back. Reconnecting…".into(), lost: true },
            &mut out,
        );
        out
    }

    /// Time passed: lets the connection go once everything has been idle a while, or the desktop
    /// has been gone too long.
    pub fn tick(&mut self, now: u64) -> Vec<Action> {
        if self.watch.is_none() {
            return Vec::new();
        }
        if self.idle_since.is_some_and(|t| now.saturating_sub(t) >= IDLE_GRACE_MS) {
            self.stop_watching();
            return vec![Action::Release];
        }
        if self.lost_since.is_some_and(|t| now.saturating_sub(t) >= GIVE_UP_MS) {
            self.stop_watching();
            self.noted = true;
            let minutes = GIVE_UP_MS / 60_000;
            return vec![
                Action::Release,
                Action::Note(Some(format!(
                    "Stopped following chats on {}. It hasn't answered for {minutes} minutes. Open Moonfish to reconnect.",
                    self.desktop
                ))),
            ];
        }
        Vec::new()
    }

    /// The chat is open on the phone: its notification goes, and comes back only for something
    /// new.
    pub fn seen(&mut self, chat_id: &str) -> Vec<Action> {
        if self.told.contains_key(chat_id) {
            vec![Action::Clear(chat_id.to_string())]
        } else {
            Vec::new()
        }
    }

    /// An answer from the notification is on its way: it says so, without its buttons.
    pub fn answering(&mut self, chat_id: &str, request_id: &str, allow: bool) -> Option<Action> {
        let alert = self.pending(chat_id, request_id)?;
        let what = if allow { "Allowing…" } else { "Denying…" };
        Some(Action::Alert(Alert { body: format!("{} · {what}", alert.body), request_id: None, loud: false, ..alert }))
    }

    /// That answer didn't go: the buttons come back, with why.
    pub fn answer_failed(&mut self, chat_id: &str, request_id: &str, why: &str) -> Option<Action> {
        let alert = self.pending(chat_id, request_id)?;
        let detail = Some(match &alert.detail {
            Some(d) => format!("Couldn't send your answer: {why}\n\n{d}"),
            None => format!("Couldn't send your answer: {why}"),
        });
        Some(Action::Alert(Alert { detail, loud: false, ..alert }))
    }

    /// Alerts are off, or this desktop isn't followed anymore: every notification goes and the
    /// connection is let go.
    pub fn reset(&mut self) -> Vec<Action> {
        let mut out: Vec<Action> = std::mem::take(&mut self.told).into_keys().map(Action::Clear).collect();
        if self.watch.is_some() {
            out.push(Action::Release);
        }
        if self.noted {
            out.push(Action::Note(None));
        }
        *self = Tracker { desktop: std::mem::take(&mut self.desktop), ..Tracker::default() };
        out
    }

    /// The approval `request_id` the chat's notification shows, if it still does.
    fn pending(&self, chat_id: &str, request_id: &str) -> Option<Alert> {
        match self.told.get(chat_id) {
            Some(Told::Need { alert, .. }) if alert.request_id.as_deref() == Some(request_id) => Some(alert.clone()),
            _ => None,
        }
    }

    fn stop_watching(&mut self) {
        self.watch = None;
        self.idle_since = None;
        self.lost_since = None;
    }

    fn show_watch(&mut self, w: Watch, out: &mut Vec<Action>) {
        if self.watch.as_ref() != Some(&w) {
            self.watch = Some(w.clone());
            out.push(Action::Watch(w));
        }
    }

    fn title(&self, chat_id: &str) -> String {
        self.titles.get(chat_id).cloned().unwrap_or_else(|| "A chat".into())
    }

    /// Worded as the desktop's own notifications (app/src/lib/notify.ts).
    fn need(&self, chat_id: &str, need: &Need) -> Alert {
        let (kind, title, fallback) = match need.kind.as_str() {
            "plan" => (AlertKind::Plan, "Plan to review", "A plan is ready for you"),
            "question" => (AlertKind::Question, "Question from the agent", "It's waiting for your answer"),
            _ => (AlertKind::Approval, "Approval needed", "It's waiting for your approval"),
        };
        let what = if need.title.trim().is_empty() { fallback } else { need.title.trim() };
        Alert {
            chat_id: chat_id.to_string(),
            kind,
            title: title.into(),
            body: format!("{}: {what}", self.title(chat_id)),
            detail: need.detail.clone().filter(|d| !d.trim().is_empty()),
            request_id: need.request_id.clone().filter(|_| kind == AlertKind::Approval),
            loud: true,
        }
    }

    fn ended(&self, chat_id: &str, kind: AlertKind) -> Alert {
        let (title, body) = match kind {
            AlertKind::Failed => ("Agent error", format!("{}: it stopped with an error", self.title(chat_id))),
            _ => ("Done", self.title(chat_id)),
        };
        Alert { chat_id: chat_id.to_string(), kind, title: title.into(), body, detail: None, request_id: None, loud: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEEDS: Prefs = Prefs { done: false };
    const DONE: Prefs = Prefs { done: true };

    fn status() -> Status {
        Status::default()
    }

    fn busy(s: Status, id: &str, title: &str) -> Status {
        let mut s = s;
        s.busy.push(id.into());
        s.titles.insert(id.into(), title.into());
        s
    }

    fn waiting(s: Status, id: &str, kind: &str, request: &str) -> Status {
        let mut s = busy(s, id, "Fix the login bug");
        let title = if kind == "question" { "Which database?" } else { "Run a command" };
        s.waiting.insert(
            id.into(),
            Need { kind: kind.into(), request_id: Some(request.into()), title: title.into(), detail: Some("npm test".into()) },
        );
        s
    }

    fn ended(id: &str, how: &str) -> Status {
        let mut s = status();
        s.ended.insert(id.into(), how.into());
        s
    }

    fn alerts(actions: &[Action]) -> Vec<&Alert> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Alert(a) => Some(a),
                _ => None,
            })
            .collect()
    }

    fn clears(actions: &[Action]) -> Vec<&str> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Clear(c) => Some(c.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_approval_alerts_once_with_allow_and_deny() {
        let mut t = Tracker::new("Studio");
        let s = waiting(status(), "c1", "approval", "r1");
        let a = t.status(&s, NEEDS, 0);
        let shown = alerts(&a);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].kind, AlertKind::Approval);
        assert_eq!(shown[0].title, "Approval needed");
        assert_eq!(shown[0].body, "Fix the login bug: Run a command");
        assert_eq!(shown[0].detail.as_deref(), Some("npm test"));
        assert_eq!(shown[0].request_id.as_deref(), Some("r1"));
        assert!(shown[0].loud);
        // The same status again (another chat changed): nothing new to say.
        assert!(alerts(&t.status(&s, NEEDS, 1)).is_empty());
    }

    #[test]
    fn questions_and_plans_alert_without_buttons() {
        let mut t = Tracker::new("Studio");
        let s = waiting(waiting(status(), "q", "question", "r1"), "p", "plan", "r2");
        let a = t.status(&s, NEEDS, 0);
        let shown = alerts(&a);
        assert_eq!(shown.len(), 2);
        let plan = shown.iter().find(|a| a.chat_id == "p").unwrap();
        assert_eq!((plan.kind, plan.title.as_str(), plan.request_id.clone()), (AlertKind::Plan, "Plan to review", None));
        let question = shown.iter().find(|a| a.chat_id == "q").unwrap();
        assert_eq!(question.title, "Question from the agent");
        assert_eq!(question.body, "Fix the login bug: Which database?");
        assert_eq!(question.request_id, None);
    }

    #[test]
    fn busy_chats_and_finished_ones_stay_quiet_by_default() {
        let mut t = Tracker::new("Studio");
        assert!(alerts(&t.status(&busy(status(), "c1", "Refactor"), NEEDS, 0)).is_empty());
        let a = t.status(&ended("c1", "done"), NEEDS, 1);
        assert!(alerts(&a).is_empty());
        assert!(clears(&a).is_empty());
    }

    #[test]
    fn a_new_request_in_the_same_chat_replaces_its_alert() {
        let mut t = Tracker::new("Studio");
        t.status(&waiting(status(), "c1", "approval", "r1"), NEEDS, 0);
        let a = t.status(&waiting(status(), "c1", "approval", "r2"), NEEDS, 1);
        let shown = alerts(&a);
        assert_eq!(shown.len(), 1, "one notification per chat");
        assert_eq!(shown[0].request_id.as_deref(), Some("r2"));
        assert!(shown[0].loud, "a new request is news");
        assert!(clears(&a).is_empty(), "replaced in place, not cleared and stacked");
    }

    #[test]
    fn an_answered_request_takes_its_alert_away() {
        let mut t = Tracker::new("Studio");
        t.status(&waiting(status(), "c1", "approval", "r1"), NEEDS, 0);
        // Answered (here, on the desktop or from the notification): the chat goes on.
        let a = t.status(&busy(status(), "c1", "Fix the login bug"), NEEDS, 1);
        assert_eq!(clears(&a), ["c1"]);
        assert!(alerts(&a).is_empty());
    }

    #[test]
    fn done_replaces_the_chats_alert_when_asked_for() {
        let mut t = Tracker::new("Studio");
        t.status(&waiting(status(), "c1", "approval", "r1"), DONE, 0);
        t.status(&busy(status(), "c1", "Fix the login bug"), DONE, 1);
        let a = t.status(&ended("c1", "done"), DONE, 2);
        let shown = alerts(&a);
        assert_eq!(shown.len(), 1);
        assert_eq!((shown[0].kind, shown[0].title.as_str(), shown[0].body.as_str()), (AlertKind::Done, "Done", "Fix the login bug"));
        // Busy again: the "Done" is old news.
        let a = t.status(&busy(status(), "c1", "Fix the login bug"), DONE, 3);
        assert_eq!(clears(&a), ["c1"]);
    }

    #[test]
    fn done_waits_for_the_last_turn_and_skips_interruptions() {
        let mut t = Tracker::new("Studio");
        t.status(&busy(status(), "c1", "Goal"), DONE, 0);
        // A queued message (or a goal) starts the next turn by itself.
        let mut between = ended("c1", "done");
        between.continuing.push("c1".into());
        assert!(alerts(&t.status(&between, DONE, 1)).is_empty());
        t.status(&busy(status(), "c1", "Goal"), DONE, 2);
        assert!(alerts(&t.status(&ended("c1", "interrupted"), DONE, 3)).is_empty());
        t.status(&busy(status(), "c1", "Goal"), DONE, 4);
        let a = t.status(&ended("c1", "error"), DONE, 5);
        assert_eq!(alerts(&a)[0].kind, AlertKind::Failed);
        assert_eq!(alerts(&a)[0].title, "Agent error");
    }

    #[test]
    fn a_desktop_without_chats_status_still_alerts() {
        let list = serde_json::json!({
            "metas": [
                { "id": "c1", "title": "Checkout" },
                { "id": "c2", "title": "Dates", "queued": [{ "id": "m", "text": "next" }] },
                { "id": "c3", "title": "Idle" },
            ],
            "running": ["c1", "c2", "c3"], "busy": ["c1", "c2"], "waiting": { "c1": "approval" },
        });
        let s = Status::from_list(&list);
        assert!(s.legacy);
        assert_eq!(s.continuing, ["c2"]);
        assert!(!s.titles.contains_key("c3"));
        let mut t = Tracker::new("Studio");
        let a = t.status(&s, DONE, 0);
        let shown = alerts(&a);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].body, "Checkout: It's waiting for your approval");
        assert_eq!(shown[0].request_id, None, "no request id: no buttons, the tap opens the chat");
        // Its turn ending isn't reported: an older desktop's chat that stops being busy is done
        // (unless its next turn starts by itself).
        let a = t.status(&Status { legacy: true, continuing: vec!["c2".into()], ..Status::default() }, DONE, 1);
        assert_eq!(alerts(&a).iter().map(|a| (a.chat_id.as_str(), a.kind)).collect::<Vec<_>>(), [("c1", AlertKind::Done)]);
    }

    #[test]
    fn watches_while_a_chat_runs_and_lets_go_once_idle() {
        let mut t = Tracker::new("Studio");
        assert!(t.status(&status(), NEEDS, 0).is_empty(), "nothing running: no service");
        assert!(!t.watching());
        let a = t.status(&busy(status(), "c1", "Refactor"), NEEDS, 0);
        assert_eq!(a, [Action::Watch(Watch { title: "Following chats on Studio".into(), text: "1 chat working".into(), lost: false })]);
        let a = t.status(&waiting(busy(status(), "c2", "Docs"), "c1", "approval", "r1"), NEEDS, 1_000);
        assert!(a.contains(&Action::Watch(Watch {
            title: "Following chats on Studio".into(),
            text: "1 chat needs you, 1 chat working".into(),
            lost: false
        })));
        // All idle: the service stays a little while (the next turn may start), then goes.
        t.status(&status(), NEEDS, 10_000);
        assert!(t.watching());
        assert!(t.tick(10_000 + IDLE_GRACE_MS - 1).is_empty());
        assert_eq!(t.tick(10_000 + IDLE_GRACE_MS), [Action::Release]);
        assert!(!t.watching());
        assert!(t.tick(10_000 + 2 * IDLE_GRACE_MS).is_empty(), "released once");
    }

    #[test]
    fn a_chat_starting_within_the_grace_keeps_the_service() {
        let mut t = Tracker::new("Studio");
        t.status(&busy(status(), "c1", "Refactor"), NEEDS, 0);
        t.status(&status(), NEEDS, 1_000);
        t.status(&busy(status(), "c2", "Next"), NEEDS, 2_000);
        assert!(t.tick(1_000 + IDLE_GRACE_MS).is_empty());
        assert!(t.watching());
    }

    #[test]
    fn a_lost_connection_says_so_then_gives_up() {
        let mut t = Tracker::new("Studio");
        assert!(t.lost(0).is_empty(), "not watching: nothing was promised");
        t.status(&busy(status(), "c1", "Refactor"), NEEDS, 0);
        let a = t.lost(5_000);
        assert!(matches!(&a[..], [Action::Watch(w)] if w.lost && w.title == "Can't reach Studio"));
        assert!(t.lost(6_000).is_empty(), "said once");
        // Back in time: the indicator says so again.
        let a = t.status(&busy(status(), "c1", "Refactor"), NEEDS, 7_000);
        assert!(matches!(&a[..], [Action::Watch(w)] if !w.lost));
        // Gone for good: stop, and leave a note rather than silence.
        t.lost(10_000);
        assert!(t.tick(10_000 + GIVE_UP_MS - 1).is_empty());
        let a = t.tick(10_000 + GIVE_UP_MS);
        assert_eq!(a[0], Action::Release);
        assert!(matches!(&a[1], Action::Note(Some(n)) if n.contains("Studio") && n.contains("Open Moonfish")));
        // Reconnected later: the note goes.
        let a = t.status(&busy(status(), "c1", "Refactor"), NEEDS, 20 * GIVE_UP_MS);
        assert_eq!(a[0], Action::Note(None));
    }

    #[test]
    fn a_chat_that_finished_while_the_link_was_down_is_still_done() {
        let mut t = Tracker::new("Studio");
        t.status(&busy(status(), "c1", "Refactor"), DONE, 0);
        t.lost(1);
        let a = t.status(&ended("c1", "done"), DONE, 2);
        assert_eq!(alerts(&a)[0].kind, AlertKind::Done);
    }

    #[test]
    fn an_answer_from_the_notification_shows_progress_and_failure() {
        let mut t = Tracker::new("Studio");
        t.status(&waiting(status(), "c1", "approval", "r1"), NEEDS, 0);
        let Some(Action::Alert(a)) = t.answering("c1", "r1", true) else { panic!("shows that it's answering") };
        assert_eq!((a.request_id, a.loud), (None, false));
        assert!(a.body.ends_with("Allowing…"));
        let Some(Action::Alert(a)) = t.answer_failed("c1", "r1", "Not connected to the desktop.") else { panic!() };
        assert_eq!(a.request_id.as_deref(), Some("r1"), "the buttons come back");
        assert!(a.detail.unwrap().starts_with("Couldn't send your answer: Not connected"));
        // Another request by now: an old answer's news isn't shown over it.
        t.status(&waiting(status(), "c1", "approval", "r2"), NEEDS, 1);
        assert_eq!(t.answer_failed("c1", "r1", "late"), None);
        assert_eq!(t.answering("c9", "r1", false), None);
    }

    #[test]
    fn opening_the_chat_clears_it_without_alerting_again() {
        let mut t = Tracker::new("Studio");
        let s = waiting(status(), "c1", "approval", "r1");
        t.status(&s, NEEDS, 0);
        assert_eq!(t.seen("c1"), [Action::Clear("c1".into())]);
        assert!(t.status(&s, NEEDS, 1).is_empty(), "the same request isn't news");
        assert!(t.seen("other").is_empty());
    }

    #[test]
    fn reset_clears_everything_and_releases() {
        let mut t = Tracker::new("Studio");
        t.status(&waiting(busy(status(), "c2", "Docs"), "c1", "approval", "r1"), NEEDS, 0);
        let a = t.reset();
        assert_eq!(a, [Action::Clear("c1".into()), Action::Release]);
        assert!(!t.watching());
        assert!(t.reset().is_empty());
        // Following again starts from nothing: the same request alerts again.
        assert_eq!(alerts(&t.status(&waiting(status(), "c1", "approval", "r1"), NEEDS, 1)).len(), 1);
    }

    #[test]
    fn a_deleted_waiting_chat_loses_its_alert() {
        let mut t = Tracker::new("Studio");
        t.status(&waiting(status(), "c1", "approval", "r1"), NEEDS, 0);
        // Gone from the status entirely (deleted, or its agent stopped).
        assert_eq!(clears(&t.status(&status(), NEEDS, 1)), ["c1"]);
    }

    #[test]
    fn status_reads_from_the_wire() {
        let s: Status = serde_json::from_value(serde_json::json!({
            "busy": ["c1"],
            "waiting": { "c1": { "kind": "approval", "requestId": "r1", "title": "Run a command", "detail": "ls" } },
            "titles": { "c1": "Hi" },
        }))
        .unwrap();
        assert_eq!(s.waiting["c1"].request_id.as_deref(), Some("r1"));
        assert!(!s.legacy && s.ended.is_empty() && s.continuing.is_empty());
    }
}
