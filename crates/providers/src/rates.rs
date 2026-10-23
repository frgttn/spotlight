//! Currency exchange rates for the calculator, from ExchangeRate-API's open
//! feed (free, no key, updated daily; it asks for attribution, which the
//! calculator shows next to currency answers).
//!
//! The last response is kept in `rates.json` in the data folder, so currencies
//! work offline and at startup. It's refreshed in the background once it's
//! older than [`MAX_AGE`].

use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime},
};

use spotlight_platform_win::net;

/// Rates against the US dollar.
const URL: &str = "https://open.er-api.com/v6/latest/USD";
/// The feed updates once a day.
const MAX_AGE: Duration = Duration::from_secs(12 * 3600);
/// After a failed fetch (offline), don't try again for this long.
const RETRY_AFTER: Duration = Duration::from_secs(10 * 60);
pub const ATTRIBUTION: &str = "Rates by ExchangeRate-API";

#[derive(Default)]
struct State {
    /// Currency code → units of it per US dollar.
    per_usd: HashMap<String, f64>,
    /// When `per_usd` was fetched; `None` if never.
    fetched: Option<SystemTime>,
    last_attempt: Option<SystemTime>,
}

pub struct Rates {
    /// `None`: no data folder; rates are fetched but not kept.
    cache: Option<PathBuf>,
    state: RwLock<State>,
    fetching: AtomicBool,
}

impl Rates {
    /// Loads the cached rates from `data_dir`, if there are any.
    pub fn new(data_dir: Option<PathBuf>) -> Arc<Self> {
        let cache = data_dir.map(|dir| dir.join("rates.json"));
        let mut state = State::default();
        if let Some(path) = &cache
            && let Ok(body) = fs::read_to_string(path)
            && let Ok(per_usd) = parse(&body)
        {
            state.per_usd = per_usd;
            state.fetched = fs::metadata(path).and_then(|meta| meta.modified()).ok();
        }
        Arc::new(Self {
            cache,
            state: RwLock::new(state),
            fetching: AtomicBool::new(false),
        })
    }

    /// Fixed rates, for tests.
    #[cfg(test)]
    pub fn fixed(per_usd: &[(&str, f64)]) -> Arc<Self> {
        let state = State {
            per_usd: per_usd
                .iter()
                .map(|&(code, rate)| (code.to_owned(), rate))
                .collect(),
            fetched: Some(SystemTime::now()),
            last_attempt: None,
        };
        Arc::new(Self {
            cache: None,
            state: RwLock::new(state),
            fetching: AtomicBool::new(false),
        })
    }

    /// Units of `code` per US dollar.
    pub fn per_usd(&self, code: &str) -> Option<f64> {
        let state = self.state.read().ok()?;
        state.per_usd.get(&code.to_ascii_uppercase()).copied()
    }

    pub fn knows(&self, code: &str) -> bool {
        self.per_usd(code).is_some()
    }

    /// Fetches new rates if the ones held are old, blocking. For the
    /// catalog thread at startup.
    pub fn refresh_if_stale(&self) {
        if self.is_stale() && !self.fetching.swap(true, Ordering::AcqRel) {
            self.fetch();
            self.fetching.store(false, Ordering::Release);
        }
    }

    /// Like [`Self::refresh_if_stale`], on a thread of its own; returns at once.
    pub fn refresh_in_background(self: &Arc<Self>) {
        if self.is_stale() && !self.fetching.swap(true, Ordering::AcqRel) {
            let rates = self.clone();
            thread::spawn(move || {
                rates.fetch();
                rates.fetching.store(false, Ordering::Release);
            });
        }
    }

    fn is_stale(&self) -> bool {
        let Ok(state) = self.state.read() else {
            return false;
        };
        let older_than = |time: Option<SystemTime>, age: Duration| {
            time.is_none_or(|time| time.elapsed().unwrap_or_default() > age)
        };
        older_than(state.fetched, MAX_AGE) && older_than(state.last_attempt, RETRY_AFTER)
    }

    fn fetch(&self) {
        if let Ok(mut state) = self.state.write() {
            state.last_attempt = Some(SystemTime::now());
        }
        let fetched = net::get_text(URL).and_then(|body| {
            let per_usd = parse(&body)?;
            Ok((body, per_usd))
        });
        match fetched {
            Ok((body, per_usd)) => {
                eprintln!("[rates] {} currencies", per_usd.len());
                if let Some(path) = &self.cache
                    && let Err(err) = fs::write(path, body)
                {
                    eprintln!("[rates] could not save {}: {err}", path.display());
                }
                if let Ok(mut state) = self.state.write() {
                    state.per_usd = per_usd;
                    state.fetched = Some(SystemTime::now());
                }
            }
            Err(err) => eprintln!("[rates] could not fetch: {err}"),
        }
    }
}

/// The feed's JSON: `{"result": "success", "rates": {"USD": 1, "EUR": 0.92, …}}`.
fn parse(body: &str) -> Result<HashMap<String, f64>, String> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|err| err.to_string())?;
    if json["result"] != "success" {
        return Err(format!("the feed says {:?}", json["result"]));
    }
    let rates = json["rates"].as_object().ok_or("no rates in the feed")?;
    let per_usd: HashMap<String, f64> = rates
        .iter()
        .filter_map(|(code, rate)| Some((code.to_ascii_uppercase(), rate.as_f64()?)))
        .filter(|(_, rate)| rate.is_finite() && *rate > 0.)
        .collect();
    if per_usd.is_empty() {
        return Err("no rates in the feed".into());
    }
    Ok(per_usd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_feed() {
        let body = r#"{"result":"success","base_code":"USD",
            "rates":{"USD":1,"EUR":0.92,"UAH":41.5,"BAD":-1}}"#;
        let rates = parse(body).unwrap();
        assert_eq!(rates["EUR"], 0.92);
        assert_eq!(rates["UAH"], 41.5);
        assert!(!rates.contains_key("BAD"));
        assert!(parse(r#"{"result":"error","error-type":"x"}"#).is_err());
        assert!(parse("<html>").is_err());
    }
}
