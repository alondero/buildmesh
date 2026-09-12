//! Best-effort native turn ordering and outstanding question tracking.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, LazyLock};

#[derive(Default)]
pub(crate) struct HookState {
    turn: Option<String>,
    questions: HashMap<String, bool>,
    permission_requests: HashSet<String>,
    completed_tasks: VecDeque<String>,
    /// Whether the foreground harness turn is still executing. Background
    /// callbacks may arrive after the foreground turn has yielded; keeping
    /// this bit separate from `questions` prevents those callbacks from
    /// publishing a false `Ready` while a model is still generating.
    is_turn_active: bool,
}

impl HookState {
    pub(crate) fn accepts(&mut self, turn: Option<&str>, starts_turn: bool) -> bool {
        if starts_turn {
            self.turn = turn.map(str::to_owned);
            self.questions.retain(|_, background| *background);
            self.permission_requests.retain(|key| {
                self.questions
                    .get(key)
                    .is_some_and(|background| *background)
            });
            self.is_turn_active = true;
            return true;
        }
        match (self.turn.as_deref(), turn) {
            (Some(current), Some(incoming)) => current == incoming,
            _ => true,
        }
    }

    pub(crate) fn question(&mut self, key: &str, resolved: bool) {
        if resolved {
            self.questions.remove(key);
            self.permission_requests.remove(key);
        } else {
            self.questions.insert(key.into(), false);
            self.permission_requests.remove(key);
        }
    }

    pub(crate) fn permission_request(&mut self, key: &str) {
        self.questions.insert(key.into(), false);
        self.permission_requests.insert(key.into());
    }

    /// Resolve the matching foreground request. A few harness versions omit
    /// the request id on the reply; only fall back to the sole foreground
    /// request so multiple outstanding prompts are never resolved by guess.
    pub(crate) fn resolve_question(&mut self, key: Option<&str>) {
        if let Some(key) = key.filter(|key| !key.is_empty()) {
            if self.questions.remove(key).is_some() {
                self.permission_requests.remove(key);
            }
            // An identified reply for a different request cannot resolve the
            // sole outstanding question by guess. Only callbacks that truly
            // omit an identifier take the conservative single-request path.
            return;
        }
        let mut foreground = self
            .questions
            .iter()
            .filter(|(_, background)| !**background)
            .map(|(key, _)| key.clone());
        let Some(key) = foreground.next() else {
            return;
        };
        if foreground.next().is_none() {
            self.questions.remove(&key);
            self.permission_requests.remove(&key);
        }
    }

    pub(crate) fn background_question(&mut self, key: &str, task: &str) {
        self.questions.remove(key);
        self.permission_requests.remove(key);
        if !self
            .completed_tasks
            .iter()
            .any(|completed| completed == task)
        {
            self.questions.insert(task.into(), true);
        }
    }

    pub(crate) fn finish_background_task(&mut self, task: &str) -> bool {
        let pending = self.questions.remove(task).is_some();
        self.permission_requests.remove(task);
        // Native callbacks are asynchronous: completion can precede the
        // tool result that tells us which task owns the question.
        if self.completed_tasks.len() == 128 {
            self.completed_tasks.pop_front();
        }
        self.completed_tasks.push_back(task.into());
        pending
    }

    pub(crate) fn has_questions(&self) -> bool {
        !self.questions.is_empty()
    }

    pub(crate) fn end_turn(&mut self) {
        self.is_turn_active = false;
    }

    /// Codex has no permission-result hook. Once its Stop callback arrives,
    /// an unresolved permission request is necessarily from the turn that has
    /// just ended; clear only permission entries, never ordinary questions.
    pub(crate) fn clear_permission_requests(&mut self) {
        for key in self.permission_requests.drain() {
            self.questions.remove(&key);
        }
    }

    pub(crate) fn mark_turn_active(&mut self) {
        self.is_turn_active = true;
    }

    pub(crate) fn is_turn_active(&self) -> bool {
        self.is_turn_active
    }
}

static HOOK_STATES: LazyLock<parking_lot::Mutex<HashMap<i64, Arc<parking_lot::Mutex<HookState>>>>> =
    LazyLock::new(Default::default);

pub(crate) fn for_node(node_id: i64) -> Arc<parking_lot::Mutex<HookState>> {
    HOOK_STATES.lock().entry(node_id).or_default().clone()
}

pub(crate) fn forget(node_id: i64) {
    HOOK_STATES.lock().remove(&node_id);
}

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

    #[test]
    fn foreground_activity_is_independent_of_detached_questions() {
        let mut state = HookState::default();
        assert!(!state.is_turn_active());
        assert!(state.accepts(Some("turn"), true));
        assert!(state.is_turn_active());
        state.question("request", false);
        state.end_turn();
        assert!(!state.is_turn_active());
        assert!(state.has_questions());
        state.mark_turn_active();
        assert!(state.is_turn_active());
    }

    #[test]
    fn an_unidentified_reply_resolves_only_one_foreground_request() {
        let mut state = HookState::default();
        state.question("one", false);
        state.resolve_question(None);
        assert!(!state.has_questions());
        state.question("one", false);
        state.question("two", false);
        state.resolve_question(None);
        assert!(state.has_questions());
    }

    #[test]
    fn an_identified_reply_for_another_request_never_guesses() {
        let mut state = HookState::default();
        state.question("one", false);
        state.resolve_question(Some("other"));
        assert!(state.has_questions());
        state.resolve_question(Some("one"));
        assert!(!state.has_questions());
    }

    #[test]
    fn clearing_permissions_does_not_clear_regular_questions() {
        let mut state = HookState::default();
        state.permission_request("approval");
        state.question("question", false);
        state.clear_permission_requests();
        assert!(state.has_questions());
        state.resolve_question(Some("question"));
        assert!(!state.has_questions());
    }
}
