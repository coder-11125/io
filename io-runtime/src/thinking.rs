//! Extended-thinking control: deciding, per turn, whether the model should
//! reason first and with how large a token budget.
//!
//! Thinking comes from two places. The `[thinking]` setting turns it on for
//! every turn at a fixed budget (toggled with `/think` or Alt+T). Independently,
//! keywords in a prompt raise it for that turn only: `think` (4K tokens),
//! `think hard` / `megathink` (10K) and `ultrathink` (32K).

use crate::config::{
    ThinkingConfig, MIN_THINKING_BUDGET, THINK_BUDGET_BASIC, THINK_BUDGET_HARD, THINK_BUDGET_MAX,
};
use regex::Regex;
use std::sync::LazyLock;

/// Never ask for more output than this in one request, whatever the budget.
const MAX_REQUEST_TOKENS: u32 = 64_000;
/// Room kept for the visible answer on top of the thinking budget.
const ANSWER_HEADROOM: u32 = 4_096;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex is valid")
}

static EMBEDDED: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?s)<file path=.*?</file>|<hook-context>.*?</hook-context>|<project-context>.*?</project-context>",
    )
});
static ULTRA: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bultrathink\b"));
static HARD: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(?:megathink|think\s+(?:very\s+)?hard(?:er)?|think\s+(?:deeply|more|longer|intensely|a\s+lot)|think\s+about\s+it)\b",
    )
});
// A bare "think" only counts as an instruction: it starts a sentence (or
// follows "please") and isn't just part of "I think …" or "what do you think?".
static BASIC: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?im)(?:^|[.!?;:]\s+|\bplease\s+)think\b\s*(?:[.!?,]|$|(?:about|through|carefully|first|step)\b)",
    )
});

/// The thinking budget requested by keywords in `prompt`, if any. Text
/// inlined by `@file` mentions and hooks is ignored, so a source file that
/// happens to contain the word `ultrathink` doesn't trigger anything.
pub fn keyword_budget(prompt: &str) -> Option<u32> {
    let typed = EMBEDDED.replace_all(prompt, "");
    if ULTRA.is_match(&typed) {
        Some(THINK_BUDGET_MAX)
    } else if HARD.is_match(&typed) {
        Some(THINK_BUDGET_HARD)
    } else if BASIC.is_match(&typed) {
        Some(THINK_BUDGET_BASIC)
    } else {
        None
    }
}

/// The thinking budget for one turn, or `None` for no extended thinking.
/// Keywords can raise a configured budget but never lower it.
pub fn budget_for_turn(config: &ThinkingConfig, prompt: &str) -> Option<u32> {
    let from_keyword = if config.keywords {
        keyword_budget(prompt)
    } else {
        None
    };
    let configured = config
        .enabled
        .then_some(config.budget_tokens.max(MIN_THINKING_BUDGET));
    match (configured, from_keyword) {
        (Some(c), Some(k)) => Some(c.max(k)),
        (c, k) => c.or(k),
    }
}

/// The `max_tokens` to request so the answer still fits after `budget` tokens
/// of reasoning. Never lowers the caller's `base`.
pub fn max_tokens_with_thinking(base: u32, budget: u32) -> u32 {
    base.max((budget + ANSWER_HEADROOM).min(MAX_REQUEST_TOKENS))
}

