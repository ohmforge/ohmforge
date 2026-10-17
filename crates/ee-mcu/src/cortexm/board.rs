//! The Raspberry Pi Pico board definition: an RP2040 (Cortex-M0+) core plus the
//! Pico's pin mapping, so firmware that talks in "GP25" (the on-board LED) lands
//! on the right SIO GPIO bit.
//!
//! The Pico brings the RP2040's `GPIO0..GPIO28` out to its header (plus a few
//! reserved pins); the user-facing pin number *is* the GPIO number, and the
//! on-board LED sits on **GP25**. [`Pico`] wraps a [`CortexM`] clocked at the
//! RP2040's 125 MHz default and resolves a GP number to its GPIO pin.

use crate::{FirmwareError, McuCore, PinId, PinIo};

use super::core::CortexM;
use super::periph::pin_id;

/// The RP2040's default system clock (125 MHz).
pub const PICO_F_CPU: f64 = 125_000_000.0;

/// The Pico's on-board LED GPIO (GP25).
pub const LED_GPIO: u8 = 25;

/// A Raspberry Pi Pico: an RP2040 (Cortex-M0+) with the Pico pin map.
pub struct Pico {
    mcu: CortexM,
}

impl Default for Pico {
    fn default() -> Self {
        Self::new()
    }
}

impl Pico {
    /// A reset Pico clocked at 125 MHz.
    pub fn new() -> Self {
        let mut mcu = CortexM::new();
        mcu.set_clock_hz(PICO_F_CPU);
        Self { mcu }
    }

    /// The underlying MCU core.
    pub fn mcu(&self) -> &CortexM {
        &self.mcu
    }
    /// Mutable access to the underlying MCU core.
    pub fn mcu_mut(&mut self) -> &mut CortexM {
        &mut self.mcu
    }

    /// Set the CPU clock in Hz (tests may slow it to keep a run short).
    pub fn set_clock_hz(&mut self, hz: f64) {
        self.mcu.set_clock_hz(hz);
    }

    /// Load firmware (raw binary; the first two words are the vector table) and
    /// reset.
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

    /// The [`PinId`] for Pico GP pin `gp` (`0..=29`), for mapping it to a circuit
    /// node in the co-sim loop. The GP number is the GPIO number.
    pub fn gpio_pin_id(gp: u8) -> Option<PinId> {
        if (gp as usize) < super::periph::PIN_COUNT {
            Some(pin_id(gp))
        } else {
            None
        }
    }

    /// The logic level on GP pin `gp` (`false` for unknown pins).
    pub fn digital_read(&self, gp: u8) -> bool {
        if (gp as usize) < super::periph::PIN_COUNT {
            self.mcu.gpio_level(gp)
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cortexm::periph::{pin_id, SIO_BASE};

    #[test]
    fn pico_led_is_on_gp25() {
        assert_eq!(LED_GPIO, 25);
        assert_eq!(Pico::gpio_pin_id(25), Some(pin_id(25)));
        assert_eq!(Pico::gpio_pin_id(30), None);
    }

    /// The Pico "blink" gate: real ARMv6-M firmware enables GP25 and toggles it,
    /// and the on-board LED pin follows.
    #[test]
    fn pico_blink_toggles_the_led_gpio() {
        // Hand-assembled firmware. A vector table (SP, reset→0x40|thumb) then at
        // byte 0x40:
        //   LDR r0, [pc, #...]   → r0 = SIO_BASE (from a literal pool)
        //   MOVS r1, #1 ; LSLS r1, r1, #25   → r1 = 1<<25 (the GP25 mask)
        //   STR r1, [r0, #0x24]  → GPIO_OE_SET = mask (GP25 = output)
        // loop:
        //   STR r1, [r0, #0x1c]  → GPIO_OUT_XOR = mask (toggle GP25)
        //   B loop
        //
        // We assemble by hand into a byte image. Offsets within the program
        // (from 0x40):
        //   0x40: LDR r0,[pc,#8]   0x4802  (base=align(0x40+4)=0x44; +8 = 0x4C pool)
        //   0x42: MOVS r1,#1       0x2101
        //   0x44: LSLS r1,r1,#25   0x0649  (imm5=25)
        //   0x46: STR r1,[r0,#0x24] 0x6241 (imm5=9 → 9*4=0x24)
        //   0x48: STR r1,[r0,#0x1c] 0x61C1 (imm5=7 → 7*4=0x1c)
        //   0x4A: B .-2 (to 0x48)  0xE7FE  (PC=0x4A+4=0x4E; +off=0x48 → off=-6 → imm11=-3)
        //   0x4C: literal SIO_BASE (word)
        let mut image = vec![0u8; 0x50];
        image[0..4].copy_from_slice(&0x0002_0000u32.to_le_bytes()); // SP
        image[4..8].copy_from_slice(&(0x40u32 | 1).to_le_bytes()); // reset → 0x40
        let prog: [(usize, u16); 6] = [
            (0x40, 0x4802),
            (0x42, 0x2101),
            (0x44, 0x0649),
            (0x46, 0x6241),
            (0x48, 0x61C1),
            (0x4A, 0xE7FD),
        ];
        for (addr, hw) in prog {
            image[addr..addr + 2].copy_from_slice(&hw.to_le_bytes());
        }
        image[0x4C..0x50].copy_from_slice(&SIO_BASE.to_le_bytes());

        let mut pico = Pico::new();
        pico.load_firmware(&image).expect("firmware loads");

        // Run setup: LDR, MOVS, LSLS, STR(OE_SET).
        for _ in 0..4 {
            pico.step();
        }
        assert!(!pico.digital_read(LED_GPIO), "LED starts low");
        pico.step(); // STR OUT_XOR → toggle high
        assert!(pico.digital_read(LED_GPIO), "blink drove GP25 high");
        pico.step(); // B loop
        pico.step(); // STR OUT_XOR → toggle low
        assert!(!pico.digital_read(LED_GPIO), "blink toggled GP25 back low");
    }
}
