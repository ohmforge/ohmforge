//! Xtensa device variants: the per-chip parameters that specialise the shared
//! [`Xtensa`](super::core::Xtensa) core into a specific part.
//!
//! Like the AVR [`AvrVariant`](crate::avr::AvrVariant), an [`XtensaVariant`] is a
//! small `Copy` descriptor carrying the numbers that differ between parts — the
//! RAM budget mapped from address 0, the number of GPIO pins the chip bonds out,
//! and the default CPU clock. The decoder, register file and step loop are shared
//! verbatim.
//!
//! # Modelled subset
//!
//! Only the ESP32 (Xtensa LX6) is described here. The second LX6 core, the PRO/APP
//! dual-core topology, the RAM/flash cache geometry and the per-pin IO-MUX are not
//! modelled — the descriptor carries just what the core sizes its flat memory from
//! and what the [`Esp32`](super::board::Esp32) board reports.

use super::periph::PIN_COUNT;

/// The per-chip parameters that specialise the shared Xtensa core into a part.
///
/// Construct one with [`esp32`](XtensaVariant::esp32), then build a core with
/// [`Xtensa::with_variant`](super::core::Xtensa::with_variant).
#[derive(Debug, Clone, Copy)]
pub struct XtensaVariant {
    /// The part name, e.g. `"ESP32"`.
    pub name: &'static str,
    /// Flat RAM mapped from address 0, in bytes (sizes the core's memory for
    /// hand-assembled programs and firmware images).
    pub ram_bytes: usize,
    /// Number of GPIO pins the part bonds out (the modelled low bank).
    pub gpio_count: usize,
    /// Default CPU clock in Hz.
    pub f_cpu: f64,
}

impl XtensaVariant {
    /// The **ESP32** (Xtensa LX6): modelled with 256 KiB of flat RAM, the
    /// `GPIO0..GPIO31` low bank, and a 240 MHz default clock.
    pub const fn esp32() -> Self {
        Self {
            name: "ESP32",
            ram_bytes: 256 * 1024,
            gpio_count: PIN_COUNT,
            f_cpu: 240_000_000.0,
        }
    }

    /// Modelled GPIO pin count.
    pub const fn pin_count(&self) -> usize {
        self.gpio_count
    }
}

impl Default for XtensaVariant {
    fn default() -> Self {
        Self::esp32()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esp32_reports_its_parameters() {
        let v = XtensaVariant::esp32();
        assert_eq!(v.name, "ESP32");
        assert_eq!(v.ram_bytes, 256 * 1024);
        assert_eq!(v.pin_count(), PIN_COUNT);
        assert_eq!(v.f_cpu, 240_000_000.0);
    }
}
