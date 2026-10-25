//! # ee-devices
//!
//! Concrete device models and their stamping/behavior, plus **board
//! definitions** (Arduino Uno/Nano/Mega, Raspberry Pi Pico, ESP32-class, and
//! user-defined custom boards) and a searchable **parts library**.
//!
//! A board is a manifest: MCU, pinout, and onboard parts. Custom boards drop in
//! by writing a manifest — [`BoardDef::from_manifest`] parses the small text form
//! in [`manifest`]; see docs/FEATURES.md §3. Drop-in part models (LED, 7-segment,
//! servo, WS2812) live in [`modules`]. (Roadmap M1–M3.)

pub mod manifest;
pub mod modules;

pub use manifest::{ManifestError, OnboardPart};

/// A board the user can `board add`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardDef {
    pub model: String,
    /// e.g. "atmega328p".
    pub mcu: String,
    /// Named pins → the MCU port bit they map to, as `"<pin>=<port><bit>"`
    /// (e.g. `"D13=PB5"`). The runnable core + pin resolution lives in
    /// `ee-mcu`'s `avr::board::Uno`; this manifest is the catalog entry.
    pub pins: Vec<String>,
    /// Parts soldered onto the board (onboard LED, headers for drop-in modules).
    /// Behavioural models for these part kinds live in [`modules`].
    pub parts: Vec<OnboardPart>,
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
            parts: vec![OnboardPart {
                kind: "led".to_string(),
                pin: "D13".to_string(),
            }],
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
            parts: vec![OnboardPart {
                kind: "led".to_string(),
                pin: "LED".to_string(),
            }],
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
            parts: vec![OnboardPart {
                kind: "led".to_string(),
                pin: "LED".to_string(),
            }],
        }
    }

    /// An **ESP32** dev-board manifest (catalog **stub**).
    ///
    /// The pin map records a representative subset of the ESP32's GPIO header
    /// (`D<n>=GPIO<n>`) plus the common on-board LED on GPIO2. This is a manifest
    /// entry only: unlike the Uno/Mega/Pico there is **no runnable ESP32 core** in
    /// `ee-mcu` (the Xtensa LX6 / RISC-V cores are out of scope for this
    /// milestone), so a board built from this definition catalogs and pin-maps but
    /// cannot execute firmware yet.
    pub fn esp32() -> Self {
        let mut pins = Vec::new();
        // A representative slice of the usable GPIO header.
        for g in [
            2u8, 4, 5, 12, 13, 14, 15, 16, 17, 18, 19, 21, 22, 23, 25, 26, 27,
        ] {
            pins.push(format!("D{g}=GPIO{g}"));
        }
        pins.push("LED=GPIO2".to_string());
        Self {
            model: "esp32".to_string(),
            mcu: "esp32".to_string(),
            pins,
            parts: vec![OnboardPart {
                kind: "led".to_string(),
                pin: "LED".to_string(),
            }],
        }
    }

    /// An **STM32** ("Blue Pill", STM32F103) manifest (catalog **stub**).
    ///
    /// The pin map records the port-A/B/C GPIO header (`P<port><bit>`) and the
    /// on-board LED on PC13. Like [`esp32`](Self::esp32) this is a manifest entry
    /// only — there is **no runnable STM32 (Cortex-M3) core** wired up here, so the
    /// board catalogs and pin-maps but does not execute firmware yet.
    pub fn stm32() -> Self {
        let mut pins = Vec::new();
        for bit in 0u8..=15 {
            pins.push(format!("PA{bit}=GPIOA{bit}"));
        }
        for bit in 0u8..=15 {
            pins.push(format!("PB{bit}=GPIOB{bit}"));
        }
        pins.push("PC13=GPIOC13".to_string());
        pins.push("LED=GPIOC13".to_string());
        Self {
            model: "stm32".to_string(),
            mcu: "stm32f103".to_string(),
            pins,
            parts: vec![OnboardPart {
                kind: "led".to_string(),
                pin: "LED".to_string(),
            }],
        }
    }

    /// Build a board from a declarative [`manifest`] text form — the drop-in path
    /// for user-defined custom boards. See [`manifest::parse`].
    pub fn from_manifest(src: &str) -> Result<Self, ManifestError> {
        manifest::parse(src)
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

    #[test]
    fn builtin_boards_record_their_onboard_led() {
        for b in [BoardDef::uno(), BoardDef::mega(), BoardDef::pico()] {
            assert!(
                b.parts.iter().any(|p| p.kind == "led"),
                "{} should carry an onboard LED part",
                b.model
            );
        }
    }

    #[test]
    fn esp32_and_stm32_are_pin_mapped_stubs() {
        let esp = BoardDef::esp32();
        assert_eq!(esp.mcu, "esp32");
        assert!(esp.pins.contains(&"D2=GPIO2".to_string()));
        assert!(esp.pins.contains(&"LED=GPIO2".to_string()));

        let stm = BoardDef::stm32();
        assert_eq!(stm.mcu, "stm32f103");
        assert!(stm.pins.contains(&"PC13=GPIOC13".to_string()));
        assert!(stm.pins.contains(&"LED=GPIOC13".to_string()));
        assert!(stm.pins.contains(&"PA0=GPIOA0".to_string()));
    }

    #[test]
    fn from_manifest_builds_a_custom_board() {
        let src = "\
name   custom1
mcu    esp32
pin    D2=GPIO2
pin    LED=GPIO2
part   led LED
part   servo D4
";
        let b = BoardDef::from_manifest(src).unwrap();
        assert_eq!(b.model, "custom1");
        assert_eq!(b.mcu, "esp32");
        assert_eq!(
            b.pins,
            vec!["D2=GPIO2".to_string(), "LED=GPIO2".to_string()]
        );
        assert_eq!(b.parts.len(), 2);
        assert_eq!(b.parts[0].kind, "led");
        assert_eq!(b.parts[1].pin, "D4");
    }
}
