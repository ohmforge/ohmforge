//! Bench power supply — a configurable DC source with a current limit
//! (`CP-UX-M1.2`).
//!
//! A lab bench PSU has two knobs: a **set voltage** and a **current limit**.
//! Into a given load it behaves as one of two sources:
//!
//! * **Constant voltage (CV):** while the load draws no more than the limit, the
//!   supply holds `v_set`; the current is `v_set / R`.
//! * **Constant current (CC):** once the load would pull more than `i_limit`,
//!   the supply caps the current at `i_limit` and the terminal voltage falls to
//!   `i_limit · R` — the classic CV/CC crossover.
//!
//! The logic is pure values; [`render`](BenchPsu::render) adds a small panel.

use crate::fmt::{fit, fmt_eng, kv_line};
use ee_core::units::Unit;

/// Which regulation region the supply is operating in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsuMode {
    /// Holding the set voltage; the load current is below the limit.
    ConstantVoltage,
    /// Holding the current limit; the terminal voltage has collapsed to `I·R`.
    ConstantCurrent,
}

impl PsuMode {
    /// A short tag for the mode.
    pub const fn tag(self) -> &'static str {
        match self {
            PsuMode::ConstantVoltage => "CV",
            PsuMode::ConstantCurrent => "CC",
        }
    }
}

/// What the supply actually delivers into a load.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PsuOutput {
    /// Terminal voltage (volts).
    pub voltage: f64,
    /// Delivered current (amperes).
    pub current: f64,
    /// Which region produced this operating point.
    pub mode: PsuMode,
}

impl PsuOutput {
    /// Delivered power, `V · I` (watts).
    pub fn power(&self) -> f64 {
        self.voltage * self.current
    }
}

/// A bench DC supply: a set voltage, a current limit and a panel width.
pub struct BenchPsu {
    title: String,
    v_set: f64,
    i_limit: f64,
    width: usize,
}

