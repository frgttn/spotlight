//! Inline calculator on `fend-core`: arithmetic, units, number bases,
//! currencies.

use std::{
    error::Error,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use fend_core::{Context, ExchangeRateFnV2, ExchangeRateFnV2Options, Interrupt};
use spotlight_core::{
    item::{AnswerCard, Command, Icon, SearchItem},
    item_id,
    provider::Provider,
};

use crate::{
    currency, glyph,
    rates::{self, Rates},
};

pub const ID: &str = "calc";

/// An answer that takes longer than this (`9^9^9`) isn't worth the keystroke.
const TIME_LIMIT: Duration = Duration::from_millis(30);

pub struct Calculator {
    rates: Arc<Rates>,
}

impl Calculator {
    pub fn new(rates: Arc<Rates>) -> Self {
        Self { rates }
    }

    /// The answer for `query`, if it reads as a calculation.
    pub fn evaluate(&self, query: &str) -> Option<String> {
        // Without a digit, fend still answers plenty ("c" is the speed of light,
        // "a" is an are), which would put noise above every app search.
        if !query.bytes().any(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let mut context = Context::new();
        context.set_exchange_rate_handler_v2(ExchangeRates(self.rates.clone()));
        // The preview variant leaves out what isn't a useful answer: errors, bare
        // units, multi-line or very long output, and input that evaluates to itself.
        let result = fend_core::evaluate_preview_with_interrupt(
            query,
            &context,
            &Deadline(Instant::now() + TIME_LIMIT),
        );
        let answer = result.get_main_result().trim();
        (!answer.is_empty()).then(|| answer.to_owned())
    }

    /// Whether `answer` is an amount of money ("38.27 EUR").
    fn is_currency(&self, answer: &str) -> bool {
        answer
            .rsplit(' ')
            .next()
            .is_some_and(|unit| unit.len() == 3 && self.rates.knows(unit))
    }

    /// Money to the cent: `approx. 455.5555555556 UAH` → `455.56 UAH`.
    /// Anything that doesn't parse as "<number> <code>" is left alone.
    fn round_money(answer: &str) -> String {
        let plain = answer.strip_prefix("approx. ").unwrap_or(answer);
        let Some((amount, code)) = plain.rsplit_once(' ') else {
            return answer.to_owned();
        };
        match amount.parse::<f64>() {
            Ok(amount) => format!("{amount:.2} {code}"),
            Err(_) => answer.to_owned(),
        }
    }
}

struct Deadline(Instant);

impl Interrupt for Deadline {
    fn should_interrupt(&self) -> bool {
        Instant::now() >= self.0
    }
}

/// fend asks for "units of this currency per base currency"; ours is the dollar.
struct ExchangeRates(Arc<Rates>);

impl ExchangeRateFnV2 for ExchangeRates {
    fn relative_to_base_currency(
        &self,
        currency: &str,
        _: &ExchangeRateFnV2Options,
    ) -> Result<f64, Box<dyn Error + Send + Sync + 'static>> {
        self.0
            .per_usd(currency)
            .ok_or_else(|| format!("no exchange rate for {currency}").into())
    }
}

impl Provider for Calculator {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Calculator"
    }

    fn description(&self) -> &'static str {
        "Answers math, units and currencies as you type. Off, it fetches no exchange rates."
    }

    /// The history command. This runs once on a background thread at
    /// startup, which is also a good moment to fetch exchange rates if old.
    fn catalog(&self) -> Vec<SearchItem> {
        self.rates.refresh_if_stale();
        vec![
            SearchItem::new(item_id(ID, "history"), "Calculator History")
                .subtitle("Calculator")
                .keywords(&["calculations", "calc history"])
                .icon(Icon::Glyph(glyph::CALCULATOR))
                .action("Open History", Command::OpenCalculatorHistory),
        ]
    }

    fn answers(&self, query: &str) -> Vec<SearchItem> {
        let query = query.trim();
        // An amount of money on its own ("5 uah"), which fend only repeats,
        // is worth as much as what in dollars (euros for dollars), as in Raycast.
        let answer = match self.amount_of(query) {
            Some((_, code)) => {
                let target = if code.eq_ignore_ascii_case("USD") {
                    "EUR"
                } else {
                    "USD"
                };
                self.evaluate(&format!("{query} to {target}"))
            }
            None => self.evaluate(query),
        };
        let Some(answer) = answer else {
            return Vec::new();
        };
        let (answer, subtitle, card) = if self.is_currency(&answer) {
            // A long-running app still gets fresh rates; this returns at once.
            self.rates.refresh_in_background();
            let answer = Self::round_money(&answer);
            let card = self.money_card(query, &answer);
            (answer, format!("Calculator · {}", rates::ATTRIBUTION), card)
        } else {
            let card = AnswerCard {
                question: pretty(query),
                question_label: "Expression".into(),
                answer: answer.clone(),
                answer_label: "Result".into(),
                note: None,
            };
            (answer, "Calculator".to_owned(), card)
        };
        vec![
            SearchItem::new(item_id(ID, "answer"), answer.clone())
                .subtitle(subtitle)
                .icon(Icon::Glyph(glyph::CALCULATOR))
                .card(card)
                .action("Copy Answer", Command::Copy(answer.clone()))
                .action(
                    "Copy Calculation",
                    Command::Copy(format!("{query} = {answer}")),
                ),
        ]
    }
}