/// Parse the argument of `/think`: `on`, `off`, a preset or a token count.
/// Returns the new `(enabled, budget)`; `None` budget keeps the current one.
pub fn parse_setting(arg: &str) -> Result<(bool, Option<u32>), String> {
    let arg = arg.trim().to_ascii_lowercase();
    match arg.as_str() {
        "on" | "true" | "yes" => Ok((true, None)),
        "off" | "false" | "no" | "0" => Ok((false, None)),
        "low" | "basic" => Ok((true, Some(THINK_BUDGET_BASIC))),
        "medium" | "hard" => Ok((true, Some(THINK_BUDGET_HARD))),
        "high" | "max" | "ultra" => Ok((true, Some(THINK_BUDGET_MAX))),
        other => {
            let digits = other.trim_end_matches('k');
            let scale = if other.ends_with('k') { 1000 } else { 1 };
            match digits.parse::<u32>() {
                Ok(n) if n.saturating_mul(scale) >= MIN_THINKING_BUDGET => Ok((
                    true,
                    Some(
                        n.saturating_mul(scale)
                            .min(MAX_REQUEST_TOKENS - ANSWER_HEADROOM),
                    ),
                )),
                Ok(_) => Err(format!(
                    "the thinking budget must be at least {MIN_THINKING_BUDGET} tokens"
                )),
                Err(_) => Err(format!(
                    "unknown setting '{other}' — use on, off, low, medium, high or a token count"
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_map_to_budget_tiers() {
        assert_eq!(
            keyword_budget("ultrathink about the design"),
            Some(THINK_BUDGET_MAX)
        );
        assert_eq!(keyword_budget("Please ULTRATHINK."), Some(THINK_BUDGET_MAX));
        for hard in [
            "think hard about this",
            "think harder",
            "megathink please",
            "think deeply before you answer",
            "think more",
            "think about it",
            "think very hard",
        ] {
            assert_eq!(keyword_budget(hard), Some(THINK_BUDGET_HARD), "{hard}");
        }
        // Ultra beats hard when both appear.
        assert_eq!(
            keyword_budget("think hard, actually ultrathink"),
            Some(THINK_BUDGET_MAX)
        );
    }

    #[test]
    fn a_bare_think_only_counts_as_an_instruction() {
        for yes in [
            "think",
            "Think about how to restructure this",
            "refactor the parser. Think carefully first",
            "please think through the edge cases",
            "fix the bug\nthink step by step",
        ] {
            assert_eq!(keyword_budget(yes), Some(THINK_BUDGET_BASIC), "{yes}");
        }
        for no in [
            "I think this is fine",
            "what do you think?",
            "do you think we should refactor",
            "thinking about lunch",
            "rethink the approach",
            "thinker.rs needs a fix",
            "",
        ] {
            assert_eq!(keyword_budget(no), None, "{no}");
        }
    }

    #[test]
    fn inlined_files_and_hook_context_are_not_scanned() {
        let prompt = "summarize @src/x.rs\n\n<file path=\"src/x.rs\">\n// ultrathink mode\n</file>";
        assert_eq!(keyword_budget(prompt), None);
        let hooked = "do it\n\n<hook-context>\nthink hard\n</hook-context>";
        assert_eq!(keyword_budget(hooked), None);
        // But the typed part still counts.
        let typed =
            "ultrathink: summarize @src/x.rs\n\n<file path=\"src/x.rs\">\nfn main() {}\n</file>";
        assert_eq!(keyword_budget(typed), Some(THINK_BUDGET_MAX));
    }

    #[test]
    fn turn_budget_combines_the_setting_and_keywords() {
        let mut cfg = ThinkingConfig::default();
        // Off + no keyword: nothing.
        assert_eq!(budget_for_turn(&cfg, "hello"), None);
        // Off, but a keyword switches it on for the turn.
        assert_eq!(budget_for_turn(&cfg, "think hard"), Some(THINK_BUDGET_HARD));
        // Keywords disabled: ignored.
        cfg.keywords = false;
        assert_eq!(budget_for_turn(&cfg, "ultrathink"), None);
        // On: the configured budget applies…
        cfg.enabled = true;
        cfg.budget_tokens = 8_000;
        assert_eq!(budget_for_turn(&cfg, "hello"), Some(8_000));
        // …a keyword can raise it but never lower it.
        cfg.keywords = true;
        assert_eq!(budget_for_turn(&cfg, "ultrathink"), Some(THINK_BUDGET_MAX));
        assert_eq!(budget_for_turn(&cfg, "think"), Some(8_000));
        // A tiny configured budget is raised to the API minimum.
        cfg.budget_tokens = 10;
        assert_eq!(budget_for_turn(&cfg, "hello"), Some(MIN_THINKING_BUDGET));
    }

    #[test]
    fn max_tokens_leaves_room_for_the_answer() {
        assert_eq!(max_tokens_with_thinking(16_384, 4_000), 16_384);
        assert_eq!(max_tokens_with_thinking(16_384, 31_999), 31_999 + 4_096);
        assert_eq!(max_tokens_with_thinking(1_024, 4_000), 8_096);
        // Capped, and never below the caller's own base.
        assert_eq!(
            max_tokens_with_thinking(16_384, 100_000),
            MAX_REQUEST_TOKENS
        );
        assert_eq!(max_tokens_with_thinking(100_000, 4_000), 100_000);
    }

    #[test]
    fn think_command_arguments_parse() {
        assert_eq!(parse_setting("on"), Ok((true, None)));
        assert_eq!(parse_setting(" OFF "), Ok((false, None)));
        assert_eq!(parse_setting("low"), Ok((true, Some(THINK_BUDGET_BASIC))));
        assert_eq!(parse_setting("medium"), Ok((true, Some(THINK_BUDGET_HARD))));
        assert_eq!(parse_setting("high"), Ok((true, Some(THINK_BUDGET_MAX))));
        assert_eq!(parse_setting("12000"), Ok((true, Some(12_000))));
        assert_eq!(parse_setting("16k"), Ok((true, Some(16_000))));
        assert!(parse_setting("500").unwrap_err().contains("at least"));
        assert!(parse_setting("banana")
            .unwrap_err()
            .contains("unknown setting"));
        // Absurd values are capped to what a request can carry.
        assert_eq!(
            parse_setting("999999"),
            Ok((true, Some(MAX_REQUEST_TOKENS - ANSWER_HEADROOM)))
        );
    }
}
