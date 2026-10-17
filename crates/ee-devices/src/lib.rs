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
}
