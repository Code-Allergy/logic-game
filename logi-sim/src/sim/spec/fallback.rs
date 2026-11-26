//! The fallback from specialization to supersteps ([`FallbackOptions`]):
//! a keyed `Sim` with a superstep measures what its runs cost per tick
//! with keys (the per-tick path through prefixes, lookups and bodies) and
//! without (the superstep of the general fused bodies), and runs the
//! cheaper one. Both give the same state at every `run` boundary, so the
//! choice changes speed only.
//!
//! The policy is a state machine over the measured runs (pure: it is fed
//! ticks and nanoseconds, and says which mode the next `run` takes):
//!
//! - **Spec**: keyed runs, measured per `window` ticks. A window that
//!   costs more than the baseline (the superstep's cost per tick) by more
//!   than `margin` percent, or every `resample` windows, or with no
//!   baseline yet, is followed by a **Sample**: `sample` ticks of
//!   supersteps, the baseline measured afresh right after the window it
//!   is compared with (the machine's load drifts). `patience` such
//!   confirmed losing windows in a row fall back; a sample that confirms
//!   a winning window halves `probe` (down to `probe_min`).
//! - **Fallen**: supersteps, until `probe` ticks have passed (the baseline
//!   is measured meanwhile, over all of them); then a **Probe**: one
//!   window of keyed runs. If it costs less than the baseline by `margin`
//!   percent, specialization is back on; otherwise `probe` doubles, up to
//!   `probe_max`, and the runtime falls back again.
//!
//! Hysteresis: falling back takes `patience` windows over `1 + margin`
//! times the baseline, coming back a probe under `1 / (1 + margin)`
//! times; in between the runtime stays where it is. A probe that came
//! back by chance soon falls back again, and the probe interval it had
//! grown to only shrinks as winning windows are confirmed, so a noisy
//! near-tie does not turn into flapping. Keys are not counted while the
//! superstep runs (no hot keys are found, no body ages), so a probe is
//! also when bodies for the keys of the moment are requested.

// Without the JIT there is no superstep to fall back to: only the
// options and the stats are used.
#![cfg_attr(not(feature = "jit"), allow(dead_code))]

#[cfg(test)]
thread_local! {
    /// Tests: 0 lets the policy decide; 1 alternates the mode at every
    /// `run` that may use either (`min_run` ignored); 2 always takes the
    /// superstep. The policy is still fed (and ignores runs in a mode it
    /// did not ask for).
    pub(crate) static FORCE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    /// Tests: instead of the wall clock, a `run` costs this many
    /// nanoseconds per superstep tick, per hit and per miss of a keyed
    /// tick (a keyed run's other ticks cost as superstep ticks).
    pub(crate) static VIRTUAL: std::cell::Cell<Option<(u64, u64, u64)>> =
        const { std::cell::Cell::new(None) };
}

/// When a keyed `Sim` runs supersteps instead ([`super::SpecOptions::fallback`]).
/// Ticks are those of whole `run` calls: a mode holds for a whole call, and
/// a window ends at the first call that completes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackOptions {
    /// Keyed ticks per measurement.
    pub window: u64,
    /// Superstep ticks per baseline sample.
    pub sample: u64,
    /// Keyed windows between two baseline samples while specialization
    /// pays.
    pub resample: u32,
    /// Percent by which a keyed window may cost more than the baseline
    /// before it counts as losing, and by which a probe must cost less to
    /// come back.
    pub margin: u32,
    /// Losing windows in a row (each confirmed by a fresh sample) that
    /// fall back.
    pub patience: u32,
    /// Superstep ticks before the first probe after a fallback; doubled
    /// after each probe that loses, up to `probe_max`.
    pub probe_min: u64,
    pub probe_max: u64,
    /// Calls of `run` with fewer ticks are neither measured nor switched
    /// (they cannot use the superstep much, and a clock read is not free).
    pub min_run: u64,
}

impl Default for FallbackOptions {
    fn default() -> Self {
        FallbackOptions {
            window: 1 << 20,
            sample: 1 << 18,
            resample: 8,
            margin: 5,
            patience: 2,
            probe_min: 1 << 22,
            probe_max: 1 << 26,
            min_run: 256,
        }
    }
}

/// Which path a `run` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Keyed ticks (`Sim::keyed_ticks`).
    Spec,
    /// The superstep of the general fused bodies.
    Super,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Keyed; windows since the last sample.
    Spec { windows: u32 },
    /// A baseline sample after a keyed window.
    Sample,
    /// Fallen back: supersteps until a probe.
    Fallen,
    /// A keyed window after a fallback.
    Probe,
}

