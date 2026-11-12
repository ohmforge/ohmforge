//! The co-simulation **pin exchange** surface: the only thing an [`McuCore`] and
//! the outside world swap each step.
//!
//! The lockstep co-simulation loop lives in the `of` binary (the one place that
//! legitimately links both the MCU and the analog engines — see
//! `docs/plan/track-boards.md`). This module is the *surface* that loop drives,
//! and it is deliberately engine-agnostic: it knows nothing about MNA, nets, or
//! the digital event kernel. A core only ever:
//!
//! - **drives pins out** — per pin, a [`PinDrive`]: driving high, driving low,
//!   high-impedance (an input), or a PWM output with an effective duty and its
//!   instantaneous level; and
//! - **reads voltages in** — per pin, the sampled analog input voltage the loop
//!   obtained from the solved circuit, which the ADC and the digital-input
//!   threshold read back.
//!
//! The loop reconciles the two with the circuit graph: it pushes [`PinDrive`]s
//! onto nodes (as logic events or voltage sources) and feeds solved node
//! voltages back as the per-pin inputs.
//!
//! Pins are addressed by an opaque [`PinId`] index. The index space is defined by
//! the concrete core; for the AVR it is `port * 8 + bit` (see
//! [`avr::pin_id`](crate::avr::pin_id)), but the exchange type itself treats it as
//! a flat index so the same surface serves every core.
//!
//! [`McuCore`]: crate::McuCore

/// An opaque pin identifier: a flat index into a core's [`PinIo`] exchange.
///
/// The mapping from an index to a physical port/bit (or ADC channel) is the
/// concrete core's business; the co-sim loop only needs stable ids to associate
/// a pin with a circuit node. Obtain ids from the core (e.g.
/// [`avr::pin_id`](crate::avr::pin_id) or
/// [`Uno::digital_pin_id`](crate::avr::Uno::digital_pin_id)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PinId(u16);

impl PinId {
    /// Wrap a raw flat index as a [`PinId`].
    pub const fn new(index: u16) -> Self {
        Self(index)
    }

    /// The raw flat index.
    pub const fn index(self) -> u16 {
        self.0
    }
}

/// What a core is asserting onto a pin this step.
///
/// This is the *drive out* half of the exchange; the co-sim loop turns it into a
/// node condition (a driven logic level, a voltage source, or nothing for a
/// high-impedance input). Drive strength and rise/fall shaping are the
/// mixed-signal boundary's job (`CP-BRD-M2.2`) — here a drive is ideal.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum PinDrive {
    /// High-impedance: the pin is an input (or disconnected). The core drives
    /// nothing and only *reads* the pin's input voltage.
    #[default]
    HighZ,
    /// Driven to a fixed logic level: `true` = high (towards Vcc), `false` = low
    /// (towards ground).
    Digital(bool),
    /// Driven by a PWM output: `duty` is the fraction of the period the pin is
    /// high (`0.0..=1.0`), and `level` is the instantaneous level at the end of
    /// the step. A slow analog load follows the duty; a fast one follows the
    /// level.
    Pwm {
        /// Fraction of the PWM period the pin spends high, `0.0..=1.0`.
        duty: f64,
        /// The instantaneous output level at the end of the step.
        level: bool,
    },
}

impl PinDrive {
    /// The instantaneous logic level this drive presents, if it drives at all
    /// (`None` for [`HighZ`](PinDrive::HighZ)). PWM reports its instantaneous
    /// level.
    pub fn level(self) -> Option<bool> {
        match self {
            PinDrive::HighZ => None,
            PinDrive::Digital(v) => Some(v),
            PinDrive::Pwm { level, .. } => Some(level),
        }
    }

    /// Whether the core is actively driving the pin (i.e. not high-impedance).
    pub fn is_driving(self) -> bool {
        !matches!(self, PinDrive::HighZ)
    }
}

/// The per-step pin exchange between a core and the co-sim loop.
///
/// A fixed-width bank of pins, each carrying a [`PinDrive`] the core writes
/// **out** and an input voltage (volts) the loop writes **in**. Construct it
/// sized to a core's [`pin_count`](crate::McuCore::pin_count); the two sides then
/// address pins by [`PinId`].
///
/// Typical loop usage: set input voltages from the solved circuit, hand it to
/// [`step_to_cycle`](crate::McuCore::step_to_cycle), then read back the drives.
#[derive(Debug, Clone, Default)]
pub struct PinIo {
    /// Drive asserted by the core, per pin (out).
    drives: Vec<PinDrive>,
    /// Sampled analog input voltage, per pin, in volts (in).
    inputs: Vec<f64>,
}