impl Calculator {
    /// `"5 uah"` → `(5, "uah")`, when the code is a currency we have a rate for.
    fn amount_of<'a>(&self, text: &'a str) -> Option<(f64, &'a str)> {
        let text = text.trim();
        let split = text.len().checked_sub(3)?;
        let (amount, code) = (text.get(..split)?.trim(), text.get(split..)?);
        if !code.bytes().all(|byte| byte.is_ascii_alphabetic()) || !self.rates.knows(code) {
            return None;
        }
        Some((amount.replace(',', "").parse().ok()?, code))
    }

    /// The card for an answer in money, `"0.12 USD"`: what was converted on
    /// the left, named when it's an amount of one currency, and the answer
    /// with its sign and currency's name on the right.
    fn money_card(&self, query: &str, answer: &str) -> AnswerCard {
        let question = before_target(query);
        let question_label = self
            .amount_of(question)
            .and_then(|(_, code)| currency::name(code))
            .unwrap_or("Expression");
        let (amount, code) = answer.rsplit_once(' ').unwrap_or((answer, ""));
        AnswerCard {
            question: pretty(question),
            question_label: question_label.into(),
            answer: currency::with_sign(amount, code),
            answer_label: currency::name(code).unwrap_or(code).into(),
            note: self
                .rates
                .fetched()
                .map(|fetched| format!("Updated {}", ago(fetched))),
        }
    }
}

/// What is converted, without where to: `"10 eur in uah"` → `"10 eur"`.
fn before_target(query: &str) -> &str {
    let lower = query.to_ascii_lowercase();
    [" to ", " in ", " as ", "->"]
        .iter()
        .filter_map(|word| lower.rfind(word))
        .max()
        .map_or(query, |at| query[..at].trim_end())
}

