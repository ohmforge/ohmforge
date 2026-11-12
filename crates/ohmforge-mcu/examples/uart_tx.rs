//! Worked example: the USART0 transmitter shifting a byte stream out at 9600
//! baud, captured for inspection.
//!
//! Run it with:
//!
//! ```text
//! cargo run -p ohmforge-mcu --example uart_tx
//! ```
//!
//! The USART registers are driven through the core's data space (what an
//! `OUT`/`STS` does): enable the transmitter, set the baud divisor, then write
//! each byte to `UDR0` once the holding buffer is free (`UDRE0` set). A spin loop
//! advances CPU cycles, so the transmitter shifts bytes out frame-by-frame in
//! step with execution. Every shifted byte is captured and printed at the end.
//!
//! At 16 MHz with `UBRR0 = 103` the bit time is `16 × 104 = 1664` cycles and an
//! 8-N-1 frame (1 start + 8 data + 1 stop) is `10 × 1664 = 16 640` cycles.

use ohmforge_mcu::avr::Uno;

// Data-space register addresses (I/O address + 0x20).
const UCSR0A: u16 = 0xC0;
const UCSR0B: u16 = 0xC1;
const UBRR0L: u16 = 0xC4;
const UBRR0H: u16 = 0xC5;
const UDR0: u16 = 0xC6;

const TXEN0: u8 = 1 << 3;
const UDRE0: u8 = 1 << 5;

/// Step the core until its cycle counter has advanced by at least `delta`.
fn advance(uno: &mut Uno, delta: u64) {
    let target = uno.mcu().cycles() + delta;
    while uno.mcu().cycles() < target {
        uno.step();
    }
}

fn main() {
    let mut uno = Uno::new();
    uno.mcu_mut().load_program(&[0xCFFF]); // RJMP .-1 — spin so stepping ticks the USART

    let avr = uno.mcu_mut();
    avr.write_data(UCSR0B, TXEN0); // enable the transmitter
    avr.write_data(UBRR0H, 0);
    avr.write_data(UBRR0L, 103); // 9600 baud @ 16 MHz

    let message = b"Hi!";
    println!("USART0 TX @ 9600 baud (16 MHz): sending {:?}\n", "Hi!");

    for &byte in message {
        // Wait for the holding buffer to drain (UDRE0 set), stepping meanwhile.
        while uno.mcu().read_data(UCSR0A) & UDRE0 == 0 {
            uno.step();
        }
        uno.mcu_mut().write_data(UDR0, byte);
        println!("  wrote UDR0 = {byte:#04x} ('{}')", byte as char);
        // Advance one full frame so the byte finishes shifting out.
        advance(&mut uno, 16_640);
    }
    // Flush any byte still in the shift register.
    advance(&mut uno, 16_640);

    let captured = uno.mcu().peripherals().usart_tx();
    let text: String = captured.iter().map(|&b| b as char).collect();
    println!(
        "\n  transmitter captured {} bytes: {:?} = \"{text}\"",
        captured.len(),
        captured
    );

    assert_eq!(captured, message, "the TX stream should match what we sent");
}
