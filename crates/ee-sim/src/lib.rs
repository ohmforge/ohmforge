//! # ee-sim
//!
//! The analog / mixed-signal simulation engine — the heart of the "emulator."
//!
//! Builds a Modified Nodal Analysis system from the [`ee_core::Circuit`], solves
//! it (Newton–Raphson for nonlinear devices, numerical integration for
//! transient), and streams out typed signals. See `docs/ARCHITECTURE.md` §
//! "The simulation core."
//!
//! Roadmap: M0 = linear MNA + DC op-point + transient (Backward Euler);
//! M1 = nonlinear devices + AC/DC-sweep; M2 = noise/Monte-Carlo. (docs/ROADMAP.md)

/// The analyses the engine can run.
#[derive(Debug, Clone)]
pub enum Analysis {
    /// DC operating point.
    OperatingPoint,
    /// Time-domain transient over `duration` seconds.
    Transient { duration: f64 },
    /// Small-signal AC sweep (Bode).
    Ac {
        f_start: f64,
        f_stop: f64,
        points_per_decade: u32,
    },
}

/// TODO(M0): stamp the circuit into a sparse MNA matrix and solve.
pub struct Engine;
