//! The **mixed-signal boundary** between a digital pin and an analog node.
//!
//! The M2.1 co-sim bridge ([`pin`](crate::pin)) exchanged *ideal* levels: a pin
//! that drove high became a stiff 5 V rail, and a node voltage became a logic
//! level at a single fixed 2.5 V threshold. Real I/O is not ideal, and the
//! difference matters once a pin meets anything but a trivial load. This module
//! (`CP-BRD-M2.2`) refines both halves of that exchange:
//!
//! - **Input side** — [`InputThresholds`]: a pin samples a node voltage into a
//!   four-valued [`Logic`] level through configurable logic-low / logic-high
//!   thresholds with a **hysteresis band** between them (a Schmitt-trigger
//!   input). A voltage in the band holds the previous level rather than
//!   chattering, and a voltage in the band with no established history reads `X`.
//!
//! - **Output side** — [`OutputDrive`]: a driven pin is not an ideal rail but a
//!   **Thévenin source** — an open-circuit voltage (the rail it swings toward)
//!   behind a series resistance (its source/sink on-resistance). Into a load the
//!   delivered voltage therefore *sags* (driving high) or *lifts* (driving low)
//!   off the rail by the divider the drive resistance forms with the load. An
//!   optional slew rate caps how fast the open-circuit voltage may move per unit
//!   time, so a fast edge into a slow node rounds instead of stepping.
//!
//! Both pieces ride on the existing [`PinDrive`]/[`PinIo`] surface: the co-sim
//! loop reads a pin's [`PinDrive`], turns it into a [`Thevenin`] with
//! [`OutputDrive::thevenin`] to stamp onto the node (a voltage source in series
//! with a resistor), solves the circuit, then feeds the solved node voltage back
//! through [`InputThresholds::resolve`] to obtain the logic level the core reads.
//!
//! [`PinDrive`]: crate::pin::PinDrive
//! [`PinIo`]: crate::pin::PinIo

use crate::pin::PinDrive;
use ohmforge_digital::Logic;

/// Configurable digital-input thresholds with a hysteresis band.
///
/// A node voltage resolves to a [`Logic`] level by comparing it against two
/// thresholds: at or **above** [`v_ih`](InputThresholds::v_ih) it reads high, at
/// or **below** [`v_il`](InputThresholds::v_il) it reads low, and strictly
/// between the two (the hysteresis band) it **holds** whatever level the input
/// last settled at — the Schmitt-trigger behaviour that rejects noise on a slow
/// edge. A degenerate band (`v_il == v_ih`) is a single hard threshold.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputThresholds {
    /// Logic-low ceiling (`V_IL`): a voltage `<= v_il` reads [`Logic::Low`].
    pub v_il: f64,
    /// Logic-high floor (`V_IH`): a voltage `>= v_ih` reads [`Logic::High`].
    pub v_ih: f64,
}

impl InputThresholds {
    /// A single hard threshold with no hysteresis (`v_il == v_ih == threshold`).
    /// A voltage at or above `threshold` reads high, anything below reads low.
    pub fn symmetric(threshold: f64) -> Self {
        Self {
            v_il: threshold,
            v_ih: threshold,
        }
    }

    /// Explicit low/high thresholds. The two are ordered so `v_il <= v_ih` even
    /// if passed reversed, so the band is always well-formed.
    pub fn with_hysteresis(v_il: f64, v_ih: f64) -> Self {
        Self {
            v_il: v_il.min(v_ih),
            v_ih: v_il.max(v_ih),
        }
    }

    /// Generic CMOS thresholds referenced to a supply: `V_IL = 0.3·Vcc`,
    /// `V_IH = 0.7·Vcc` (a ~0.4·Vcc hysteresis band around mid-supply).
    pub fn cmos(vcc: f64) -> Self {
        Self::with_hysteresis(0.3 * vcc, 0.7 * vcc)
    }

    /// The width of the hysteresis band (`v_ih - v_il`), in volts.
    pub fn band(&self) -> f64 {
        self.v_ih - self.v_il
    }

    /// Resolve a node `volts` into a logic level, given the input's `previous`
    /// settled level for the hysteresis band.
    ///
    /// Above `v_ih` → [`Logic::High`]; below `v_il` → [`Logic::Low`]; inside the
    /// band → the `previous` level (a floating/unknown history collapses to
    /// [`Logic::X`], never a silent `0`).
    pub fn resolve(&self, volts: f64, previous: Logic) -> Logic {
        if volts >= self.v_ih {
            Logic::High
        } else if volts <= self.v_il {
            Logic::Low
        } else {
            // In the hysteresis band: hold the last settled level. A pin with no
            // established history (Z) is genuinely unknown here, not low.
            previous.sensed()
        }
    }
}

impl Default for InputThresholds {
    /// Mid-supply (2.5 V) hard threshold — the M2.1 bridge's fixed behaviour, so
    /// a core that never configures its boundary samples exactly as before.
    fn default() -> Self {
        Self::symmetric(2.5)
    }
}

