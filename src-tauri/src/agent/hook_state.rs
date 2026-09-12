//! Best-effort native turn ordering and outstanding question tracking.
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock};

#[derive(Default)]
pub(crate) struct HookState {
    turn: Option<String>,
    questions: HashMap<String, bool>,
    completed_tasks: VecDeque<String>,
}

impl HookState {
    pub(crate) fn accepts(&mut self, turn: Option<&str>, starts_turn: bool) -> bool {
        if starts_turn {
            self.turn = turn.map(str::to_owned);
            self.questions.retain(|_, background| *background);
            return true;
        }
        match (self.turn.as_deref(), turn) {
            (Some(current), Some(incoming)) => current == incoming,
            _ => true,
        }
    }

    pub(crate) fn question(&mut self, key: &str, resolved: bool) {
        if resolved { self.questions.remove(key); } else { self.questions.insert(key.into(), false); }
    }

    pub(crate) fn background_question(&mut self, key: &str, task: &str) {
        self.questions.remove(key);
        if !self.completed_tasks.iter().any(|completed| completed == task) {
            self.questions.insert(task.into(), true);
        }
    }

    pub(crate) fn finish_background_task(&mut self, task: &str) -> bool {
        let pending = self.questions.remove(task).is_some();
        // Native callbacks are asynchronous: completion can precede the
        // tool result that tells us which task owns the question.
        if self.completed_tasks.len() == 128 { self.completed_tasks.pop_front(); }
        self.completed_tasks.push_back(task.into());
        pending
    }

    pub(crate) fn has_questions(&self) -> bool { !self.questions.is_empty() }
}

static HOOK_STATES: LazyLock<parking_lot::Mutex<HashMap<i64, Arc<parking_lot::Mutex<HookState>>>>> =
    LazyLock::new(Default::default);

pub(crate) fn for_node(node_id: i64) -> Arc<parking_lot::Mutex<HookState>> {
    HOOK_STATES.lock().entry(node_id).or_default().clone()
}

pub(crate) fn forget(node_id: i64) { HOOK_STATES.lock().remove(&node_id); }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_turn_cannot_finish_a_new_prompt_but_missing_tokens_are_accepted() {
        let mut state = HookState::default();
        assert!(state.accepts(Some("old"), true));
        assert!(state.accepts(Some("new"), true));
        assert!(!state.accepts(Some("old"), false));
        assert!(state.accepts(Some("new"), false));
        assert!(state.accepts(None, false));
    }

    #[test]
    fn background_stop_cannot_hide_outstanding_questions() {
        let mut state = HookState::default();
        state.question("first", false);
        state.question("second", false);
        assert!(state.has_questions());
        state.question("first", true);
        assert!(state.has_questions());
        state.question("second", true);
        assert!(!state.has_questions());
        state.question("old", false);
        state.accepts(Some("next"), true);
        assert!(!state.has_questions());
    }

    #[test]
    fn detached_questions_survive_new_prompts_and_out_of_order_task_results() {
        let mut state = HookState::default();
        state.question("call", false);
        state.background_question("call", "task");
        state.accepts(Some("next"), true);
        assert!(state.has_questions());
        assert!(state.finish_background_task("task"));
        assert!(!state.has_questions());
        state.question("early-call", false);
        assert!(!state.finish_background_task("early-task"));
        state.background_question("early-call", "early-task");
        assert!(!state.has_questions());
    }

    #[test]
    fn state_owner_is_shared_per_node_but_isolated_between_nodes() {
        let first = for_node(9_900_001);
        let same = for_node(9_900_001);
        let other = for_node(9_900_002);
        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &other));
        first.lock().question("request", false);
        assert!(same.lock().has_questions());
        assert!(!other.lock().has_questions());
        forget(9_900_001);
        forget(9_900_002);
    }
}