impl PinIo {
    /// A fresh exchange for `pin_count` pins: every pin high-impedance, every
    /// input 0 V.
    pub fn new(pin_count: usize) -> Self {
        Self {
            drives: vec![PinDrive::HighZ; pin_count],
            inputs: vec![0.0; pin_count],
        }
    }

    /// The number of pins in the exchange.
    pub fn pin_count(&self) -> usize {
        self.drives.len()
    }

    /// The drive the core is currently asserting on `pin`.
    pub fn drive(&self, pin: PinId) -> PinDrive {
        self.drives
            .get(pin.index() as usize)
            .copied()
            .unwrap_or(PinDrive::HighZ)
    }

    /// Set the drive on `pin` (the core's side of the exchange).
    pub fn set_drive(&mut self, pin: PinId, drive: PinDrive) {
        if let Some(slot) = self.drives.get_mut(pin.index() as usize) {
            *slot = drive;
        }
    }

    /// The analog input voltage presented on `pin`, in volts.
    pub fn input(&self, pin: PinId) -> f64 {
        self.inputs
            .get(pin.index() as usize)
            .copied()
            .unwrap_or(0.0)
    }

    /// Set the analog input voltage on `pin` (the loop's side of the exchange).
    pub fn set_input(&mut self, pin: PinId, volts: f64) {
        if let Some(slot) = self.inputs.get_mut(pin.index() as usize) {
            *slot = volts;
        }
    }

    /// Iterate every pin's `(id, drive)` — handy for the loop to push the whole
    /// bank onto the circuit after a step.
    pub fn drives(&self) -> impl Iterator<Item = (PinId, PinDrive)> + '_ {
        self.drives
            .iter()
            .enumerate()
            .map(|(i, &d)| (PinId::new(i as u16), d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_bank_is_high_z_and_zero_volts() {
        let io = PinIo::new(4);
        assert_eq!(io.pin_count(), 4);
        assert_eq!(io.drive(PinId::new(2)), PinDrive::HighZ);
        assert_eq!(io.input(PinId::new(2)), 0.0);
    }

    #[test]
    fn set_and_read_both_halves() {
        let mut io = PinIo::new(3);
        io.set_drive(PinId::new(1), PinDrive::Digital(true));
        io.set_input(PinId::new(2), 3.3);
        assert_eq!(io.drive(PinId::new(1)), PinDrive::Digital(true));
        assert_eq!(io.input(PinId::new(2)), 3.3);
        // Untouched pins keep their defaults.
        assert_eq!(io.drive(PinId::new(0)), PinDrive::HighZ);
    }

    #[test]
    fn out_of_range_access_is_graceful() {
        let mut io = PinIo::new(1);
        // Reads clamp to the defaults; writes are dropped rather than panicking.
        assert_eq!(io.drive(PinId::new(9)), PinDrive::HighZ);
        assert_eq!(io.input(PinId::new(9)), 0.0);
        io.set_drive(PinId::new(9), PinDrive::Digital(true));
        io.set_input(PinId::new(9), 5.0);
        assert_eq!(io.pin_count(), 1);
    }

    #[test]
    fn pin_drive_level_and_driving_helpers() {
        assert_eq!(PinDrive::HighZ.level(), None);
        assert!(!PinDrive::HighZ.is_driving());
        assert_eq!(PinDrive::Digital(true).level(), Some(true));
        assert!(PinDrive::Digital(false).is_driving());
        assert_eq!(
            PinDrive::Pwm {
                duty: 0.25,
                level: true
            }
            .level(),
            Some(true)
        );
    }

    #[test]
    fn drives_iter_visits_every_pin() {
        let mut io = PinIo::new(3);
        io.set_drive(PinId::new(0), PinDrive::Digital(false));
        io.set_drive(PinId::new(2), PinDrive::Digital(true));
        let collected: Vec<_> = io.drives().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0], (PinId::new(0), PinDrive::Digital(false)));
        assert_eq!(collected[1], (PinId::new(1), PinDrive::HighZ));
        assert_eq!(collected[2], (PinId::new(2), PinDrive::Digital(true)));
    }
}