/// Output drive characteristics: how strongly a driven pin sources and sinks
/// current, and how fast its output may move.
///
/// A driven pin is modelled as an ideal rail behind a series on-resistance —
/// [`r_source`](OutputDrive::r_source) when driving high (toward `vcc`),
/// [`r_sink`](OutputDrive::r_sink) when driving low (toward ground). The
/// [`thevenin`](OutputDrive::thevenin) of a [`PinDrive`] is what the co-sim loop
/// stamps onto the node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputDrive {
    /// The positive supply the pin swings toward when driving high, in volts.
    pub vcc: f64,
    /// Series resistance while driving high (source / pull-up on-resistance), Ω.
    pub r_source: f64,
    /// Series resistance while driving low (sink / pull-down on-resistance), Ω.
    pub r_sink: f64,
    /// Optional slew-rate limit in volts per second: the fastest the open-circuit
    /// output voltage may move. `None` is an instantaneous (ideal) edge.
    pub slew_v_per_s: Option<f64>,
}

impl OutputDrive {
    /// An (near-)ideal push-pull driver at `vcc`: a small symmetric 25 Ω
    /// on-resistance and instantaneous edges. The default boundary.
    pub fn ideal(vcc: f64) -> Self {
        Self {
            vcc,
            r_source: 25.0,
            r_sink: 25.0,
            slew_v_per_s: None,
        }
    }

    /// A push-pull driver with a symmetric source/sink resistance `r_on`.
    pub fn push_pull(vcc: f64, r_on: f64) -> Self {
        Self {
            vcc,
            r_source: r_on,
            r_sink: r_on,
            slew_v_per_s: None,
        }
    }

    /// Set a slew-rate limit (volts per second), returning the updated drive.
    pub fn with_slew(mut self, v_per_s: f64) -> Self {
        self.slew_v_per_s = Some(v_per_s);
        self
    }

    /// The Thévenin equivalent the given `drive` presents, or `None` for a
    /// high-impedance pin (which stamps nothing onto the node).
    ///
    /// Driving high → (`vcc`, `r_source`); driving low → (`0 V`, `r_sink`). A PWM
    /// drive uses its instantaneous level (a slow load follows the duty via the
    /// level toggling each step).
    pub fn thevenin(&self, drive: PinDrive) -> Option<Thevenin> {
        match drive.level() {
            None => None,
            Some(true) => Some(Thevenin {
                v_oc: self.vcc,
                r: self.r_source,
            }),
            Some(false) => Some(Thevenin {
                v_oc: 0.0,
                r: self.r_sink,
            }),
        }
    }

    /// Apply the slew limit to a target open-circuit voltage over an interval
    /// `dt` seconds, starting from `from` volts: the result moves toward `target`
    /// by at most `slew · dt`. With no slew limit (or non-positive `dt`) the
    /// target is reached immediately.
    pub fn slew_toward(&self, from: f64, target: f64, dt: f64) -> f64 {
        match self.slew_v_per_s {
            Some(rate) if dt > 0.0 && rate.is_finite() => {
                let max_step = rate * dt;
                let delta = target - from;
                if delta.abs() <= max_step {
                    target
                } else {
                    from + max_step.copysign(delta)
                }
            }
            _ => target,
        }
    }
}

impl Default for OutputDrive {
    /// A near-ideal 5 V push-pull driver — the M2.1 bridge's effectively stiff
    /// rail, now with a small, explicit series resistance.
    fn default() -> Self {
        Self::ideal(5.0)
    }
}

/// A Thévenin-equivalent source: an open-circuit voltage `v_oc` behind a series
/// resistance `r`. What a driven pin presents to the node it is wired to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thevenin {
    /// Open-circuit (no-load) output voltage, in volts.
    pub v_oc: f64,
    /// Series (output) resistance, in ohms.
    pub r: f64,
}

impl Thevenin {
    /// The voltage delivered into a resistive load `r_load` tied to ground — the
    /// output of the divider `v_oc · r_load / (r_load + r)`.
    ///
    /// Driving high (`v_oc = vcc`) this sits *below* the rail by the sag across
    /// the drive resistance; the stiffer the drive (smaller `r`) the closer to
    /// the rail it stays. An infinite (open) load delivers the full `v_oc`.
    pub fn delivered_into(&self, r_load: f64) -> f64 {
        if !r_load.is_finite() {
            return self.v_oc;
        }
        let denom = r_load + self.r;
        if denom <= 0.0 {
            return self.v_oc;
        }
        self.v_oc * r_load / denom
    }

