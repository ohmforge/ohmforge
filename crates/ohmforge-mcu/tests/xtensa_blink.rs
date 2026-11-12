//! CP-BRD-M7.1 gate: a hand-assembled Xtensa (ESP32 LX6) blink toggles an
//! [`Esp32`] GPIO pin, and the decoder's covered-opcode set is asserted.
//!
//! # How the blink program was assembled
//!
//! Xtensa instructions are variable-length, little-endian byte streams. The
//! program loads the ESP32 GPIO register base through an `L32R` literal pool
//! (the Xtensa idiom for a 32-bit constant — the pool sits *before* the
//! instruction), configures GPIO2 (a common on-board LED) as an output, then
//! loops setting and clearing the pin through the atomic write-1-to-set /
//! write-1-to-clear windows. By byte address:
//!
//! ```text
//!  0x00: .word 0x3FF44000     ; GPIO register base (the L32R literal pool)
//!  0x04: L32R a2, 0x00        ; 21 FF FF — a2 = GPIO_BASE
//!  0x07: MOVI a3, 4           ; 32 A0 04 — a3 = 1<<2 (the GPIO2 bit mask)
//!  0x0A: S32I a3, a2, 0x24    ; 32 62 09 — GPIO_ENABLE_W1TS: GPIO2 = output
//!  0x0D: (loop) S32I a3,a2,8  ; 32 62 02 — GPIO_OUT_W1TS: drive GPIO2 high
//!  0x10: S32I a3, a2, 0x0C    ; 32 62 03 — GPIO_OUT_W1TC: drive GPIO2 low
//!  0x13: J loop               ; 86 FD FF — back to 0x0D
//! ```
//!
//! Since the `L32R` pool precedes the code, execution starts at the entry point
//! `0x04` (above the pool), set with [`Xtensa::set_pc`] after loading the image.

use ohmforge_mcu::xtensa::{decode, Esp32, Instr, Size, GPIO_BASE, LED_GPIO};

/// The GPIO register base the blink loads via its `L32R` literal pool.
const BLINK_LITERAL: u32 = GPIO_BASE;

/// Build the blink byte image: the `L32R` literal pool at `0x00`, code from
/// `0x04`.
fn blink_image() -> Vec<u8> {
    let mut image = vec![0u8; 0x18];
    image[0..4].copy_from_slice(&BLINK_LITERAL.to_le_bytes());
    let code: [(usize, [u8; 3]); 6] = [
        (0x04, [0x21, 0xFF, 0xFF]), // L32R a2, 0x00
        (0x07, [0x32, 0xA0, 0x04]), // MOVI a3, 4
        (0x0A, [0x32, 0x62, 0x09]), // S32I a3, a2, 0x24 (ENABLE_W1TS)
        (0x0D, [0x32, 0x62, 0x02]), // S32I a3, a2, 0x08 (OUT_W1TS)
        (0x10, [0x32, 0x62, 0x03]), // S32I a3, a2, 0x0C (OUT_W1TC)
        (0x13, [0x86, 0xFD, 0xFF]), // J 0x0D
    ];
    for (addr, bytes) in code {
        image[addr..addr + 3].copy_from_slice(&bytes);
    }
    image
}

#[test]
fn esp32_blink_toggles_gpio2() {
    let image = blink_image();
    let mut esp = Esp32::new();
    esp.mcu_mut().load_program(&image);
    esp.mcu_mut().set_pc(0x04); // entry point, above the literal pool

    // Setup: L32R (base), MOVI (mask), S32I (enable output).
    esp.step(); // L32R a2 → GPIO_BASE
    assert_eq!(esp.mcu().reg(2), GPIO_BASE, "L32R loaded the GPIO base");
    esp.step(); // MOVI a3, 4
    esp.step(); // S32I ENABLE_W1TS → GPIO2 output
    assert!(
        !esp.digital_read(LED_GPIO),
        "enabled as output but driving low"
    );

    // The loop body toggles the pin: high, then low, repeating.
    esp.step(); // OUT_W1TS → high
    assert!(esp.digital_read(LED_GPIO), "blink drove GPIO2 high");
    esp.step(); // OUT_W1TC → low
    assert!(!esp.digital_read(LED_GPIO), "blink drove GPIO2 back low");
    esp.step(); // J → back to the loop top
    esp.step(); // OUT_W1TS → high again
    assert!(esp.digital_read(LED_GPIO), "blink toggled GPIO2 high again");

    // And GPIO2 really is the mapped LED pin.
    assert_eq!(LED_GPIO, 2);
}

