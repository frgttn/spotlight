//! Measurements shared by the spikes: process memory, paint-latency probe, percentiles.

use std::{
    mem::size_of,
    time::{Duration, Instant},
};

use windows::Win32::System::{
    ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
    Threading::GetCurrentProcess,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct Memory {
    /// Physical memory currently mapped into the process (what Task Manager's details tab shows).
    pub working_set_mb: f64,
    /// Committed private memory (Task Manager's "Memory" column is close to this).
    pub private_mb: f64,
}

pub fn memory() -> Memory {
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    if !ok.as_bool() {
        return Memory::default();
    }
    const MB: f64 = 1024.0 * 1024.0;
    Memory {
        working_set_mb: counters.WorkingSetSize as f64 / MB,
        private_mb: counters.PagefileUsage as f64 / MB,
    }
}

pub fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.
}

#[derive(Clone, Copy, Debug)]
pub enum Probe {
    FirstShow,
    Show,
    Key,
}

/// Latency from a trigger to the frame that shows it. The UI calls [`PaintProbe::arm`]
/// when the change happens and [`PaintProbe::painted`] once the frame has been
/// rendered but not yet presented (in GPUI: a canvas painted last in the tree).
/// Only Present and DWM composition come after.
#[derive(Default)]
pub struct PaintProbe {
    pending: Option<(Instant, Probe)>,
    pub first_show_ms: Option<f64>,
    pub last_show_ms: Option<f64>,
    pub key_ms: Option<f64>,
    /// Warm shows only; the first show is reported separately.
    pub show_samples: Vec<f64>,
    pub key_samples: Vec<f64>,
}

impl PaintProbe {
    pub fn arm(&mut self, started: Instant, probe: Probe) {
        self.pending = Some((started, probe));
    }

    pub fn painted(&mut self) {
        let Some((started, probe)) = self.pending.take() else {
            return;
        };
        let elapsed = ms(started.elapsed());
        match probe {
            Probe::FirstShow => {
                eprintln!("[spike] hotkey -> painted: {elapsed:.2} ms (first show)");
                self.first_show_ms = Some(elapsed);
            }
            Probe::Show => {
                eprintln!("[spike] hotkey -> painted: {elapsed:.2} ms");
                self.last_show_ms = Some(elapsed);
                self.show_samples.push(elapsed);
            }
            Probe::Key => {
                self.key_ms = Some(elapsed);
                self.key_samples.push(elapsed);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Percentiles {
    pub n: usize,
    pub median: f64,
    pub p95: f64,
    pub max: f64,
}

impl Percentiles {
    pub fn of(samples: &[f64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
        Self {
            n: sorted.len(),
            median: at(0.5),
            p95: at(0.95),
            max: *sorted.last().unwrap(),
        }
    }
}

/// What a self-test run prints, identical for both spikes.
pub struct Summary {
    pub apps: usize,
    pub items: usize,
    pub discover_ms: Option<f64>,
    pub first_show_ms: Option<f64>,
    pub show: Percentiles,
    pub filter: Percentiles,
    pub key_to_paint: Percentiles,
}

impl Summary {
    pub fn print(&self, startup: Memory, idle: Memory) {
        let row = |name: &str, p: Percentiles| {
            eprintln!(
                "  {name:<26} n={:<3} median {:>6.2} ms   p95 {:>6.2} ms   max {:>6.2} ms",
                p.n, p.median, p.p95, p.max
            )
        };
        eprintln!("[selftest] ---------------------------------------------------------------");
        eprintln!(
            "  items                      {} ({} apps), discovery {:.1} ms",
            self.items,
            self.apps,
            self.discover_ms.unwrap_or(f64::NAN)
        );
        eprintln!(
            "  first show -> painted      {:.2} ms",
            self.first_show_ms.unwrap_or(f64::NAN)
        );
        row("warm show -> painted", self.show);
        row("filter per keystroke", self.filter);
        row("keystroke -> painted", self.key_to_paint);
        eprintln!(
            "  RAM after startup          ws {:.1} MB, private {:.1} MB",
            startup.working_set_mb, startup.private_mb
        );
        eprintln!(
            "  RAM idle, hidden (5 s)     ws {:.1} MB, private {:.1} MB",
            idle.working_set_mb, idle.private_mb
        );
        eprintln!("[selftest] ---------------------------------------------------------------");
    }
}

/// Self-test script shared by both spikes, so the numbers are comparable.
pub mod selftest {
    pub const SHOW_CYCLES: usize = 15;
    pub const SHOWN_MS: u64 = 250;
    pub const HIDDEN_MS: u64 = 150;
    pub const QUERY: &str = "visual studio code";
    pub const KEY_INTERVAL_MS: u64 = 80;
    pub const SETTLE_MS: u64 = 300;
    pub const IDLE_MS: u64 = 5_000;
}
