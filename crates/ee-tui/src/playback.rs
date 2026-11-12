//! Transient "play" / scrub — a time-cursor playback over a cached transient
//! solution (`CP-UX-M7.1`).
//!
//! Where the [`Overlay`](crate::Overlay) paints one *static* operating point onto
//! the schematic, playback turns a whole transient **run** into something you can
//! scrub: the app caches the probed-node voltage time series, holds a *playhead*
//! (a sample index), and the user can play, pause, step, and seek it. The scope
//! and the live overlay then reflect the **sampled instant** at the playhead
//! rather than the DC op-point.
//!
//! Like the rest of `ee-tui` this is a pure, testable state machine with **no
//! dependency on `ee-sim`**: the frames arrive as data — the `Vec<Signal>` a
//! transient `run` already emits, named `v(n<net>)` — and nothing here touches a
//! terminal. The run loop drives [`advance`](Playback::advance) on its frame
//! timeout, reusing the same animation-tick plumbing the current-flow overlay
//! uses (`CP-UX-M6.1`), so "playing" simply marches the playhead one sample per
//! frame and stops (clamped) at the end.
//!
//! **Sampling is nearest-sample, not interpolated:** the playhead is a sample
//! *index* into the stored time axis, and the voltages at the playhead are exactly
//! the transient samples at that index. This keeps the scrub bit-exact against the
//! cached solution (no synthetic in-between values) and makes the gate assertions
//! a direct equality rather than a tolerance.
//!
//! The playhead's per-net voltages are produced by reducing each stored series to
//! a single-sample [`Signal`] at the cursor and feeding it through
//! [`NodeVoltages::from_signals`] — the **exact same** `v(n<net>)` annotation path
//! a DC op-point uses — so the overlay cannot drift between the two sources.

use crate::overlay::NodeVoltages;
use ee_core::signal::{Domain, Signal};

/// A scrubbable, cached transient result with a playhead.
///
/// Build it from a transient run's signals with [`from_signals`](Playback::from_signals);
/// drive the playhead with [`play`](Playback::play)/[`pause`](Playback::pause),
/// [`step`](Playback::step), [`seek`](Playback::seek), and [`advance`](Playback::advance);
/// and read the sampled instant with [`voltages`](Playback::voltages) /
/// [`samples`](Playback::samples). Empty by default (no transient loaded yet).
#[derive(Debug, Clone, Default)]
pub struct Playback {
    /// The cached transient signals — real, time-domain only — sharing a common
    /// time axis (the probed-node voltage series a transient `run` produced).
    signals: Vec<Signal>,
    /// The playhead: a sample index in `0..len()` (clamped; 0 when empty).
    cursor: usize,
    /// Whether the playhead advances one sample per [`advance`](Playback::advance).
    playing: bool,
}

impl Playback {
    /// An empty playback — nothing loaded, playhead at 0, paused.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a transient run's signals into a fresh, paused playback at sample 0.
    ///
    /// Only real, non-empty, time-domain signals are kept (AC/complex results are
    /// ignored); the playhead spans the **shortest** common length, so signals of
    /// differing length never index out of bounds.
    pub fn from_signals(signals: &[Signal]) -> Self {
        let signals = signals
            .iter()
            .filter(|s| s.domain == Domain::Time && s.real().is_some() && !s.is_empty())
            .cloned()
            .collect();
        Self {
            signals,
            cursor: 0,
            playing: false,
        }
    }

    /// Whether no playable transient is loaded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The loaded transient series (for re-plotting the scope at the live pane
    /// size). Real, time-domain only — the same signals the playhead scrubs.
    pub fn signals(&self) -> &[Signal] {
        &self.signals
    }

    /// The number of playable frames — the shortest common sample count, so every
    /// loaded series has a value at every playhead position.
    pub fn len(&self) -> usize {
        self.signals.iter().map(Signal::len).min().unwrap_or(0)
    }

    /// The current playhead sample index (always `< len()` when non-empty).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether the playhead is currently advancing.
    pub fn playing(&self) -> bool {
        self.playing
    }

    /// The playhead's time in seconds (the time-axis sample at the cursor); `0.0`
    /// when empty.
    pub fn time(&self) -> f64 {
        self.signals
            .first()
            .and_then(|s| s.x.get(self.cursor).copied())
            .unwrap_or(0.0)
    }

    /// Start advancing the playhead (no-op on an empty playback).
    pub fn play(&mut self) {
        if !self.is_empty() {
            self.playing = true;
        }
    }

    /// Stop advancing the playhead, leaving it where it is.
    pub fn pause(&mut self) {
        self.playing = false;
    }

