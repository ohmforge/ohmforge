//! External-toolchain compile orchestration (CP-BRD-M2.5): turn a sketch into a
//! loadable HEX image by shelling out to a **system** toolchain, then hand the
//! bytes to [`load_firmware`](crate::McuCore::load_firmware).
//!
//! # Why this is gated
//!
//! Compiling real firmware needs a cross-compiler that is not part of this
//! workspace: `avr-gcc` + `avr-objcopy`, or `arduino-cli`. Requiring either for
//! `cargo test` would make the suite depend on the host having a toolchain (and,
//! for `arduino-cli`, on a network to install board cores). So this module is:
//!
//! - **feature-gated** behind the `toolchain` cargo feature (OFF by default), so
//!   the normal build and `cargo test --workspace` never compile this code and
//!   pull in no `std::process` use; and
//! - **presence-gated** at runtime — [`detect()`](crate::toolchain::detect)
//!   returns `None` when no toolchain is installed, so a caller (or the gate
//!   test) **skips** cleanly rather than failing.
//!
//! Shelling out is appropriate *here* because this is a build-orchestration
//! feature; it is deliberately isolated to this module and is never invoked by
//! any other test or by the cores themselves. The module is also excluded from
//! `wasm32` targets (no process spawning there).
//!
//! # Flow
//!
//! ```no_run
//! # #[cfg(feature = "toolchain")] {
//! use ee_mcu::avr::Uno;
//! use ee_mcu::toolchain;
//! use std::path::Path;
//!
//! let work = std::env::temp_dir().join("ee-sketch");
//! std::fs::create_dir_all(&work).unwrap();
//! // Compile a C sketch for the Uno's ATmega328P to Intel HEX...
//! let hex = toolchain::compile_to_hex(Path::new("blink.c"), "atmega328p", &work).unwrap();
//! // ...then load it into the emulator and run it.
//! let mut uno = Uno::new();
//! uno.load_firmware(&hex).unwrap();
//! # }
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

/// A detected system toolchain capable of producing a HEX image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toolchain {
    /// `avr-gcc` (compile) + `avr-objcopy` (ELF → Intel HEX).
    AvrGcc,
    /// `arduino-cli compile` (handles `.ino` sketches, emits a `.hex`).
    ArduinoCli,
}

/// Why compile orchestration could not produce a HEX image.
#[derive(Debug)]
pub enum ToolchainError {
    /// No supported toolchain was found on `PATH`.
    NotFound,
    /// A toolchain program could not be spawned (e.g. vanished from `PATH`).
    Spawn {
        program: String,
        source: std::io::Error,
    },
    /// A toolchain program ran but exited non-zero.
    Stage {
        /// Which step failed (`"avr-gcc"`, `"avr-objcopy"`, `"arduino-cli"`).
        stage: &'static str,
        /// The program's captured stderr, for diagnosis.
        stderr: String,
    },
    /// A filesystem error reading/writing the scratch dir or the HEX output.
    Io(std::io::Error),
}

impl std::fmt::Display for ToolchainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolchainError::NotFound => {
                f.write_str("no AVR toolchain found (install avr-gcc/avr-objcopy or arduino-cli)")
            }
            ToolchainError::Spawn { program, source } => {
                write!(f, "could not run `{program}`: {source}")
            }
            ToolchainError::Stage { stage, stderr } => {
                write!(f, "`{stage}` failed:\n{stderr}")
            }
            ToolchainError::Io(e) => write!(f, "toolchain I/O error: {e}"),
        }
    }
}

impl std::error::Error for ToolchainError {}

impl From<std::io::Error> for ToolchainError {
    fn from(e: std::io::Error) -> Self {
        ToolchainError::Io(e)
    }
}

