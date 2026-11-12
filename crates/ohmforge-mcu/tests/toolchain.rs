//! (B) CP-BRD-M2.5 gate: external-toolchain compile orchestration.
//!
//! Compile a sketch to a HEX image with a **system** toolchain, load it into the
//! emulator and run it end-to-end — and **skip cleanly** when no toolchain is
//! present, so the default `cargo test --workspace` needs no external compiler.
//!
//! Two layers of gating (see `crates/ohmforge-mcu/src/toolchain.rs`):
//!
//! - Without the `toolchain` cargo feature (the default), the end-to-end path is
//!   not even compiled; the placeholder test below records that it is opt-in.
//! - With `--features toolchain`, the test detects `avr-gcc`/`arduino-cli` and
//!   skips if neither is installed, otherwise compiles `blink.c` → HEX → runs it
//!   on an Uno and asserts the LED (D13 → PB5) is driven high.

#[cfg(not(feature = "toolchain"))]
#[test]
fn external_toolchain_compile_is_opt_in() {
    // Visible with `--nocapture`; the point is that the default run neither
    // requires a compiler nor shells out.
    eprintln!(
        "SKIPPED: external-toolchain compile orchestration is behind the `toolchain` \
         feature. Run `cargo test -p ohmforge-mcu --features toolchain` (and install \
         avr-gcc/avr-objcopy or arduino-cli) to exercise it end-to-end."
    );
}

#[cfg(feature = "toolchain")]
mod gated {
    use ohmforge_mcu::avr::Uno;
    use ohmforge_mcu::toolchain::{self, ToolchainError};

    /// A freestanding ATmega328P sketch: PB5 (Arduino D13 / the on-board LED) as
    /// an output, driven high, then spin. No avr-libc runtime is needed
    /// (`-nostartfiles`), so it uses only opcodes the modelled core implements.
    const BLINK_C: &str = r#"
#include <avr/io.h>
int main(void) {
    DDRB = (1 << 5);
    PORTB = (1 << 5);
    for (;;) { }
}
"#;

    #[test]
    fn compile_c_to_hex_then_blink_runs() {
        // Presence gate: no toolchain ⇒ skip (not fail).
        let Some(tool) = toolchain::detect() else {
            eprintln!("SKIPPED: no avr-gcc/avr-objcopy or arduino-cli on PATH");
            return;
        };
        eprintln!("compiling with detected toolchain: {tool:?}");

        // A unique scratch directory under the OS temp dir.
        let work = std::env::temp_dir().join(format!("ee-toolchain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).expect("create scratch dir");
        let src = work.join("blink.c");
        std::fs::write(&src, BLINK_C).expect("write sketch");

        let hex = match toolchain::compile_to_hex(&src, "atmega328p", &work) {
            Ok(hex) => hex,
            // The toolchain disappeared between detect() and use: still a skip.
            Err(ToolchainError::NotFound) => {
                eprintln!("SKIPPED: toolchain not found at compile time");
                return;
            }
            Err(e) => panic!("sketch compilation failed: {e}"),
        };
        assert!(
            hex.starts_with(b":"),
            "avr-objcopy produced an Intel HEX image"
        );

        // Load the compiled firmware and run it: the LED must come on.
        let mut uno = Uno::new();
        uno.load_firmware(&hex)
            .expect("load compiled hex into the Uno");
        for _ in 0..64 {
            if uno.digital_read(13) {
                break;
            }
            uno.step();
        }
        assert!(
            uno.digital_read(13),
            "compiled blink drove D13 (PB5) high end-to-end"
        );

        let _ = std::fs::remove_dir_all(&work);
    }
}