impl BenchPsu {
    /// A new supply at `0 V` / `1 A` limit, 28 cells wide.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            v_set: 0.0,
            i_limit: 1.0,
            width: 28,
        }
    }

    /// Set the output voltage (volts).
    pub fn voltage(mut self, v_set: f64) -> Self {
        self.v_set = v_set;
        self
    }

    /// Set the current limit (amperes); negatives are clamped to `0`.
    pub fn current_limit(mut self, i_limit: f64) -> Self {
        self.i_limit = i_limit.max(0.0);
        self
    }

    /// Set the panel width in character cells (minimum 16).
    pub fn size(mut self, width: usize) -> Self {
        self.width = width.max(16);
        self
    }

    /// The configured set voltage.
    pub fn set_voltage(&self) -> f64 {
        self.v_set
    }

    /// The configured current limit.
    pub fn limit(&self) -> f64 {
        self.i_limit
    }

    /// Compute the delivered V/I into a resistive `load` (ohms) and the region.
    ///
    /// A short (`load ≤ 0`) collapses the terminal voltage to `0` and delivers
    /// the current limit (CC). An open or non-finite load draws no current and
    /// sits at `v_set` (CV). Otherwise the CV current `v_set / load` is compared
    /// to the limit: below it the supply holds `v_set`, at or above it the
    /// supply clamps to `i_limit` and the voltage falls to `i_limit · load`.
    pub fn deliver(&self, load: f64) -> PsuOutput {
        // Short circuit: all of the limit current flows, no voltage develops.
        if load <= 0.0 {
            return PsuOutput {
                voltage: 0.0,
                current: self.i_limit,
                mode: PsuMode::ConstantCurrent,
            };
        }
        // Open / invalid load: no current, full set voltage.
        if !load.is_finite() {
            return PsuOutput {
                voltage: self.v_set,
                current: 0.0,
                mode: PsuMode::ConstantVoltage,
            };
        }
        let cv_current = self.v_set / load;
        if cv_current <= self.i_limit {
            PsuOutput {
                voltage: self.v_set,
                current: cv_current,
                mode: PsuMode::ConstantVoltage,
            }
        } else {
            PsuOutput {
                voltage: self.i_limit * load,
                current: self.i_limit,
                mode: PsuMode::ConstantCurrent,
            }
        }
    }

    /// Render a panel: the set point, the limit, and the delivered operating
    /// point into `load` with its CV/CC mode.
    pub fn render(&self, load: f64) -> String {
        let out_pt = self.deliver(load);
        let mut lines = String::new();
        lines.push_str(&fit(&format!("  PSU · {}", self.title), self.width));
        lines.push('\n');
        lines.push_str(&kv_line(
            &format!("  [{}]", out_pt.mode.tag()),
            &format!(
                "{} {}",
                fmt_eng(out_pt.voltage, Unit::Volt.symbol()),
                fmt_eng(out_pt.current, Unit::Ampere.symbol()),
            ),
            self.width,
        ));
        lines.push('\n');
        let set = format!(
            "  set {}  lim {}  load {}",
            fmt_eng(self.v_set, Unit::Volt.symbol()),
            fmt_eng(self.i_limit, Unit::Ampere.symbol()),
            fmt_eng(load, Unit::Ohm.symbol()),
        );
        lines.push_str(&fit(&set, self.width));
        lines.push('\n');
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_voltage_region_delivers_the_set_voltage() {
        // 5 V, 1 A limit into 100 Ω → 50 mA, well under the limit → CV at 5 V.
        let psu = BenchPsu::new("bench").voltage(5.0).current_limit(1.0);
        let o = psu.deliver(100.0);
        assert_eq!(o.mode, PsuMode::ConstantVoltage);
        assert!((o.voltage - 5.0).abs() < 1e-12, "V = {}", o.voltage);
        assert!((o.current - 0.05).abs() < 1e-12, "I = {}", o.current);
    }

    #[test]
    fn current_limit_crossover_clamps_i_and_drops_v() {
        // 5 V, 1 A limit into 1 Ω: CV would need 5 A, so it crosses to CC.
        let psu = BenchPsu::new("bench").voltage(5.0).current_limit(1.0);
        let o = psu.deliver(1.0);
        assert_eq!(o.mode, PsuMode::ConstantCurrent);
        assert!(
            (o.current - 1.0).abs() < 1e-12,
            "I clamps to limit: {}",
            o.current
        );
        // V across the 1 Ω load at 1 A is 1 V, not the 5 V set point.
        assert!((o.voltage - 1.0).abs() < 1e-12, "V = I·R = {}", o.voltage);
    }

    #[test]
    fn crossover_point_is_exactly_at_the_limit() {
        // 5 V / 5 Ω = 1 A = the limit → still CV (≤), delivering the set voltage.
        let psu = BenchPsu::new("b").voltage(5.0).current_limit(1.0);
        let at = psu.deliver(5.0);
        assert_eq!(at.mode, PsuMode::ConstantVoltage);
        assert!((at.voltage - 5.0).abs() < 1e-12);
        assert!((at.current - 1.0).abs() < 1e-12);
        // Just past it → CC.
        let past = psu.deliver(4.9);
        assert_eq!(past.mode, PsuMode::ConstantCurrent);
    }

    #[test]
    fn short_and_open_loads_do_not_panic() {
        let psu = BenchPsu::new("b").voltage(12.0).current_limit(2.0);
        // Short: 0 V, full limit current, CC.
        let short = psu.deliver(0.0);
        assert_eq!(short.mode, PsuMode::ConstantCurrent);
        assert_eq!(short.voltage, 0.0);
        assert!((short.current - 2.0).abs() < 1e-12);
        // Open: full set voltage, no current, CV.
        let open = psu.deliver(f64::INFINITY);
        assert_eq!(open.mode, PsuMode::ConstantVoltage);
        assert!((open.voltage - 12.0).abs() < 1e-12);
        assert_eq!(open.current, 0.0);
        // Power at the CV point into 100 Ω: 12 V · 120 mA = 1.44 W.
        assert!((psu.deliver(100.0).power() - 1.44).abs() < 1e-9);
    }

    #[test]
    fn render_reports_mode_and_fits_width() {
        let width = 32;
        let out = BenchPsu::new("Bench")
            .voltage(5.0)
            .current_limit(1.0)
            .size(width)
            .render(1.0);
        assert!(out.contains("[CC]"), "CC mode shown:\n{out}");
        for line in out.lines() {
            assert!(
                line.chars().count() <= width,
                "line exceeds width {width}: {line:?}"
            );
        }
    }
}
