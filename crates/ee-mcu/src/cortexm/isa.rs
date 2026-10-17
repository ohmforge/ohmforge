//! The decoded-instruction type for the ARMv6-M (Thumb) subset, and its operand
//! shapes.
//!
//! [`Instr`] is what [`decode`](super::decode::decode) produces and
//! [`CortexM`](super::core::CortexM) executes. Register operands are plain
//! register numbers (`0..16`, where `13`=SP, `14`=LR, `15`=PC); immediates are
//! already extracted and (where the encoding scales them) pre-scaled; branch
//! offsets are signed **byte** offsets relative to the branch's `PC` value
//! (the instruction address + 4, as Thumb defines it).
//!
//! ARMv6-M is the Thumb subset the Cortex-M0/M0+ implements: almost every
//! instruction is a single 16-bit halfword, the exception being the 32-bit
//! `BL` (and a handful of system encodings this core does not model).

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

/// How a load/store forms its effective address off the base register `rn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddrMode {
    /// `[rn, #imm]` — a (pre-scaled) immediate byte offset.
    Imm(u32),
    /// `[rn, rm]` — a register offset.
    Reg(u8),
}

/// A load or store. `rn == 15` is a PC-relative literal access (the base is the
/// aligned `PC`); `rn == 13` is an SP-relative access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mem {
    /// `true` = load (into `rt`), `false` = store (from `rt`).
    pub load: bool,
    /// Access width.
    pub size: Size,
    /// Sign-extend a sub-word load (`LDRSB`/`LDRSH`); ignored for stores and
    /// word accesses.
    pub signed: bool,
    /// The transferred register.
    pub rt: u8,
    /// The base register.
    pub rn: u8,
    /// The offset addressing mode.
    pub addr: AddrMode,
}

