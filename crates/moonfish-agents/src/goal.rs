//! Goal continuation for drivers without a native goal API.

use crate::event::{ChatEvent, NoticeLevel};
use std::collections::HashMap;

pub const MAX_ROUNDS: u32 = 20;
pub const PROMPT_PREFIX: &str = "moonfish-goal-";

pub fn is_marker(line: &str) -> bool {
    let marker = line.trim_end().strip_prefix("MOONFISH_GOAL_COMPLETE_").or_else(|| line.trim_end().strip_prefix("MOONFISH_GOAL_BLOCKED_"));
    marker.is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub fn prompt(objective: &str, completion: &str, round: u32) -> String {
    let request = if round == 0 { "Work on this goal" } else { "Continue this goal from the work already done" };
    let blocked = completion.replacen("MOONFISH_GOAL_COMPLETE_", "MOONFISH_GOAL_BLOCKED_", 1);
    format!(
        "{request}:\n{objective}\n\nWork within the current permission settings. Verify the objective against concrete evidence before stopping. If it is complete, give a short verification summary and finish with the exact line `{completion}`. If you need input, use the question tool when available. If you cannot continue without user input, explain why and finish with the exact line `{blocked}`. If more work can be done, say what remains and leave out the completion line."
    )
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Complete,
    Pause(&'static str),
}

#[derive(Default)]
pub struct Progress {
    started: bool,
    answers: HashMap<String, String>,
    asked: bool,
}

impl Progress {
    pub fn observe(&mut self, event: &ChatEvent, completion: &str) -> Option<Outcome> {
        match event {
            ChatEvent::TurnStarted { .. } => self.started = true,
            ChatEvent::Text { item_id, text } if self.started => {
                self.answers.insert(item_id.clone(), text.clone());
            }
            ChatEvent::TextDelta { item_id, text } if self.started => {
                self.answers.entry(item_id.clone()).or_default().push_str(text);
            }
            ChatEvent::Question { .. } if self.started => self.asked = true,
            ChatEvent::QuestionResolved { .. } => self.asked = false,
            ChatEvent::TurnFinished { interrupted, .. } if self.started => {
                if *interrupted {
                    return Some(Outcome::Pause("The turn was stopped."));
                }
                if self.asked {
                    return Some(Outcome::Pause("The agent needs your response."));
                }
                let blocked = completion.replacen("MOONFISH_GOAL_COMPLETE_", "MOONFISH_GOAL_BLOCKED_", 1);
                if blocked != completion && self.answers.values().any(|answer| answer.trim_end().lines().last() == Some(blocked.as_str())) {
                    return Some(Outcome::Pause("The agent needs your response."));
                }
                if self.answers.values().any(|answer| answer.trim_end().lines().last() == Some(completion)) {
                    return Some(Outcome::Complete);
                }
                return Some(Outcome::Continue);
            }
            ChatEvent::Error { .. } | ChatEvent::Exited { .. } => {
                return Some(Outcome::Pause("The agent stopped before completing the goal."))
            }
            ChatEvent::Notice { level: NoticeLevel::Warn, text }
                if text.starts_with("The agent stopped") || text.starts_with("The agent declined") =>
            {
                return Some(Outcome::Pause("The agent could not continue the turn."));
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> ChatEvent {
        ChatEvent::TurnStarted { turn_id: "t".into() }
    }
    fn finish(interrupted: bool) -> ChatEvent {
        ChatEvent::TurnFinished { turn_id: "t".into(), usage: None, cost_usd: None, duration_ms: None, interrupted }
    }

    #[test]
    fn completes_only_after_an_exact_marker_at_the_end_of_an_answer() {
        let mut progress = Progress::default();
        assert_eq!(progress.observe(&finish(false), "done-1"), None);
        progress.observe(&started(), "done-1");
        progress.observe(&ChatEvent::TextDelta { item_id: "a".into(), text: "Verified.\ndone-".into() }, "done-1");
        progress.observe(&ChatEvent::TextDelta { item_id: "a".into(), text: "1\n".into() }, "done-1");
        assert_eq!(progress.observe(&finish(false), "done-1"), Some(Outcome::Complete));
    }

    #[test]
    fn continues_after_an_unfinished_turn_and_stops_after_an_interrupt() {
        let mut progress = Progress::default();
        progress.observe(&started(), "done-1");
        assert_eq!(progress.observe(&finish(false), "done-1"), Some(Outcome::Continue));
        assert!(matches!(progress.observe(&finish(true), "done-1"), Some(Outcome::Pause(_))));
    }

    #[test]
    fn a_blocked_goal_pauses() {
        let mut progress = Progress::default();
        progress.observe(&started(), "MOONFISH_GOAL_COMPLETE_a");
        progress.observe(
            &ChatEvent::Text { item_id: "a".into(), text: "I need a choice.\nMOONFISH_GOAL_BLOCKED_a".into() },
            "MOONFISH_GOAL_COMPLETE_a",
        );
        assert_eq!(progress.observe(&finish(false), "MOONFISH_GOAL_COMPLETE_a"), Some(Outcome::Pause("The agent needs your response.")));
    }
}
