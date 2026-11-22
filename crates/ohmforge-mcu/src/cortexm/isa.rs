//! The decoded-instruction type for the ARMv6-M / ARMv7-M (Thumb / Thumb-2)
//! subset, and its operand shapes.
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
//! `BL`. The Cortex-M3/M4 add the **Thumb-2** 32-bit encodings — the variants
//! after [`Instr::Nop`] below — for data-processing with modified immediates and
//! shifted registers, the long/accumulating multiplies, bitfield operations,
//! `MOVW`/`MOVT`, `CLZ`/`RBIT`, `LDRD`/`STRD`, wide `LDM`/`STM`, `MRS`/`MSR`,
//! and the wider branch ranges, plus the 16-bit `IT` block.

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
    /// `[rn, #imm]` — a (pre-scaled) immediate byte offset, no writeback.
    Imm(u32),
    /// `[rn, rm]` — a register offset (unscaled).
    Reg(u8),
    /// `[rn, rm, LSL #shift]` — a register offset scaled by a left shift (the
    /// Thumb-2 scaled-register form; `shift` is `0..=3`).
    RegShift { rm: u8, shift: u8 },
    /// An immediate offset with explicit index/writeback control (the Thumb-2
    /// pre-/post-indexed forms). The accessed address is `rn ± imm` when
    /// `index` (pre-index) and `rn` otherwise (post-index); when `wback` the
    /// base register is updated to `rn ± imm`. `add` selects `+`/`-`.
    ImmWb {
        imm: u32,
        add: bool,
        index: bool,
        wback: bool,
    },
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

    /// `CBZ`/`CBNZ rn, #imm` — compare the register against zero and branch
    /// forward by `imm` bytes (always a positive offset). `nonzero` selects
    /// `CBNZ` (branch when `rn != 0`); otherwise `CBZ` (branch when `rn == 0`).
    Cb { nonzero: bool, rn: u8, imm: u32 },

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

    // --- Thumb-2 (ARMv7-M) 32-bit encodings ---
    /// Data-processing with a *modified immediate* (`ThumbExpandImm`). `op` is the
    /// 4-bit opcode (`0000`=AND, `0001`=BIC, `0010`=ORR/MOV, `0011`=ORN/MVN,
    /// `0100`=EOR, `1000`=ADD/CMN, `1010`=ADC, `1011`=SBC, `1101`=SUB/CMP,
    /// `1110`=RSB). `rn == 15` with ORR/ORN is MOV/MVN; `rd == 15` with S and a
    /// comparison opcode is TST/TEQ/CMN/CMP. `carry` is the shifter carry-out of
    /// the immediate expansion (`None` = leave `C` unchanged), used by the logical
    /// forms when they set flags.
    DpImm {
        op: u8,
        rd: u8,
        rn: u8,
        imm: u32,
        setflags: bool,
        carry: Option<bool>,
    },
    /// Data-processing with a *shifted register*. Same `op` map as [`Instr::DpImm`];
    /// `stype`/`samt` are the shift type (`0`=LSL,`1`=LSR,`2`=ASR,`3`=ROR/RRX) and
    /// decoded amount applied to `rm`.
    DpShiftReg {
        op: u8,
        rd: u8,
        rn: u8,
        rm: u8,
        stype: u8,
        samt: u8,
        setflags: bool,
    },
    /// Register-controlled shift (`LSL`/`LSR`/`ASR`/`ROR rd, rn, rm`), `stype` as
    /// above.
    ShiftReg {
        rd: u8,
        rn: u8,
        rm: u8,
        stype: u8,
        setflags: bool,
    },

    /// `MOVW rd, #imm16` — zero-extended 16-bit immediate.
    Movw { rd: u8, imm: u32 },
    /// `MOVT rd, #imm16` — set the top halfword, keep the low halfword.
    Movt { rd: u8, imm: u32 },

    /// `UBFX rd, rn, #lsb, #width` — unsigned bitfield extract.
    Ubfx { rd: u8, rn: u8, lsb: u8, width: u8 },
    /// `SBFX rd, rn, #lsb, #width` — signed bitfield extract.
    Sbfx { rd: u8, rn: u8, lsb: u8, width: u8 },
    /// `BFI rd, rn, #lsb, #width` — bitfield insert.
    Bfi { rd: u8, rn: u8, lsb: u8, width: u8 },
    /// `BFC rd, #lsb, #width` — bitfield clear.
    Bfc { rd: u8, lsb: u8, width: u8 },

    /// `MUL rd, rn, rm` (32-bit, never sets flags).
    Mul32 { rd: u8, rn: u8, rm: u8 },
    /// `MLA rd, rn, rm, ra` — `rd = ra + rn*rm`.
    Mla { rd: u8, rn: u8, rm: u8, ra: u8 },
    /// `MLS rd, rn, rm, ra` — `rd = ra - rn*rm`.
    Mls { rd: u8, rn: u8, rm: u8, ra: u8 },
    /// `UMULL rdlo, rdhi, rn, rm` — unsigned 32×32→64 multiply.
    Umull { rdlo: u8, rdhi: u8, rn: u8, rm: u8 },
    /// `SMULL rdlo, rdhi, rn, rm` — signed 32×32→64 multiply.
    Smull { rdlo: u8, rdhi: u8, rn: u8, rm: u8 },

    /// `UDIV rd, rn, rm` — unsigned divide (`rd = rn / rm`; a zero divisor
    /// yields `0`, matching ARMv7-M without divide-by-zero trapping).
    Udiv { rd: u8, rn: u8, rm: u8 },
    /// `SDIV rd, rn, rm` — signed divide (`rd = rn / rm`, rounding toward zero;
    /// a zero divisor yields `0`).
    Sdiv { rd: u8, rn: u8, rm: u8 },

    /// `CLZ rd, rm` — count leading zeros.
    Clz { rd: u8, rm: u8 },
    /// `RBIT rd, rm` — reverse the bit order.
    Rbit { rd: u8, rm: u8 },

    /// `LDRD`/`STRD rt, rt2, [rn, #±imm]{!}` — load/store two words.
    MemD {
        load: bool,
        rt: u8,
        rt2: u8,
        rn: u8,
        imm: u32,
        add: bool,
        index: bool,
        wback: bool,
    },
    /// Wide `STM`/`LDM` (`regs` is the full 16-bit register list). `db` selects the
    /// decrement-before form (`STMDB`/`LDMDB`, and thus `PUSH.W`/`POP.W`).
    StmW {
        rn: u8,
        regs: u16,
        wback: bool,
        db: bool,
    },
    /// Wide `LDM` — see [`StmW`](Instr::StmW).
    LdmW {
        rn: u8,
        regs: u16,
        wback: bool,
        db: bool,
    },

    /// `MRS rd, <sysm>` — read a special register.
    Mrs { rd: u8, sysm: u8 },
    /// `MSR <sysm>, rn` — write a special register.
    Msr { rn: u8, sysm: u8 },

    /// `IT{x{y{z}}} <firstcond>` — begin an If-Then block (`mask != 0`).
    It { firstcond: u8, mask: u8 },

    /// An encoding outside the modelled subset. Executing it halts the core
    /// gracefully (see [`CortexM::halted`](super::core::CortexM::halted)) rather
    /// than panicking. See [`decode`](mod@super::decode) for the documented
    /// TODOs.
    Unknown(u16),
}

/// Register number of the stack pointer (`SP` = `r13`).
pub const SP: u8 = 13;
/// Register number of the link register (`LR` = `r14`).
pub const LR: u8 = 14;
/// Register number of the program counter (`PC` = `r15`).
pub const PC: u8 = 15;
