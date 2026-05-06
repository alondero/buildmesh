use crate::models::Provider;

static ANSI_REGEX: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"\x1b\[[0-9;]*[A-Za-z]|\x1b\][^\x1b]*\x1b\\|\x1b\[[0-9;]*m|\x1b\[\?[0-9;]*[hl]")
            .unwrap()
    });

fn strip_ansi(raw: &str) -> String {
    ANSI_REGEX.replace_all(raw, "").to_string()
}

pub struct TurnDetector {
    prompt_regex: regex::Regex,
    prompts_seen: u32,
}

impl TurnDetector {
    pub fn new(provider: Provider) -> Self {
        let pattern = match provider {
            Provider::Anthropic | Provider::Minimax => r"(?m)^\s*❯\s*$",
            Provider::Gemini => r"(?m)^\s*>>>\s*$",
            Provider::OpenCode => r"(?m)^\s*[>$❯]\s*$",
        };

        Self {
            prompt_regex: regex::Regex::new(pattern).unwrap(),
            prompts_seen: 0,
        }
    }

    pub fn contains_prompt(&mut self, raw_data: &str) -> bool {
        let stripped = strip_ansi(raw_data);

        if self.prompt_regex.is_match(&stripped) {
            self.prompts_seen += 1;
            tracing::info!("turn_detector: prompt matched (prompts_seen={})", self.prompts_seen);
            return self.prompts_seen > 1;
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_claude_prompt_after_skip() {
        let mut det = TurnDetector::new(Provider::Anthropic);

        // First prompt (spawn) — should be skipped
        assert!(!det.contains_prompt("some output\n❯\n"));

        // Second prompt — should detect
        assert!(det.contains_prompt("more output\n❯\n"));
    }

    #[test]
    fn ignores_prompt_char_in_content() {
        let mut det = TurnDetector::new(Provider::Anthropic);
        det.prompts_seen = 1;

        // ❯ embedded in a line with other content should not match
        assert!(!det.contains_prompt("  use ❯ for prompts\n"));
    }

    #[test]
    fn detects_prompt_in_middle_of_chunk() {
        let mut det = TurnDetector::new(Provider::Anthropic);
        det.prompts_seen = 1;

        // Claude outputs prompt then status bar in same chunk
        assert!(det.contains_prompt("cost info\n❯\n?forshortcuts"));
    }

    #[test]
    fn strips_ansi_before_matching() {
        let mut det = TurnDetector::new(Provider::Anthropic);
        det.prompts_seen = 1;

        let ansi_prompt = "\x1b[?25h\x1b[1;32m❯\x1b[0m\n";
        assert!(det.contains_prompt(ansi_prompt));
    }

    #[test]
    fn gemini_triple_arrow() {
        let mut det = TurnDetector::new(Provider::Gemini);
        det.prompts_seen = 1;

        assert!(det.contains_prompt("response text\n>>>\n"));
        assert!(!det.contains_prompt("response text\n>>\n"));
    }
}
