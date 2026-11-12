//! The ESP32 board definition: an Xtensa LX6 core plus the ESP32's GPIO mapping,
//! so firmware that talks in "GPIO2" (a common on-board LED pin) lands on the
//! right GPIO bit.
//!
//! The ESP32 brings `GPIO0..GPIO39` out, of which this core models the low
//! `GPIO0..GPIO31` bank; the user-facing pin number *is* the GPIO number, and the
//! on-board LED on many dev boards sits on **GPIO2**. [`Esp32`] wraps an
//! [`Xtensa`] clocked at the ESP32's 240 MHz default and resolves a GPIO number
//! to its pin, plugging into the identical [`McuCore`] co-sim surface as the
//! [`Uno`](crate::avr::Uno) and [`Pico`](crate::cortexm::Pico).

use crate::{FirmwareError, McuCore, PinId, PinIo};

use super::core::Xtensa;
use super::periph::{pin_id, PIN_COUNT};

/// The ESP32's default CPU clock (240 MHz).
pub const ESP32_F_CPU: f64 = 240_000_000.0;

/// A common ESP32 dev-board on-board LED GPIO (GPIO2).
pub const LED_GPIO: u8 = 2;

/// An ESP32 board: an Xtensa LX6 core with the ESP32 GPIO map.
pub struct Esp32 {
    mcu: Xtensa,
}

impl Default for Esp32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Esp32 {
    /// A reset ESP32 clocked at 240 MHz.
    pub fn new() -> Self {
        let mut mcu = Xtensa::new();
        mcu.set_clock_hz(ESP32_F_CPU);
        Self { mcu }
    }

    /// The underlying MCU core (registers, memory, GPIO).
    pub fn mcu(&self) -> &Xtensa {
        &self.mcu
    }
    /// Mutable access to the underlying MCU core.
    pub fn mcu_mut(&mut self) -> &mut Xtensa {
        &mut self.mcu
    }

    /// Set the CPU clock in Hz (tests may slow it to keep a run short).
    pub fn set_clock_hz(&mut self, hz: f64) {
        self.mcu.set_clock_hz(hz);
    }

    /// Load firmware (a raw byte image) into memory and reset.
    pub fn load_firmware(&mut self, image: &[u8]) -> Result<(), FirmwareError> {
        self.mcu.load_firmware(image)
    }

    /// Execute one instruction.
    pub fn step(&mut self) {
        self.mcu.step();
    }
    /// Run until the simulated clock reaches `t` seconds.
    pub fn step_to(&mut self, t: f64) {
        self.mcu.step_to(t);
    }

    // --- Co-simulation bridge (forwards to the core; see [`McuCore`]) ---

    /// A [`PinIo`] exchange sized for this board's pins.
    pub fn pin_io(&self) -> PinIo {
        PinIo::new(self.mcu.pin_count())
    }

    /// The absolute cycle count at which the core next wants to run.
    pub fn next_event(&self) -> Option<u64> {
        self.mcu.next_event()
    }

    /// Advance to the cycle bound `until_cycle` while exchanging pin state.
    pub fn step_to_cycle(&mut self, until_cycle: u64, io: &mut PinIo) -> u64 {
        self.mcu.step_to_cycle(until_cycle, io)
    }

    /// A single co-sim step exchanging pin state.
    pub fn step_with_io(&mut self, io: &mut PinIo) {
        self.mcu.step_with_io(io);
    }

    /// The [`PinId`] for `GPIO<gpio>` (`0..=31`), for mapping it to a circuit node
    /// in the co-sim loop. The GPIO number is the pin index.
    pub fn gpio_pin_id(gpio: u8) -> Option<PinId> {
        if (gpio as usize) < PIN_COUNT {
            Some(pin_id(gpio))
        } else {
            None
        }
    }

    /// The logic level on `GPIO<gpio>` (`false` for unknown pins).
    pub fn digital_read(&self, gpio: u8) -> bool {
        if (gpio as usize) < PIN_COUNT {
            self.mcu.gpio_level(gpio)
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esp32_led_is_on_gpio2() {
        assert_eq!(LED_GPIO, 2);
        assert_eq!(Esp32::gpio_pin_id(2), Some(pin_id(2)));
        assert_eq!(Esp32::gpio_pin_id(32), None);
    }

    #[test]
    fn esp32_publishes_led_drive_to_pin_io() {
        use super::super::periph::GPIO_BASE;
        // Enable GPIO2 as an output and drive it high, then confirm the drive
        // reaches the co-sim PinIo exchange.
        //  literal GPIO_BASE at 0 ; code from 4.
        let mut image = vec![0u8; 0x10];
        image[0..4].copy_from_slice(&GPIO_BASE.to_le_bytes());
        let code: [(usize, [u8; 3]); 4] = [
            (0x04, [0x21, 0xFF, 0xFF]), // L32R a2, -4 → GPIO_BASE
            (0x07, [0x32, 0xA0, 0x04]), // MOVI a3, 4 (mask = 1<<2)
            (0x0A, [0x32, 0x62, 0x09]), // S32I a3,a2,0x24 → ENABLE_W1TS
            (0x0D, [0x32, 0x62, 0x02]), // S32I a3,a2,0x08 → OUT_W1TS (high)
        ];
        for (addr, bytes) in code {
            image[addr..addr + 3].copy_from_slice(&bytes);
        }
        let mut board = Esp32::new();
        board.mcu_mut().load_program(&image);
        board.mcu_mut().set_pc(0x04);
        let mut io = board.pin_io();
        for _ in 0..4 {
            board.step_with_io(&mut io);
        }
        let led = Esp32::gpio_pin_id(LED_GPIO).unwrap();
        assert_eq!(io.drive(led), crate::PinDrive::Digital(true));
    }
}