/// The policy's state ([`FallbackOptions`]).
#[derive(Debug, Clone)]
pub(crate) struct Fallback {
    o: FallbackOptions,
    state: State,
    /// The measurement under way: ticks and nanoseconds.
    ticks: u64,
    nanos: u64,
    /// Nanoseconds per tick of the superstep (last sample), and of the
    /// last keyed window.
    baseline: Option<f64>,
    keyed: f64,
    /// Confirmed losing windows in a row.
    losing: u32,
    /// Superstep ticks before the next probe.
    probe: u64,
    pub fallbacks: u64,
    pub probes: u64,
    pub super_ticks: u64,
}

impl Fallback {
    pub fn new(o: &FallbackOptions) -> Self {
        let o = FallbackOptions {
            window: o.window.max(1),
            sample: o.sample.max(1),
            probe_min: o.probe_min.max(1),
            probe_max: o.probe_max.max(o.probe_min.max(1)),
            ..o.clone()
        };
        Fallback {
            probe: o.probe_min,
            state: State::Spec { windows: 0 },
            ticks: 0,
            nanos: 0,
            baseline: None,
            keyed: 0.0,
            losing: 0,
            fallbacks: 0,
            probes: 0,
            super_ticks: 0,
            o,
        }
    }

    /// The mode of the next `run` of `ticks` ticks (`None`: too short to
    /// measure; it runs keyed).
    #[inline]
    pub fn mode(&self, ticks: u64) -> Option<Mode> {
        (ticks >= self.o.min_run).then_some(self.current())
    }

    /// The mode the policy is in.
    pub fn current(&self) -> Mode {
        match self.state {
            State::Spec { .. } | State::Probe => Mode::Spec,
            State::Sample | State::Fallen => Mode::Super,
        }
    }

    /// Whether the policy is fallen back (not sampling).
    pub fn fallen(&self) -> bool {
        self.state == State::Fallen
    }

