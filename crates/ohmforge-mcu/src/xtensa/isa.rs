//! The decoded-instruction type for the supported Xtensa (LX6) subset, and its
//! operand shapes.
//!
//! [`Instr`] is what [`decode`](super::decode::decode) produces and
//! [`Xtensa`](super::core::Xtensa) executes. Xtensa is a 32-bit load/store RISC
//! with **variable-length** instructions: the core (RRR/RRI8/RI16/CALL/BRI12)
//! encodings are three bytes, and the Code-Density option adds two-byte "narrow"
//! forms. Instructions are stored little-endian, so the first byte holds bits
//! `7:0`, the second `15:8`, and the third `23:16`.
//!
//! Register operands are plain address-register numbers (`a0..a15`). Immediates
//! are already extracted and, where the encoding scales or sign-extends them,
//! pre-processed: the load/store [`off`](Instr::Load) is the final byte offset,
//! branch and jump offsets are stored as signed byte offsets relative to the
//! branch instruction's own address (Xtensa adds `4` to that address to form the
//! target — see [`core`](super::core)), and [`Movi`](Instr::Movi) /
//! [`Addi`](Instr::Addi) carry their sign-extended immediate.
//!
//! The subset deliberately models the **CALL0 (windowless) ABI**: a flat
//! sixteen-entry register file where `a0` is the link register. The windowed
//! call machinery (register-window rotation, `CALL4/8/12`, `ENTRY`, `RETW`) is
//! not modelled; see [`decode`](mod@super::decode) for the exact coverage.

/// The width of a memory access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// 8-bit.
    Byte,
    /// 16-bit.
    Half,
    /// 32-bit.
    Word,
}

/// A decoded Xtensa instruction (the supported subset).
///
/// Field conventions follow the Xtensa manual's operand names: `r` is the RRR
/// destination, `s`/`t` are source registers (RRR second/third operands and the
/// RRI8 base/data registers), and immediates are pre-extracted. Any encoding
/// outside the modelled subset decodes to [`Instr::Unknown`], carrying the raw
/// instruction word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instr {
    // --- RRR arithmetic / logical (`ar = as op at`) ---
    /// `ADD ar, as, at` — `a[r] = a[s] + a[t]`.
    Add { r: u8, s: u8, t: u8 },
    /// `SUB ar, as, at` — `a[r] = a[s] - a[t]`.
    Sub { r: u8, s: u8, t: u8 },
    /// `AND ar, as, at` — `a[r] = a[s] & a[t]`.
    And { r: u8, s: u8, t: u8 },
    /// `OR ar, as, at` — `a[r] = a[s] | a[t]` (`MOV ar, as` is `OR ar, as, as`).
    Or { r: u8, s: u8, t: u8 },
    /// `XOR ar, as, at` — `a[r] = a[s] ^ a[t]`.
    Xor { r: u8, s: u8, t: u8 },

    // --- RRI8 immediate ALU / move ---
    /// `MOVI at, imm` — `a[t] = imm` (12-bit sign-extended immediate).
    Movi { t: u8, imm: i32 },
    /// `ADDI at, as, imm` — `a[t] = a[s] + imm` (8-bit sign-extended immediate).
    Addi { t: u8, s: u8, imm: i32 },

    // --- Loads / stores ---
    /// A load: `a[t] = mem[a[s] + off]` of `size`, sign-extending a sub-word load
    /// when `signed` (`L16SI`); the base/offset form a byte address.
    Load {
        /// Destination register.
        t: u8,
        /// Base register.
        s: u8,
        /// Byte offset (already scaled by the access width).
        off: u32,
        /// Access width.
        size: Size,
        /// Sign-extend a sub-word load.
        signed: bool,
    },
    /// A store: `mem[a[s] + off] = a[t]` of `size`.
    Store {
        /// Source register.
        t: u8,
        /// Base register.
        s: u8,
        /// Byte offset (already scaled by the access width).
        off: u32,
        /// Access width.
        size: Size,
    },
    /// `L32R at, label` — load a 32-bit word from the PC-relative literal pool.
    /// `imm16` is the raw encoded field; the effective address is formed in
    /// [`core`](super::core) (always at or below the aligned instruction PC).
    L32r { t: u8, imm16: u16 },

    // --- Control flow ---
    /// `J label` — unconditional jump by a signed byte `offset`.
    J { offset: i32 },
    /// `CALL0 label` — windowless call: `a0 = return address`, jump by the
    /// (word-scaled) signed byte `offset` from the aligned PC.
    Call0 { offset: i32 },
    /// `CALLX0 as` — windowless indirect call: `a0 = return address`, `PC = a[s]`.
    Callx0 { s: u8 },
    /// `RET` / `RET.N` — return: `PC = a0`.
    Ret,
    /// `JX as` — indirect jump: `PC = a[s]`.
    Jx { s: u8 },
    /// `BEQZ as, label` — branch by `offset` if `a[s] == 0`.
    Beqz { s: u8, offset: i32 },
    /// `BNEZ as, label` — branch by `offset` if `a[s] != 0`.
    Bnez { s: u8, offset: i32 },
    /// `BLTZ as, label` — branch by `offset` if `a[s] < 0` (signed).
    Bltz { s: u8, offset: i32 },
    /// `BGEZ as, label` — branch by `offset` if `a[s] >= 0` (signed).
    Bgez { s: u8, offset: i32 },

    // --- Code-density (narrow, two-byte) ---
    /// `MOV.N at, as` — `a[t] = a[s]`.
    MovN { t: u8, s: u8 },
    /// `NOP` / `NOP.N` — no operation.
    Nop,

    /// An encoding outside the modelled subset; carries the raw instruction word
    /// (up to 24 bits). See [`decode`](mod@super::decode) for the documented TODOs.
    Unknown(u32),
}