    /// Toggle between playing and paused.
    pub fn toggle(&mut self) {
        if self.playing {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Seek the playhead to an absolute sample index, clamped to the last frame.
    pub fn seek(&mut self, index: usize) {
        self.cursor = self.clamp(index);
    }

    /// Step the playhead by `delta` samples (negative rewinds), clamped at both
    /// ends so it never runs off either edge of the cached run.
    pub fn step(&mut self, delta: isize) {
        let next = (self.cursor as isize + delta).max(0) as usize;
        self.seek(next);
    }

    /// Advance one frame when playing; clamp at — and stop at — the final frame.
    ///
    /// The run loop calls this on its frame timeout (the same cadence as the
    /// current-flow animation tick). A paused or empty playback is a no-op, and
    /// reaching the last sample pauses so the playhead rests on the end.
    pub fn advance(&mut self) {
        if !self.playing || self.is_empty() {
            return;
        }
        let last = self.len() - 1;
        if self.cursor >= last {
            self.cursor = last;
            self.playing = false;
        } else {
            self.cursor += 1;
        }
    }

    /// The per-net voltages at the playhead, as a [`NodeVoltages`].
    ///
    /// Each loaded series is reduced to a single-sample [`Signal`] at the cursor
    /// and run through [`NodeVoltages::from_signals`], so the playhead overlay is
    /// annotated by the exact same `v(n<net>)` path as a DC op-point — the
    /// instantaneous transient voltages, not the operating point.
    pub fn voltages(&self) -> NodeVoltages {
        let snapshot: Vec<Signal> = self
            .signals
            .iter()
            .filter_map(|s| {
                let v = s.real()?.get(self.cursor).copied()?;
                let t = s.x.get(self.cursor).copied()?;
                Some(Signal::time(s.name.clone(), s.unit, vec![t], vec![v]))
            })
            .collect();
        NodeVoltages::from_signals(&snapshot)
    }

    /// The sampled value of the signal named `name` at the playhead, if loaded.
    pub fn sample(&self, name: &str) -> Option<f64> {
        self.signals
            .iter()
            .find(|s| s.name == name)
            .and_then(Signal::real)
            .and_then(|y| y.get(self.cursor).copied())
    }

    /// Every loaded signal's `(name, value)` at the playhead — the scope's data at
    /// the cursor, in the order the signals were probed.
    pub fn samples(&self) -> Vec<(String, f64)> {
        self.signals
            .iter()
            .filter_map(|s| {
                let v = s.real()?.get(self.cursor).copied()?;
                Some((s.name.clone(), v))
            })
            .collect()
    }

    /// Clamp an index into the valid playhead range (`0` on an empty playback).
    fn clamp(&self, index: usize) -> usize {
        match self.len() {
            0 => 0,
            n => index.min(n - 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee_core::graph::NetId;
    use ee_core::units::Unit;

    /// A two-node transient ramp: `v(n1)` rises 0→3 V and `v(n2)` rises 0→1.5 V
    /// over four samples at 0, 1, 2, 3 ms. Nearest-sample scrubbing should read
    /// these back exactly at each cursor.
    fn ramp() -> Vec<Signal> {
        let t = vec![0.0, 1e-3, 2e-3, 3e-3];
        vec![
            Signal::time("v(n1)", Unit::Volt, t.clone(), vec![0.0, 1.0, 2.0, 3.0]),
            Signal::time("v(n2)", Unit::Volt, t, vec![0.0, 0.5, 1.0, 1.5]),
        ]
    }

    #[test]
    fn empty_playback_is_a_safe_no_op() {
        let mut p = Playback::new();
        assert!(p.is_empty());
        assert_eq!(p.len(), 0);
        p.play();
        assert!(!p.playing(), "play on an empty playback does nothing");
        p.seek(10);
        p.step(5);
        p.advance();
        assert_eq!(p.cursor(), 0, "the playhead stays pinned at 0");
        assert_eq!(p.time(), 0.0);
        assert!(p.sample("v(n1)").is_none());
    }

    #[test]
    fn from_signals_keeps_time_domain_reals_and_spans_the_shortest() {
        // A complex (AC) signal is dropped; a shorter real signal caps the length.
        let mut sigs = ramp();
        sigs.push(Signal::freq(
            "v(ac)",
            Unit::Volt,
            vec![1.0, 2.0],
            vec![ee_core::signal::Complex::new(1.0, 0.0); 2],
        ));
        sigs.push(Signal::time(
            "v(n3)",
            Unit::Volt,
            vec![0.0, 1e-3],
            vec![9.0, 9.0],
        ));
        let p = Playback::from_signals(&sigs);
        assert_eq!(
            p.len(),
            2,
            "the shortest real series (2 samples) caps the span"
        );
        assert!(p.sample("v(ac)").is_none(), "the AC signal was dropped");
    }

    #[test]
    fn step_and_seek_clamp_at_both_ends() {
        let mut p = Playback::from_signals(&ramp());
        assert_eq!(p.len(), 4);
        p.step(-1);
        assert_eq!(p.cursor(), 0, "stepping back from 0 clamps");
        p.seek(100);
        assert_eq!(p.cursor(), 3, "seek past the end clamps to the last frame");
        p.step(10);
        assert_eq!(p.cursor(), 3, "stepping past the end clamps");
        p.step(-2);
        assert_eq!(p.cursor(), 1);
    }

    #[test]
    fn advance_marches_while_playing_and_stops_at_the_end() {
        let mut p = Playback::from_signals(&ramp());
        p.advance();
        assert_eq!(p.cursor(), 0, "a paused playback does not advance");
        p.play();
        assert!(p.playing());
        for expect in [1, 2, 3] {
            p.advance();
            assert_eq!(p.cursor(), expect);
        }
        assert!(p.playing(), "still playing until the clamp");
        p.advance();
        assert_eq!(p.cursor(), 3, "clamped at the last frame");
        assert!(!p.playing(), "reaching the end pauses");
    }

    #[test]
    fn voltages_at_the_playhead_are_the_nearest_sample() {
        let mut p = Playback::from_signals(&ramp());
        for cursor in 0..p.len() {
            p.seek(cursor);
            let v = p.voltages();
            let n1 = v.get(NetId(1)).unwrap();
            let n2 = v.get(NetId(2)).unwrap();
            assert!((n1 - cursor as f64).abs() < 1e-12, "v(n1) at {cursor}");
            assert!(
                (n2 - cursor as f64 * 0.5).abs() < 1e-12,
                "v(n2) at {cursor}"
            );
            assert_eq!(p.time(), cursor as f64 * 1e-3, "playhead time");
        }
    }
}
