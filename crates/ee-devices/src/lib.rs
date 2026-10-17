//! # ee-devices
//!
//! Concrete device models and their stamping/behavior, plus **board
//! definitions** (Arduino Uno/Nano/Mega, Raspberry Pi Pico, ESP32-class, and
//! user-defined custom boards) and a searchable **parts library**.
//!
//! A board is a manifest: MCU, pinout, and onboard parts. Custom boards drop in
//! by writing a manifest — see docs/FEATURES.md §3. (Roadmap M1–M3.)

/// A board the user can `board add`.
#[derive(Debug, Clone)]
pub struct BoardDef {
    pub model: String,
    /// e.g. "atmega328p".
    pub mcu: String,
    /// Named pins → the MCU port bit they map to, as `"<pin>=<port><bit>"`
    /// (e.g. `"D13=PB5"`). The runnable core + pin resolution lives in
    /// `ee-mcu`'s `avr::board::Uno`; this manifest is the catalog entry.
    pub pins: Vec<String>,
}

impl BoardDef {
    /// The Arduino Uno (ATmega328P) manifest: digital D0..D13, analog A0..A5,
    /// and the on-board LED on D13 (PB5). The port/bit strings match the pin map
    /// in `ee-mcu`'s `avr::board::Uno`.
    pub fn uno() -> Self {
        let mut pins = Vec::new();
        // D0..D7 → PORTD 0..7 ; D8..D13 → PORTB 0..5.
        for d in 0u8..=7 {
            pins.push(format!("D{d}=PD{d}"));
        }
        for d in 8u8..=13 {
            pins.push(format!("D{d}=PB{}", d - 8));
        }
        // A0..A5 → PORTC 0..5.
        for a in 0u8..=5 {
            pins.push(format!("A{a}=PC{a}"));
        }
        Self {
            model: "uno".to_string(),
            mcu: "atmega328p".to_string(),
            pins,
        }
    }

    /// The Arduino Mega 2560 (ATmega2560) manifest: digital D0..D53, analog
    /// A0..A15, and the on-board LED on D13 (PB7). The runnable core + pin
    /// resolution lives in `ee-mcu`'s `avr::board::Mega`; this manifest is the
    /// catalog entry. The ATmega2560 is the same AVR ISA as the Uno's 328P with
    /// a larger memory map and more ports/timers.
    pub fn mega() -> Self {
        let mut pins = Vec::new();
        // The PORTB header pins the emulator models: D10..D13 → PB4..PB7, with
        // the on-board LED on D13 (PB7). The full Mega header spans more ports
        // (see `ee-mcu`'s `avr::board::Mega`); the catalog records the LED and
        // the modelled PORTB pins.
        for d in 10u8..=13 {
            pins.push(format!("D{d}=PB{}", d - 6));
        }
        pins.push("LED=PB7".to_string());
        Self {
            model: "mega".to_string(),
            mcu: "atmega2560".to_string(),
            pins,
        }
    }

    /// The Raspberry Pi Pico (RP2040 / Cortex-M0+) manifest: GP0..GP28 mapped to
    /// the SIO GPIO bits (the GP number is the GPIO number), plus the on-board
    /// LED on GP25. The runnable core + pin resolution lives in `ee-mcu`'s
    /// `cortexm::board::Pico`; this manifest is the catalog entry.
    pub fn pico() -> Self {
        let mut pins = Vec::new();
        // GP0..GP28 → GPIO0..GPIO28 (the GP number is the GPIO number).
        for gp in 0u8..=28 {
            pins.push(format!("GP{gp}=GPIO{gp}"));
        }
        // The on-board LED (GP25) under its friendly name, too.
        pins.push("LED=GPIO25".to_string());
        Self {
            model: "pico".to_string(),
            mcu: "rp2040".to_string(),
            pins,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BoardDef;

    #[test]
    fn uno_manifest_maps_the_led_pin() {
        let uno = BoardDef::uno();
        assert_eq!(uno.mcu, "atmega328p");
        assert!(uno.pins.contains(&"D13=PB5".to_string()));
        assert!(uno.pins.contains(&"A0=PC0".to_string()));
        assert_eq!(uno.pins.len(), 14 + 6);
    }

    #[test]
    fn mega_manifest_maps_the_led_pin() {
        let mega = BoardDef::mega();
        assert_eq!(mega.mcu, "atmega2560");
        assert!(mega.pins.contains(&"D13=PB7".to_string()));
        assert!(mega.pins.contains(&"LED=PB7".to_string()));
        assert!(mega.pins.contains(&"D10=PB4".to_string()));
    }

    #[test]
    fn pico_manifest_maps_the_led_pin() {
        let pico = BoardDef::pico();
        assert_eq!(pico.mcu, "rp2040");
        assert!(pico.pins.contains(&"GP25=GPIO25".to_string()));
        assert!(pico.pins.contains(&"LED=GPIO25".to_string()));
        assert!(pico.pins.contains(&"GP0=GPIO0".to_string()));
        assert_eq!(pico.pins.len(), 29 + 1);
    }
}