/// Whether a program runs and reports a version (used for presence detection).
fn program_available(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `true` if `avr-gcc` and `avr-objcopy` are both on `PATH`.
pub fn avr_gcc_available() -> bool {
    program_available("avr-gcc") && program_available("avr-objcopy")
}

/// `true` if `arduino-cli` is on `PATH`.
pub fn arduino_cli_available() -> bool {
    program_available("arduino-cli")
}

/// Detect a usable toolchain, preferring `avr-gcc` (hermetic, no board cores to
/// install) over `arduino-cli`. Returns `None` when neither is present — the
/// signal a caller uses to **skip**.
pub fn detect() -> Option<Toolchain> {
    if avr_gcc_available() {
        Some(Toolchain::AvrGcc)
    } else if arduino_cli_available() {
        Some(Toolchain::ArduinoCli)
    } else {
        None
    }
}

/// Run `cmd`, mapping a spawn failure and a non-zero exit into a
/// [`ToolchainError`].
fn run(stage: &'static str, program: &str, cmd: &mut Command) -> Result<(), ToolchainError> {
    let output = cmd.output().map_err(|source| ToolchainError::Spawn {
        program: program.to_string(),
        source,
    })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(ToolchainError::Stage {
            stage,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Compile a C source for `mcu` (e.g. `"atmega328p"`) to Intel HEX bytes with
/// `avr-gcc` + `avr-objcopy`, using `work_dir` as scratch space.
///
/// Compiled `-nostartfiles` so the reset vector is the sketch's own entry (no
/// avr-libc C runtime, which would use flash features such as `LPM` that the
/// modelled core does not implement). A freestanding sketch that drives GPIO
/// and loops therefore runs directly on the emulator.
pub fn compile_c_to_hex(
    source: &Path,
    mcu: &str,
    work_dir: &Path,
) -> Result<Vec<u8>, ToolchainError> {
    std::fs::create_dir_all(work_dir)?;
    let elf = work_dir.join("firmware.elf");
    let hex = work_dir.join("firmware.hex");

    run(
        "avr-gcc",
        "avr-gcc",
        Command::new("avr-gcc")
            .arg(format!("-mmcu={mcu}"))
            .args(["-Os", "-ffreestanding", "-nostartfiles", "-nostdlib"])
            .arg("-o")
            .arg(&elf)
            .arg(source),
    )?;
    run(
        "avr-objcopy",
        "avr-objcopy",
        Command::new("avr-objcopy")
            .args(["-O", "ihex", "-R", ".eeprom"])
            .arg(&elf)
            .arg(&hex),
    )?;

    Ok(std::fs::read(&hex)?)
}

/// Compile an Arduino `.ino` sketch with `arduino-cli` for `fqbn`
/// (e.g. `"arduino:avr:uno"`), returning the produced Intel HEX bytes.
///
/// The sketch is copied into a `<name>/<name>.ino` directory (as `arduino-cli`
/// requires) under `work_dir`, then `arduino-cli compile` is run with its hex
/// output captured. This path needs the matching board core installed.
pub fn compile_ino_to_hex(
    source: &Path,
    fqbn: &str,
    work_dir: &Path,
) -> Result<Vec<u8>, ToolchainError> {
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("sketch");
    let sketch_dir = work_dir.join(stem);
    let out_dir = work_dir.join("build");
    std::fs::create_dir_all(&sketch_dir)?;
    std::fs::create_dir_all(&out_dir)?;
    // arduino-cli requires the sketch file to be named after its directory.
    let sketch_ino = sketch_dir.join(format!("{stem}.ino"));
    std::fs::copy(source, &sketch_ino)?;

    run(
        "arduino-cli",
        "arduino-cli",
        Command::new("arduino-cli")
            .arg("compile")
            .args(["--fqbn", fqbn])
            .arg("--output-dir")
            .arg(&out_dir)
            .arg(&sketch_dir),
    )?;

    // arduino-cli writes `<stem>.ino.hex` (sometimes `<stem>.hex`) into out_dir.
    let hex = find_hex(&out_dir)?;
    Ok(std::fs::read(hex)?)
}

/// Find the first `.hex` file in `dir`.
fn find_hex(dir: &Path) -> Result<PathBuf, ToolchainError> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("hex") {
            return Ok(path);
        }
    }
    Err(ToolchainError::Stage {
        stage: "arduino-cli",
        stderr: format!("no .hex produced in {}", dir.display()),
    })
}

/// Detect a toolchain and compile `source` (a `.c`/`.ino` sketch) to Intel HEX
/// bytes for an ATmega328P-class `mcu`, using `work_dir` as scratch space.
///
/// Dispatches on the file extension and the detected toolchain: `.ino` prefers
/// `arduino-cli`; `.c` uses `avr-gcc`. Returns [`ToolchainError::NotFound`] when
/// no toolchain is installed (the caller should skip).
pub fn compile_to_hex(
    source: &Path,
    mcu: &str,
    work_dir: &Path,
) -> Result<Vec<u8>, ToolchainError> {
    let is_ino = source.extension().and_then(|e| e.to_str()) == Some("ino");
    match detect() {
        None => Err(ToolchainError::NotFound),
        Some(Toolchain::ArduinoCli) if is_ino => {
            compile_ino_to_hex(source, &fqbn_for(mcu), work_dir)
        }
        // avr-gcc (preferred) compiles C directly; also used for a .c sketch
        // even when only arduino-cli-style naming was expected.
        Some(Toolchain::AvrGcc) => compile_c_to_hex(source, mcu, work_dir),
        Some(Toolchain::ArduinoCli) => compile_ino_to_hex(source, &fqbn_for(mcu), work_dir),
    }
}

/// A best-effort Arduino FQBN for a known `mcu` (for the `arduino-cli` path).
fn fqbn_for(mcu: &str) -> String {
    match mcu {
        "atmega2560" => "arduino:avr:mega",
        "attiny85" => "ATTinyCore:avr:attinyx5",
        _ => "arduino:avr:uno",
    }
    .to_string()
}
