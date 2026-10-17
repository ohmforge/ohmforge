//! Worked example: real ARMv6-M (Thumb) firmware blinking the Raspberry Pi
//! Pico's on-board LED on GP25, through the RP2040 SIO GPIO block.
//!
//! Run it with:
//!
//! ```text
//! cargo run -p ee-mcu --example pico_blink
//! ```
//!
//! The firmware is a hand-assembled raw image: a two-word vector table (initial
//! SP, then the reset vector pointing at the program with the Thumb bit set)
//! followed by code that loads `SIO_BASE`, builds the `1 << 25` GP25 mask,
//! enables the output (`GPIO_OE_SET`) and then toggles it in a loop
//! (`GPIO_OUT_XOR`). See `docs/guide/emulation.md` for the instruction-by-
//! instruction walkthrough.

use ee_mcu::cortexm::board::LED_GPIO;
use ee_mcu::cortexm::periph::SIO_BASE;
use ee_mcu::cortexm::Pico;

fn main() {
    // Build the raw firmware image.
    //   0x00: initial SP   = 0x0002_0000
    //   0x04: reset vector = 0x40 | 1 (Thumb)
    //   0x40: LDR  r0,[pc,#8]    0x4802  → r0 = SIO_BASE (literal at 0x4C)
    //   0x42: MOVS r1,#1         0x2101
    //   0x44: LSLS r1,r1,#25     0x0649  → r1 = 1 << 25 (GP25 mask)
    //   0x46: STR  r1,[r0,#0x24] 0x6241  → GPIO_OE_SET = mask (GP25 output)
    //   0x48: STR  r1,[r0,#0x1c] 0x61C1  → GPIO_OUT_XOR = mask (toggle GP25)
    //   0x4A: B    .-2           0xE7FD  → back to the toggle
    //   0x4C: literal SIO_BASE (word)
    let mut image = vec![0u8; 0x50];
    image[0..4].copy_from_slice(&0x0002_0000u32.to_le_bytes());
    image[4..8].copy_from_slice(&(0x40u32 | 1).to_le_bytes());
    for (addr, hw) in [
        (0x40usize, 0x4802u16),
        (0x42, 0x2101),
        (0x44, 0x0649),
        (0x46, 0x6241),
        (0x48, 0x61C1),
        (0x4A, 0xE7FD),
    ] {
        image[addr..addr + 2].copy_from_slice(&hw.to_le_bytes());
    }
    image[0x4C..0x50].copy_from_slice(&SIO_BASE.to_le_bytes());

    let mut pico = Pico::new();
    pico.load_firmware(&image).expect("firmware loads");

    println!("Raspberry Pi Pico blink — GP{LED_GPIO} (on-board LED) via the RP2040 SIO\n");

    // Setup: LDR, MOVS, LSLS, STR(OE_SET).
    for _ in 0..4 {
        pico.step();
    }
    println!(
        "  after setup: GP{LED_GPIO} = {}",
        level(pico.digital_read(LED_GPIO))
    );

    let mut states = Vec::new();
    for cycle in 1..=4 {
        pico.step(); // STR GPIO_OUT_XOR → toggle
        let high = pico.digital_read(LED_GPIO);
        println!("  toggle {cycle}: GP{LED_GPIO} = {}", level(high));
        states.push(high);
        pico.step(); // B .-2 (back to the toggle)
    }

    // Starting low, each XOR flips the LED: the sequence must alternate
    // high, low, high, low …
    assert_eq!(
        states,
        vec![true, false, true, false],
        "GP{LED_GPIO} toggles on every GPIO_OUT_XOR"
    );
}

fn level(high: bool) -> &'static str {
    if high {
        "HIGH"
    } else {
        "low"
    }
}