#[test]
fn blink_drive_reaches_the_cosim_pin_exchange() {
    // The same program, stepped through the `PinIo` exchange, drives the LED pin
    // out to the co-sim bridge (no `ohmforge-sim` dependency crosses here).
    let image = blink_image();
    let mut esp = Esp32::new();
    esp.mcu_mut().load_program(&image);
    esp.mcu_mut().set_pc(0x04);
    let mut io = esp.pin_io();

    for _ in 0..4 {
        esp.step_with_io(&mut io); // through enabling + first OUT_W1TS (high)
    }
    let led = Esp32::gpio_pin_id(LED_GPIO).unwrap();
    assert_eq!(io.drive(led), ohmforge_mcu::PinDrive::Digital(true));
}

/// The decoder's covered-opcode set: the supported subset decodes to real
/// [`Instr`] variants, and unimplemented encodings decode to [`Instr::Unknown`].
#[test]
fn decoder_covers_the_supported_subset() {
    // Each entry is (bytes, expected decoded instruction, expected byte length).
    let covered: &[([u8; 3], Instr, u8)] = &[
        // RRR ALU (and MOV via OR) + control.
        ([0x50, 0x34, 0x80], Instr::Add { r: 3, s: 4, t: 5 }, 3),
        ([0x50, 0x34, 0xC0], Instr::Sub { r: 3, s: 4, t: 5 }, 3),
        ([0x50, 0x34, 0x10], Instr::And { r: 3, s: 4, t: 5 }, 3),
        ([0x50, 0x34, 0x20], Instr::Or { r: 3, s: 4, t: 5 }, 3),
        ([0x50, 0x34, 0x30], Instr::Xor { r: 3, s: 4, t: 5 }, 3),
        ([0x80, 0x00, 0x00], Instr::Ret, 3),
        ([0xA0, 0x05, 0x00], Instr::Jx { s: 5 }, 3),
        ([0xC0, 0x03, 0x00], Instr::Callx0 { s: 3 }, 3),
        // RRI8 immediate ALU / move.
        ([0x22, 0xA0, 0x04], Instr::Movi { t: 2, imm: 4 }, 3),
        ([0x32, 0xC4, 0x05], Instr::Addi { t: 3, s: 4, imm: 5 }, 3),
        // Loads / stores.
        (
            [0x42, 0x22, 0x01],
            Instr::Load {
                t: 4,
                s: 2,
                off: 0x04,
                size: Size::Word,
                signed: false,
            },
            3,
        ),
        (
            [0x32, 0x62, 0x02],
            Instr::Store {
                t: 3,
                s: 2,
                off: 0x08,
                size: Size::Word,
            },
            3,
        ),
        (
            [0x42, 0x02, 0x07],
            Instr::Load {
                t: 4,
                s: 2,
                off: 0x07,
                size: Size::Byte,
                signed: false,
            },
            3,
        ),
        (
            [0x42, 0x92, 0x02],
            Instr::Load {
                t: 4,
                s: 2,
                off: 0x04,
                size: Size::Half,
                signed: true,
            },
            3,
        ),
        // L32R, J, BZ branches.
        (
            [0x21, 0xFF, 0xFF],
            Instr::L32r {
                t: 2,
                imm16: 0xFFFF,
            },
            3,
        ),
        ([0x86, 0xFD, 0xFF], Instr::J { offset: -10 }, 3),
        ([0x16, 0x83, 0x00], Instr::Beqz { s: 3, offset: 8 }, 3),
        ([0x56, 0x83, 0x00], Instr::Bnez { s: 3, offset: 8 }, 3),
        // Code-density narrow forms (2-byte).
        ([0x3D, 0x04, 0x00], Instr::MovN { t: 3, s: 4 }, 2),
        ([0x3D, 0xF0, 0x00], Instr::Nop, 2), // NOP.N
        ([0x0D, 0xF0, 0x00], Instr::Ret, 2), // RET.N
    ];
    for (bytes, expected, len) in covered {
        let (instr, got_len) = decode(bytes[0], bytes[1], bytes[2]);
        assert_eq!(instr, *expected, "decoding {bytes:02X?}");
        assert_eq!(got_len, *len, "length of {bytes:02X?}");
    }

    // Encodings OUTSIDE the modelled subset must decode to `Unknown`.
    let uncovered: &[[u8; 3]] = &[
        [0x15, 0x00, 0x00], // CALL4 — a windowed call
        [0x07, 0x00, 0x00], // BEQ — register-register compare branch (op0=7)
        [0x26, 0x00, 0x00], // BEQI — immediate-compare branch (op0=6, n=2)
        [0x00, 0x00, 0x01], // a shift (RST1, op1=1)
        [0x00, 0x00, 0x90], // ADDX2 (RST0, op2=9)
        [0x08, 0x00, 0x00], // L32I.N — an unmodelled narrow form
        [0x0E, 0x00, 0x00], // a reserved major opcode
    ];
    for bytes in uncovered {
        let (instr, _) = decode(bytes[0], bytes[1], bytes[2]);
        assert!(
            matches!(instr, Instr::Unknown(_)),
            "{bytes:02X?} should be Unknown, got {instr:?}"
        );
    }
}