    /// The current (amps) this source drives into a resistive load `r_load` to
    /// ground: `v_oc / (r_load + r)`.
    pub fn current_into(&self, r_load: f64) -> f64 {
        let denom = r_load + self.r;
        if denom <= 0.0 {
            0.0
        } else {
            self.v_oc / denom
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_threshold_resolves_above_and_below() {
        let t = InputThresholds::symmetric(2.5);
        assert_eq!(t.band(), 0.0);
        assert_eq!(t.resolve(3.3, Logic::Z), Logic::High);
        assert_eq!(t.resolve(1.0, Logic::Z), Logic::Low);
        // Exactly at the threshold reads high (>=).
        assert_eq!(t.resolve(2.5, Logic::Z), Logic::High);
    }

    #[test]
    fn hysteresis_band_holds_the_previous_level() {
        // V_IL = 2.0, V_IH = 3.0: the 1 V band between them holds state.
        let t = InputThresholds::with_hysteresis(2.0, 3.0);
        assert_eq!(t.band(), 1.0);

        // Clearly outside the band: unambiguous.
        assert_eq!(t.resolve(3.5, Logic::Low), Logic::High);
        assert_eq!(t.resolve(1.5, Logic::High), Logic::Low);

        // Inside the band a voltage holds whatever was last settled — the same
        // 2.5 V reads high if we were high and low if we were low.
        assert_eq!(t.resolve(2.5, Logic::High), Logic::High);
        assert_eq!(t.resolve(2.5, Logic::Low), Logic::Low);

        // A rising edge only trips high once it clears V_IH, and a falling edge
        // only trips low once it drops under V_IL — the Schmitt action.
        assert_eq!(t.resolve(2.9, Logic::Low), Logic::Low, "not yet past V_IH");
        assert_eq!(t.resolve(3.0, Logic::Low), Logic::High, "clears V_IH");
        assert_eq!(
            t.resolve(2.1, Logic::High),
            Logic::High,
            "not yet below V_IL"
        );
        assert_eq!(t.resolve(2.0, Logic::High), Logic::Low, "drops to V_IL");
    }

    #[test]
    fn band_with_no_history_is_unknown() {
        let t = InputThresholds::cmos(5.0); // V_IL = 1.5, V_IH = 3.5
        assert_eq!(t.v_il, 1.5);
        assert_eq!(t.v_ih, 3.5);
        // Mid-band with nothing ever driven is genuinely unknown, not 0.
        assert_eq!(t.resolve(2.5, Logic::Z), Logic::X);
    }

    #[test]
    fn reversed_thresholds_are_normalised() {
        let t = InputThresholds::with_hysteresis(3.0, 2.0);
        assert_eq!(t.v_il, 2.0);
        assert_eq!(t.v_ih, 3.0);
    }

    #[test]
    fn thevenin_maps_drive_to_source_and_series_r() {
        let d = OutputDrive::push_pull(5.0, 40.0);
        assert_eq!(d.thevenin(PinDrive::HighZ), None);
        assert_eq!(
            d.thevenin(PinDrive::Digital(true)),
            Some(Thevenin { v_oc: 5.0, r: 40.0 })
        );
        assert_eq!(
            d.thevenin(PinDrive::Digital(false)),
            Some(Thevenin { v_oc: 0.0, r: 40.0 })
        );
        // PWM drives at its instantaneous level.
        assert_eq!(
            d.thevenin(PinDrive::Pwm {
                duty: 0.5,
                level: true
            }),
            Some(Thevenin { v_oc: 5.0, r: 40.0 })
        );
    }

    #[test]
    fn driven_pin_sags_under_load_per_its_drive_resistance() {
        // A 5 V pin with 100 Ω of source resistance driving high.
        let th = OutputDrive::push_pull(5.0, 100.0)
            .thevenin(PinDrive::Digital(true))
            .unwrap();

        // Open circuit: the full rail.
        assert!((th.delivered_into(f64::INFINITY) - 5.0).abs() < 1e-9);

        // Into an equal 100 Ω load the output sits at exactly half the rail.
        assert!((th.delivered_into(100.0) - 2.5).abs() < 1e-9);

        // Into 900 Ω: 5·900/1000 = 4.5 V — a 0.5 V sag off the rail.
        assert!((th.delivered_into(900.0) - 4.5).abs() < 1e-9);

        // A stiffer drive (10 Ω) sags far less into the same 900 Ω load.
        let stiff = OutputDrive::push_pull(5.0, 10.0)
            .thevenin(PinDrive::Digital(true))
            .unwrap();
        let v_stiff = stiff.delivered_into(900.0);
        assert!(v_stiff > 4.9, "stiff drive barely sags: {v_stiff}");

        // And the delivered current matches V/(R+Rload).
        assert!((th.current_into(900.0) - 5.0 / 1000.0).abs() < 1e-12);
    }

    #[test]
    fn slew_limits_the_edge_rate() {
        // 1 V/µs = 1e6 V/s. Over 1 µs the output can move at most 1 V.
        let d = OutputDrive::ideal(5.0).with_slew(1e6);
        assert_eq!(d.slew_toward(0.0, 5.0, 1e-6), 1.0, "capped rise");
        assert_eq!(
            d.slew_toward(4.5, 5.0, 1e-6),
            5.0,
            "small step reaches target"
        );
        assert_eq!(d.slew_toward(5.0, 0.0, 1e-6), 4.0, "capped fall");
        // No slew limit → instantaneous.
        assert_eq!(OutputDrive::ideal(5.0).slew_toward(0.0, 5.0, 1e-6), 5.0);
    }
}