/// The expression as it would be written: `40*10` → `40 × 10`, `1/3` →
/// `1 ÷ 3`. A slash between letters is a unit (`km/h`) and stays.
fn pretty(query: &str) -> String {
    let chars: Vec<char> = query.chars().collect();
    let numeric = |c: Option<&char>| c.is_some_and(|c| c.is_ascii_digit() || "().".contains(*c));
    let mut out = String::with_capacity(query.len());
    for (ix, &c) in chars.iter().enumerate() {
        let sign = match c {
            '*' => Some('×'),
            '/' if numeric(chars[..ix].iter().rev().find(|c| !c.is_whitespace()))
                && numeric(chars[ix + 1..].iter().find(|c| !c.is_whitespace())) =>
            {
                Some('÷')
            }
            _ => None,
        };
        match sign {
            Some(sign) => {
                out.push(' ');
                out.push(sign);
                out.push(' ');
            }
            None => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "less than a minute ago", "5 minutes ago", "3 hours ago", "2 days ago".
fn ago(time: SystemTime) -> String {
    let seconds = time.elapsed().unwrap_or_default().as_secs();
    let (count, unit) = match seconds {
        0..60 => return "less than a minute ago".into(),
        60..3600 => (seconds / 60, "minute"),
        3600..86400 => (seconds / 3600, "hour"),
        _ => (seconds / 86400, "day"),
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{plural} ago")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calculator() -> Calculator {
        Calculator::new(Rates::fixed(&[("USD", 1.), ("EUR", 0.9), ("UAH", 41.)]))
    }

    #[test]
    fn answers_arithmetic_and_units() {
        let calc = calculator();
        assert_eq!(calc.evaluate("1 + 2 * 3").as_deref(), Some("7"));
        assert_eq!(calc.evaluate("2^10").as_deref(), Some("1024"));
        assert_eq!(calc.evaluate("1 km to m").as_deref(), Some("1000 m"));
        assert_eq!(calc.evaluate("0xff to decimal").as_deref(), Some("255"));
    }

    #[test]
    fn converts_currencies() {
        let calc = calculator();
        let euros = calc.evaluate("100 usd to eur").unwrap();
        eprintln!("100 usd to eur = {euros}");
        assert!(euros.starts_with("90") && euros.ends_with("EUR"), "{euros}");
        let hryvnias = calc.evaluate("10 eur in uah").unwrap();
        eprintln!("10 eur in uah = {hryvnias}");
        assert!(hryvnias.contains("455.5"), "{hryvnias}");
        assert!(calc.is_currency(&hryvnias));
        assert!(!calc.is_currency("1000 m"));
    }

    #[test]
    fn money_is_rounded_to_the_cent() {
        assert_eq!(
            Calculator::round_money("approx. 455.5555555556 UAH"),
            "455.56 UAH"
        );
        assert_eq!(Calculator::round_money("90 EUR"), "90.00 EUR");
        assert_eq!(Calculator::round_money("1/3 EUR"), "1/3 EUR");
        let answers = calculator().answers("10 eur in uah");
        assert_eq!(answers[0].title, "455.56 UAH");
        assert!(
            answers[0]
                .subtitle
                .as_deref()
                .unwrap()
                .contains("ExchangeRate-API")
        );
    }

    #[test]
    fn cards_show_the_expression_and_the_result() {
        let calc = calculator();
        let card = calc.answers("40*10")[0].card.clone().unwrap();
        assert_eq!(card.question, "40 × 10");
        assert_eq!(card.question_label, "Expression");
        assert_eq!(card.answer, "400");
        assert_eq!(card.answer_label, "Result");
        assert_eq!(pretty("(1 + 2)/3"), "(1 + 2) ÷ 3");
        assert_eq!(pretty("100 km/h to m/s"), "100 km/h to m/s");
    }

    #[test]
    fn cards_name_the_currencies() {
        let calc = calculator();
        let answers = calc.answers("10 eur in uah");
        let card = answers[0].card.clone().unwrap();
        assert_eq!(card.question, "10 eur");
        assert_eq!(card.question_label, "Euro");
        assert_eq!(card.answer, "₴455.56");
        assert_eq!(card.answer_label, "Ukrainian Hryvnia");
        assert_eq!(card.note.as_deref(), Some("Updated less than a minute ago"));
    }

    #[test]
    fn an_amount_of_money_alone_is_given_in_dollars() {
        let calc = calculator();
        let answers = calc.answers("41 uah");
        assert_eq!(answers[0].title, "1.00 USD");
        let card = answers[0].card.clone().unwrap();
        assert_eq!(card.answer, "$1.00");
        assert_eq!(card.question_label, "Ukrainian Hryvnia");
        assert_eq!(calc.answers("10 usd")[0].title, "9.00 EUR");
        assert!(calc.answers("10 xyz").is_empty());
    }

    #[test]
    fn unknown_currencies_give_no_answer() {
        let calc = Calculator::new(Rates::fixed(&[("USD", 1.)]));
        assert_eq!(calc.evaluate("5 usd to eur"), None);
    }

    #[test]
    fn ignores_what_is_not_a_calculation() {
        let calc = calculator();
        for query in [
            "",
            "chrome",
            "c",
            "visual studio",
            "2024",
            "7-Zip",
            "Office 365",
        ] {
            assert_eq!(calc.evaluate(query), None, "{query:?}");
        }
    }

    #[test]
    fn gives_up_on_huge_calculations() {
        let started = Instant::now();
        let _ = calculator().evaluate("9^9^9^9");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