    /// A `run` of `ticks` ticks in `mode` (as [`Fallback::mode`] said)
    /// took `nanos`.
    pub fn record(&mut self, mode: Mode, ticks: u64, nanos: u64) {
        if mode == Mode::Super {
            self.super_ticks += ticks;
        }
        if mode != self.current() {
            return;
        }
        self.ticks += ticks;
        self.nanos += nanos;
        let need = match self.state {
            State::Spec { .. } | State::Probe => self.o.window,
            State::Sample => self.o.sample,
            State::Fallen => self.probe,
        };
        if self.ticks < need {
            return;
        }
        let cost = self.nanos as f64 / self.ticks as f64;
        (self.ticks, self.nanos) = (0, 0);
        let margin = 1.0 + f64::from(self.o.margin) / 100.0;
        self.state = match self.state {
            State::Spec { windows } => {
                self.keyed = cost;
                let windows = windows + 1;
                match self.baseline {
                    Some(b) if windows < self.o.resample && cost <= b * margin => {
                        self.losing = 0;
                        State::Spec { windows }
                    }
                    _ => State::Sample,
                }
            }
            State::Sample => {
                self.baseline = Some(cost);
                if self.keyed > cost * margin {
                    self.losing += 1;
                } else {
                    self.losing = 0;
                    self.probe = (self.probe / 2).max(self.o.probe_min);
                }
                if self.losing >= self.o.patience.max(1) {
                    self.losing = 0;
                    self.fallbacks += 1;
                    State::Fallen
                } else {
                    State::Spec { windows: 0 }
                }
            }
            State::Fallen => {
                self.baseline = Some(cost);
                self.probes += 1;
                State::Probe
            }
            State::Probe => {
                self.keyed = cost;
                if self.baseline.is_some_and(|b| cost * margin <= b) {
                    State::Spec { windows: 0 }
                } else {
                    self.probe = self.probe.saturating_mul(2).min(self.o.probe_max);
                    self.fallbacks += 1;
                    State::Fallen
                }
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> FallbackOptions {
        FallbackOptions {
            window: 100,
            sample: 50,
            resample: 4,
            margin: 5,
            patience: 2,
            probe_min: 1000,
            probe_max: 4000,
            min_run: 10,
        }
    }

    /// Feed runs of `n` ticks in whatever mode the policy asks for,
    /// costing `spec` and `sup` nanoseconds per tick, until `ticks` have
    /// run; the ticks run in each mode.
    fn drive(f: &mut Fallback, n: u64, ticks: u64, spec: u64, sup: u64) -> (u64, u64) {
        let (mut s, mut p) = (0, 0);
        let mut done = 0;
        while done < ticks {
            let m = f.mode(n).expect("measured");
            let c = if m == Mode::Spec { spec } else { sup };
            f.record(m, n, n * c);
            if m == Mode::Spec {
                s += n;
            } else {
                p += n;
            }
            done += n;
        }
        (s, p)
    }

    /// The fallbacks after the first: one per probe that ended (a probe
    /// under way has not).
    fn fallbacks_after_probes(f: &Fallback) -> u64 {
        match f.state {
            State::Probe => f.probes,
            _ => 1 + f.probes,
        }
    }

    /// Specialization that pays stays on: only the samples run
    /// supersteps (one per `resample` windows).
    #[test]
    fn paying_specialization_stays_on() {
        let mut f = Fallback::new(&opts());
        let (s, p) = drive(&mut f, 10, 100_000, 8, 10);
        assert_eq!(f.fallbacks, 0);
        assert!(!f.fallen());
        // A sample of 50 per 4 windows of 100 (the first window too).
        assert!(
            p * 8 <= s + 1000 && p * 10 >= s,
            "{s} keyed, {p} supersteps"
        );
    }

    /// Specialization that loses falls back after `patience` confirmed
    /// windows, then probes with a growing interval, up to the maximum.
    #[test]
    fn losing_specialization_falls_back_and_probes_rarely() {
        let mut f = Fallback::new(&opts());
        // Window, sample (1 losing), window, sample (2): fallen.
        drive(&mut f, 10, 300, 12, 10);
        assert_eq!(f.fallbacks, 1);
        assert!(f.fallen());
        let (s, p) = drive(&mut f, 10, 100_000, 12, 10);
        // Probes after 1000, 2000, 4000, 4000, ... superstep ticks.
        assert!(f.probes >= 20 && f.probes <= 30, "{} probes", f.probes);
        assert_eq!(f.fallbacks, fallbacks_after_probes(&f));
        assert!(s <= 100 * f.probes + 100 && p > 90_000, "{s} keyed, {p}");
    }

    /// One losing window followed by a winning one does not fall back
    /// (`patience`); a margin within 5% never does.
    #[test]
    fn hysteresis_on_the_way_down() {
        let mut f = Fallback::new(&opts());
        // First window, sample: losing.
        drive(&mut f, 10, 150, 12, 10);
        assert_eq!((f.losing, f.fallbacks), (1, 0));
        // A winning window: reset, and no sample needed.
        drive(&mut f, 10, 100, 9, 10);
        assert_eq!((f.losing, f.fallbacks), (0, 0));
        assert_eq!(f.current(), Mode::Spec);
        // 4% worse: never falls back.
        drive(&mut f, 10, 100_000, 104, 100);
        assert_eq!(f.fallbacks, 0);
        assert!(!f.fallen());
    }

    /// Fallen back, a probe within the margin either way does not come
    /// back; one cheaper by more than the margin does, the probe interval
    /// shrinking back to its minimum only as samples confirm the keys.
    #[test]
    fn hysteresis_on_the_way_up() {
        let mut f = Fallback::new(&opts());
        drive(&mut f, 10, 300, 12, 10);
        assert!(f.fallen());
        drive(&mut f, 10, 20_000, 104, 100);
        let probes = f.probes;
        assert!(probes > 3);
        assert_eq!(f.fallbacks, fallbacks_after_probes(&f));
        assert_eq!(f.probe, 4000);
        // 4% cheaper: still not back.
        drive(&mut f, 10, 20_000, 96, 100);
        assert!(f.probes > probes);
        assert_eq!(f.fallbacks, fallbacks_after_probes(&f));
        // Finish a probe under way, then run until the next one, which
        // runs 5% cheaper: back.
        while f.current() == Mode::Spec {
            f.record(Mode::Spec, 10, 960);
        }
        assert!(f.fallen());
        while f.current() == Mode::Super {
            f.record(Mode::Super, 10, 1000);
        }
        f.record(Mode::Spec, 100, 9500);
        assert_eq!(f.current(), Mode::Spec);
        assert!(!f.fallen());
        assert_eq!(f.probe, 4000);
        // And stays while it pays, the interval halving with each
        // confirmed window (one sample per 4 windows).
        let falls = f.fallbacks;
        drive(&mut f, 10, 450, 90, 100);
        assert_eq!(f.probe, 2000);
        drive(&mut f, 10, 10_000, 90, 100);
        assert_eq!(f.fallbacks, falls);
        assert_eq!(f.probe, 1000);
    }

    /// Runs shorter than `min_run` are not measured and run keyed; a run
    /// in another mode than asked is ignored.
    #[test]
    fn short_and_foreign_runs_are_not_measured() {
        let mut f = Fallback::new(&opts());
        assert_eq!(f.mode(9), None);
        assert_eq!(f.mode(10), Some(Mode::Spec));
        f.record(Mode::Super, 1000, 1);
        assert_eq!(f.ticks, 0);
        f.record(Mode::Spec, 99, 99);
        assert_eq!(f.current(), Mode::Spec);
        f.record(Mode::Spec, 1, 1);
        // No baseline yet: a sample.
        assert_eq!(f.current(), Mode::Super);
    }
}