/// A decoded ARMv6-M (Thumb) instruction.
///
/// The `setflags` fields capture whether the encoding updates the APSR
/// condition flags (the `S` suffix): outside an IT block — which ARMv6-M does
/// not have — the data-processing register forms always set flags, while the
/// high-register `ADD`/`MOV` forms never do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instr {
    /// `MOV`/`MOVS rd, #imm`.
    MovImm { rd: u8, imm: u32, setflags: bool },
    /// `MOV`/`MOVS rd, rm` (register copy; high-reg form never sets flags).
    MovReg { rd: u8, rm: u8, setflags: bool },

    /// `ADD`/`ADDS rd, rn, rm`.
    AddReg {
        rd: u8,
        rn: u8,
        rm: u8,
        setflags: bool,
    },
    /// `ADD`/`ADDS rd, rn, #imm`.
    AddImm {
        rd: u8,
        rn: u8,
        imm: u32,
        setflags: bool,
    },
    /// `SUB`/`SUBS rd, rn, rm`.
    SubReg {
        rd: u8,
        rn: u8,
        rm: u8,
        setflags: bool,
    },
    /// `SUB`/`SUBS rd, rn, #imm`.
    SubImm {
        rd: u8,
        rn: u8,
        imm: u32,
        setflags: bool,
    },
    /// `RSBS rd, rn, #0` — negate.
    Rsb { rd: u8, rn: u8 },
    /// `ADC rd, rm` (= `ADCS rd, rd, rm`).
    Adc { rd: u8, rm: u8 },
    /// `SBC rd, rm` (= `SBCS rd, rd, rm`).
    Sbc { rd: u8, rm: u8 },

    /// `CMP rn, rm`.
    CmpReg { rn: u8, rm: u8 },
    /// `CMP rn, #imm`.
    CmpImm { rn: u8, imm: u32 },
    /// `CMN rn, rm`.
    Cmn { rn: u8, rm: u8 },
    /// `TST rn, rm`.
    Tst { rn: u8, rm: u8 },

    /// `ANDS rd, rm`.
    And { rd: u8, rm: u8 },
    /// `ORRS rd, rm`.
    Orr { rd: u8, rm: u8 },
    /// `EORS rd, rm`.
    Eor { rd: u8, rm: u8 },
    /// `BICS rd, rm`.
    Bic { rd: u8, rm: u8 },
    /// `MVNS rd, rm`.
    Mvn { rd: u8, rm: u8 },
    /// `MULS rd, rm` (= `rd = rn * rd`).
    Mul { rd: u8, rm: u8 },

    /// `LSL`/`LSLS rd, rm, #imm` (a zero immediate is a plain `MOV` that still
    /// sets N/Z and leaves C untouched).
    LslImm { rd: u8, rm: u8, imm: u8 },
    /// `LSRS rd, rm, #imm` (imm `0` encodes a shift of 32).
    LsrImm { rd: u8, rm: u8, imm: u8 },
    /// `ASRS rd, rm, #imm` (imm `0` encodes a shift of 32).
    AsrImm { rd: u8, rm: u8, imm: u8 },
    /// `LSLS rd, rm` — shift by the low byte of `rm`.
    LslReg { rd: u8, rm: u8 },
    /// `LSRS rd, rm`.
    LsrReg { rd: u8, rm: u8 },
    /// `ASRS rd, rm`.
    AsrReg { rd: u8, rm: u8 },
    /// `RORS rd, rm`.
    RorReg { rd: u8, rm: u8 },

    /// `SXTH`/`SXTB`/`UXTH`/`UXTB rd, rm`.
    Extend {
        rd: u8,
        rm: u8,
        size: Size,
        signed: bool,
    },
    /// `REV`/`REV16`/`REVSH rd, rm`, selected by `op` (`0`/`1`/`2`).
    Rev { rd: u8, rm: u8, op: u8 },

    /// A load or store (see [`Mem`]).
    Mem(Mem),
    /// `ADR rd, #imm` / `ADD rd, PC, #imm` — aligned-PC + immediate.
    Adr { rd: u8, imm: u32 },
    /// `ADD rd, SP, #imm`.
    AddSpImm { rd: u8, imm: u32 },
    /// `ADD SP, SP, #imm`.
    AddSp { imm: u32 },
    /// `SUB SP, SP, #imm`.
    SubSp { imm: u32 },

    /// `PUSH {reglist}` — `regs` is the low-register bitmap, `lr` pushes LR.
    Push { regs: u8, lr: bool },
    /// `POP {reglist}` — `regs` is the low-register bitmap, `pc` pops PC.
    Pop { regs: u8, pc: bool },
    /// `STMIA rn!, {reglist}`.
    Stm { rn: u8, regs: u8 },
    /// `LDMIA rn!, {reglist}`.
    Ldm { rn: u8, regs: u8 },

    /// `B` — unconditional branch (signed byte offset).
    B { offset: i32 },
    /// `B<cond>` — conditional branch (`cond` is the 4-bit condition code).
    Bcond { cond: u8, offset: i32 },
    /// `BL` — branch with link (the only 32-bit encoding modelled).
    Bl { offset: i32 },
    /// `BX rm` — branch (and exchange); `BX LR` returns.
    Bx { rm: u8 },
    /// `BLX rm` — branch with link to a register target.
    Blx { rm: u8 },
    /// `SVC #imm` — supervisor call (raises the SVCall exception).
    Svc { imm: u8 },

    /// `NOP` (and the other hint instructions, treated as no-ops).
    Nop,
    /// An encoding outside the modelled subset; executes as a no-op under debug
    /// assertion. See [`decode`](mod@super::decode) for the documented TODOs.
    Unknown(u16),
}

/// Register number of the stack pointer (`SP` = `r13`).
pub const SP: u8 = 13;
/// Register number of the link register (`LR` = `r14`).
pub const LR: u8 = 14;
/// Register number of the program counter (`PC` = `r15`).
pub const PC: u8 = 15;
