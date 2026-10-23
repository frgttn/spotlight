//! Inline calculator on `fend-core`: arithmetic, units, number bases.

use std::time::{Duration, Instant};

use fend_core::{Context, Interrupt};
use spotlight_core::{
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
};

use crate::glyph;

pub const ID: &str = "calc";

/// An answer that takes longer than this (`9^9^9`) isn't worth the keystroke.
const TIME_LIMIT: Duration = Duration::from_millis(30);

pub struct Calculator;

struct Deadline(Instant);

impl Interrupt for Deadline {
    fn should_interrupt(&self) -> bool {
        Instant::now() >= self.0
    }
}

/// The answer for `query`, if it reads as a calculation.
pub fn evaluate(query: &str) -> Option<String> {
    // Without a digit, fend still answers plenty ("c" is the speed of light,
    // "a" is an are), which would put noise above every app search.
    if !query.bytes().any(|byte| byte.is_ascii_digit()) {
        return None;
    }
    // The preview variant leaves out what isn't a useful answer: errors, bare
    // units, multi-line or very long output, and input that evaluates to itself.
    let result = fend_core::evaluate_preview_with_interrupt(
        query,
        &Context::new(),
        &Deadline(Instant::now() + TIME_LIMIT),
    );
    let answer = result.get_main_result().trim();
    (!answer.is_empty()).then(|| answer.to_owned())
}

impl Provider for Calculator {
    fn id(&self) -> &'static str {
        ID
    }

    fn answers(&self, query: &str) -> Vec<SearchItem> {
        let Some(answer) = evaluate(query) else {
            return Vec::new();
        };
        vec![
            SearchItem::new(item_id(ID, "answer"), answer.clone())
                .subtitle("Calculator")
                .icon(Icon::Glyph(glyph::CALCULATOR))
                .action("Copy Answer", Command::Copy(answer.clone()))
                .action(
                    "Copy Calculation",
                    Command::Copy(format!("{} = {answer}", query.trim())),
                ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_arithmetic_and_units() {
        assert_eq!(evaluate("1 + 2 * 3").as_deref(), Some("7"));
        assert_eq!(evaluate("2^10").as_deref(), Some("1024"));
        assert_eq!(evaluate("1 km to m").as_deref(), Some("1000 m"));
        assert_eq!(evaluate("0xff to decimal").as_deref(), Some("255"));
    }

    #[test]
    fn ignores_what_is_not_a_calculation() {
        for query in [
            "",
            "chrome",
            "c",
            "visual studio",
            "2024",
            "7-Zip",
            "Office 365",
        ] {
            assert_eq!(evaluate(query), None, "{query:?}");
        }
    }

    #[test]
    fn gives_up_on_huge_calculations() {
        let started = Instant::now();
        let _ = evaluate("9^9^9^9");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
