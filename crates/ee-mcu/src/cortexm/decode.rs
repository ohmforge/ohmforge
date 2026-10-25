//! A table-driven decoder from Thumb / Thumb-2 halfwords to [`Instr`].
//!
//! [`decode`] takes the opcode halfword `w0` and the following halfword `w1` and
//! returns the decoded [`Instr`] together with how many **bytes** it occupies
//! (`2` or `4`).
//!
//! # 16- vs 32-bit
//!
//! The width is settled by the top five bits of the first halfword, exactly as
//! the ARM ARM defines: `w0[15:11]` in `{0b11101, 0b11110, 0b11111}` marks a
//! 32-bit Thumb-2 instruction (4 bytes, consuming `w1`); everything else is a
//! 16-bit Thumb instruction (2 bytes). (`0b11100` — the plain 16-bit `B` — is
//! deliberately *not* in that set.) The 16-bit path is the ARMv6-M decoder the
//! Cortex-M0+ uses; the 32-bit path (`decode_thumb32`) adds the ARMv7-M
//! Thumb-2 encodings the Cortex-M3/M4 bring.
//!
//! The 16-bit decode is structured by the top bits of `w0`, mirroring the
//! ARMv6-M ARM's Thumb encoding map:
//!
//! - `00xxxx` — shift by immediate, add/subtract, move/compare immediate.
//! - `010000` — data-processing (register).
//! - `010001` — special data processing + branch/exchange (high registers).
//! - `01001x` — `LDR` (literal).
//! - `0101xx`/`011xxx`/`1000xx` — load/store (register and immediate offset).
//! - `1001xx` — load/store (SP-relative).
//! - `1010xx` — `ADR` / `ADD (SP + imm)`.
//! - `1011xx` — miscellaneous (stack ops, extends, byte-reversals, hints…).
//! - `1100xx` — `STMIA` / `LDMIA`.
//! - `1101xx` — conditional branch, `SVC`.
//! - `11100x` — unconditional branch.
//! - `1011 1111` — `IT` blocks (and the hint space, as `NOP`).
//! - `11101x`/`11110x`/`11111x` — the 32-bit Thumb-2 space (`decode_thumb32`).
//!
//! # Coverage
//!
//! ## ARMv6-M (16-bit)
//!
//! The full data-processing register group (`AND EOR LSL LSR ASR ADC SBC ROR TST
//! RSB CMP CMN ORR MUL BIC MVN`); `MOV`/`ADD`/`SUB`/`CMP` in their immediate,
//! register and high-register forms; the shift-by-immediate moves; `LDR`/`STR`
//! (+ halfword/byte, signed loads) in literal, immediate, register and
//! SP-relative forms; `ADR`; `ADD`/`SUB SP`; `PUSH`/`POP`; `STMIA`/`LDMIA`;
//! `SXT*`/`UXT*`; `REV*`; `B`/`B<cond>`/`BL`/`BX`/`BLX`; `SVC`; `IT`; and
//! `NOP`/hints.
//!
//! ## ARMv7-M / Thumb-2 (32-bit, Cortex-M3/M4)
//!
//! Data-processing with a *modified immediate* and with a *shifted register*
//! (`AND BIC ORR ORN EOR ADD ADC SBC SUB RSB`, plus the `MOV`/`MVN` and
//! `TST`/`TEQ`/`CMN`/`CMP` aliases); register-controlled shifts
//! (`LSL`/`LSR`/`ASR`/`ROR`); the plain-binary-immediate `ADDW`/`SUBW`,
//! `MOVW`/`MOVT`, and the bitfield ops `UBFX`/`SBFX`/`BFI`/`BFC`; the multiplies
//! `MUL`/`MLA`/`MLS`/`UMULL`/`SMULL`; `CLZ`/`RBIT` and the wide byte-reversals;
//! `LDR`/`STR` (+ byte/half, signed) with 12-bit and register offsets;
//! `LDRD`/`STRD`; wide `LDM`/`STM` (IA/DB, i.e. `PUSH.W`/`POP.W`); the wide
//! branches `B.W`/`B<cond>.W`/`BL`; and `MRS`/`MSR`.
//!
//! Not yet implemented (decode to [`Instr::Unknown`], a documented TODO): the
//! saturating `SSAT`/`USAT`, `SDIV`/`UDIV`, the DSP `SMLAL`/`SMLALxy` family,
//! load/store with pre/post-index + writeback or a scaled register offset,
//! `TBB`/`TBH`, `LDREX`/`STREX`, `CBZ`/`CBNZ`, all coprocessor / floating-point
//! encodings, and (as on the M0+) the barriers `DSB`/`DMB`/`ISB`, `CPS`,
//! `BKPT`, and `UDF`.

use super::isa::{AddrMode, Instr, Mem, Size};

/// Decode one instruction. Returns the instruction and its length in **bytes**
/// (`2` for a Thumb halfword, `4` for the 32-bit `BL`).
pub fn decode(w0: u16, w1: u16) -> (Instr, u8) {
    match w0 >> 10 {
        // 00xxxx: shift-by-immediate / add-subtract / move-compare-immediate.
        0b000000..=0b001111 => (decode_shift_addsub_movcmp(w0), 2),
        // 010000: data processing (register).
        0b010000 => (decode_data_processing(w0), 2),
        // 010001: special data processing + branch/exchange.
        0b010001 => (decode_special(w0), 2),
        // 01001x: LDR (literal).
        0b010010 | 0b010011 => (decode_ldr_literal(w0), 2),
        // 0101xx / 011xxx / 1000xx: load/store (register & immediate offset).
        0b010100..=0b100011 => (decode_load_store(w0), 2),
        // 1001xx: load/store (SP-relative).
        0b100100..=0b100111 => (decode_load_store_sp(w0), 2),
        // 1010xx: ADR / ADD (SP + immediate).
        0b101000..=0b101011 => (decode_adr_add_sp(w0), 2),
        // 1011xx: miscellaneous 16-bit instructions.
        0b101100..=0b101111 => (decode_misc(w0), 2),
        // 1100xx: STMIA / LDMIA.
        0b110000..=0b110011 => (decode_load_store_multiple(w0), 2),
        // 1101xx: conditional branch + SVC.
        0b110100..=0b110111 => (decode_cond_branch(w0), 2),
        // 11100x: unconditional branch (16-bit).
        0b111000 | 0b111001 => (
            Instr::B {
                offset: imm11_offset(w0),
            },
            2,
        ),
        // 11101x / 11110x / 11111x: the 32-bit Thumb-2 (ARMv7-M) encodings.
        0b111010..=0b111111 => (decode_thumb32(w0, w1), 4),
        _ => (Instr::Unknown(w0), 2),
    }
}

// --- 00xxxx: shift-by-immediate / add-subtract / move-compare-immediate ---

fn decode_shift_addsub_movcmp(w: u16) -> Instr {
    let rd = (w & 0x7) as u8;
    let rn = ((w >> 3) & 0x7) as u8;
    let imm5 = ((w >> 6) & 0x1F) as u8;
    match (w >> 11) & 0x1F {
        // 000 00/01/10: LSL/LSR/ASR by immediate.
        0b00000 => Instr::LslImm {
            rd,
            rm: rn,
            imm: imm5,
        },
        0b00001 => Instr::LsrImm {
            rd,
            rm: rn,
            imm: imm5,
        },
        0b00010 => Instr::AsrImm {
            rd,
            rm: rn,
            imm: imm5,
        },
        // 00011: add/subtract (register or 3-bit immediate).
        0b00011 => {
            let rm = ((w >> 6) & 0x7) as u8;
            let imm3 = ((w >> 6) & 0x7) as u32;
            match (w >> 9) & 0x3 {
                0b00 => Instr::AddReg {
                    rd,
                    rn,
                    rm,
                    setflags: true,
                },
                0b01 => Instr::SubReg {
                    rd,
                    rn,
                    rm,
                    setflags: true,
                },
                0b10 => Instr::AddImm {
                    rd,
                    rn,
                    imm: imm3,
                    setflags: true,
                },
                _ => Instr::SubImm {
                    rd,
                    rn,
                    imm: imm3,
                    setflags: true,
                },
            }
        }
        // 001xx: move/compare/add/subtract 8-bit immediate (rd in bits 10:8).
        0b00100 => Instr::MovImm {
            rd: ((w >> 8) & 0x7) as u8,
            imm: (w & 0xFF) as u32,
            setflags: true,
        },
        0b00101 => Instr::CmpImm {
            rn: ((w >> 8) & 0x7) as u8,
            imm: (w & 0xFF) as u32,
        },
        0b00110 => {
            let r = ((w >> 8) & 0x7) as u8;
            Instr::AddImm {
                rd: r,
                rn: r,
                imm: (w & 0xFF) as u32,
                setflags: true,
            }
        }
        0b00111 => {
            let r = ((w >> 8) & 0x7) as u8;
            Instr::SubImm {
                rd: r,
                rn: r,
                imm: (w & 0xFF) as u32,
                setflags: true,
            }
        }
        _ => Instr::Unknown(w),
    }
}

// --- 010000: data processing (register) ---

fn decode_data_processing(w: u16) -> Instr {
    let rd = (w & 0x7) as u8; // also Rdn
    let rm = ((w >> 3) & 0x7) as u8;
    match (w >> 6) & 0xF {
        0b0000 => Instr::And { rd, rm },
        0b0001 => Instr::Eor { rd, rm },
        0b0010 => Instr::LslReg { rd, rm },
        0b0011 => Instr::LsrReg { rd, rm },
        0b0100 => Instr::AsrReg { rd, rm },
        0b0101 => Instr::Adc { rd, rm },
        0b0110 => Instr::Sbc { rd, rm },
        0b0111 => Instr::RorReg { rd, rm },
        0b1000 => Instr::Tst { rn: rd, rm },
        0b1001 => Instr::Rsb { rd, rn: rm },
        0b1010 => Instr::CmpReg { rn: rd, rm },
        0b1011 => Instr::Cmn { rn: rd, rm },
        0b1100 => Instr::Orr { rd, rm },
        0b1101 => Instr::Mul { rd, rm },
        0b1110 => Instr::Bic { rd, rm },
        _ => Instr::Mvn { rd, rm },
    }
}

// --- 010001: special data processing + branch/exchange (high registers) ---

fn decode_special(w: u16) -> Instr {
    let op = (w >> 8) & 0x3;
    // High-register forms: Rm is bits 6:3, Rd/Rn is {DN:bit7, bits 2:0}.
    let rm = ((w >> 3) & 0xF) as u8;
    let rd = (((w >> 4) & 0x8) | (w & 0x7)) as u8;
    match op {
        0b00 => Instr::AddReg {
            rd,
            rn: rd,
            rm,
            setflags: false,
        },
        0b01 => Instr::CmpReg { rn: rd, rm },
        0b10 => Instr::MovReg {
            rd,
            rm,
            setflags: false,
        },
        // 11: BX / BLX (Rm in bits 6:3, bit7 selects link).
        _ => {
            if w & (1 << 7) != 0 {
                Instr::Blx { rm }
            } else {
                Instr::Bx { rm }
            }
        }
    }
}

// --- 01001x: LDR (literal) ---

fn decode_ldr_literal(w: u16) -> Instr {
    let rt = ((w >> 8) & 0x7) as u8;
    let imm = (w & 0xFF) as u32 * 4;
    Instr::Mem(Mem {
        load: true,
        size: Size::Word,
        signed: false,
        rt,
        rn: super::isa::PC,
        addr: AddrMode::Imm(imm),
    })
}

// --- 0101xx / 011xxx / 1000xx: load/store (register & immediate offset) ---

fn decode_load_store(w: u16) -> Instr {
    let rt = (w & 0x7) as u8;
    let rn = ((w >> 3) & 0x7) as u8;
    match w >> 12 {
        // 0101: load/store register offset, opcode in bits 11:9.
        0b0101 => {
            let rm = ((w >> 6) & 0x7) as u8;
            let (load, size, signed) = match (w >> 9) & 0x7 {
                0b000 => (false, Size::Word, false), // STR
                0b001 => (false, Size::Half, false), // STRH
                0b010 => (false, Size::Byte, false), // STRB
                0b011 => (true, Size::Byte, true),   // LDRSB
                0b100 => (true, Size::Word, false),  // LDR
                0b101 => (true, Size::Half, false),  // LDRH
                0b110 => (true, Size::Byte, false),  // LDRB
                _ => (true, Size::Half, true),       // LDRSH
            };
            Instr::Mem(Mem {
                load,
                size,
                signed,
                rt,
                rn,
                addr: AddrMode::Reg(rm),
            })
        }
        // 0110: STR/LDR word immediate (imm5 scaled by 4).
        0b0110 => {
            let imm = ((w >> 6) & 0x1F) as u32 * 4;
            Instr::Mem(Mem {
                load: w & (1 << 11) != 0,
                size: Size::Word,
                signed: false,
                rt,
                rn,
                addr: AddrMode::Imm(imm),
            })
        }
        // 0111: STRB/LDRB immediate (imm5 unscaled).
        0b0111 => {
            let imm = ((w >> 6) & 0x1F) as u32;
            Instr::Mem(Mem {
                load: w & (1 << 11) != 0,
                size: Size::Byte,
                signed: false,
                rt,
                rn,
                addr: AddrMode::Imm(imm),
            })
        }
        // 1000: STRH/LDRH immediate (imm5 scaled by 2).
        _ => {
            let imm = ((w >> 6) & 0x1F) as u32 * 2;
            Instr::Mem(Mem {
                load: w & (1 << 11) != 0,
                size: Size::Half,
                signed: false,
                rt,
                rn,
                addr: AddrMode::Imm(imm),
            })
        }
    }
}

// --- 1001xx: load/store (SP-relative) ---

fn decode_load_store_sp(w: u16) -> Instr {
    let rt = ((w >> 8) & 0x7) as u8;
    let imm = (w & 0xFF) as u32 * 4;
    Instr::Mem(Mem {
        load: w & (1 << 11) != 0,
        size: Size::Word,
        signed: false,
        rt,
        rn: super::isa::SP,
        addr: AddrMode::Imm(imm),
    })
}

// --- 1010xx: ADR / ADD (SP + immediate) ---

fn decode_adr_add_sp(w: u16) -> Instr {
    let rd = ((w >> 8) & 0x7) as u8;
    let imm = (w & 0xFF) as u32 * 4;
    if w & (1 << 11) == 0 {
        Instr::Adr { rd, imm } // ADD rd, PC, #imm
    } else {
        Instr::AddSpImm { rd, imm } // ADD rd, SP, #imm
    }
}

// --- 1011xx: miscellaneous 16-bit instructions ---

fn decode_misc(w: u16) -> Instr {
    let rd = (w & 0x7) as u8;
    let rm = ((w >> 3) & 0x7) as u8;
    match (w >> 6) & 0x3F {
        // 0000 0xx: ADD SP, SP, #imm7 (scaled by 4).
        0b000000 | 0b000001 => Instr::AddSp {
            imm: (w & 0x7F) as u32 * 4,
        },
        // 0000 1xx: SUB SP, SP, #imm7.
        0b000010 | 0b000011 => Instr::SubSp {
            imm: (w & 0x7F) as u32 * 4,
        },
        // 0010 00/01/10/11: SXTH / SXTB / UXTH / UXTB.
        0b001000 => Instr::Extend {
            rd,
            rm,
            size: Size::Half,
            signed: true,
        },
        0b001001 => Instr::Extend {
            rd,
            rm,
            size: Size::Byte,
            signed: true,
        },
        0b001010 => Instr::Extend {
            rd,
            rm,
            size: Size::Half,
            signed: false,
        },
        0b001011 => Instr::Extend {
            rd,
            rm,
            size: Size::Byte,
            signed: false,
        },
        // 1010 00/01/11: REV / REV16 / REVSH.
        0b101000 => Instr::Rev { rd, rm, op: 0 },
        0b101001 => Instr::Rev { rd, rm, op: 1 },
        0b101011 => Instr::Rev { rd, rm, op: 2 },
        // 010xxx: PUSH {reglist, lr}.
        0b010000..=0b010111 => Instr::Push {
            regs: (w & 0xFF) as u8,
            lr: w & (1 << 8) != 0,
        },
        // 110xxx: POP {reglist, pc}.
        0b110000..=0b110111 => Instr::Pop {
            regs: (w & 0xFF) as u8,
            pc: w & (1 << 8) != 0,
        },
        // 1011 1111: IT blocks and hints (NOP/WFI/…). `mask` (bits 3:0) selects:
        // zero is a hint (treated as NOP), non-zero begins an IT block with
        // `firstcond` in bits 7:4.
        0b111100..=0b111111 => {
            let mask = (w & 0xF) as u8;
            if mask == 0 {
                Instr::Nop
            } else {
                Instr::It {
                    firstcond: ((w >> 4) & 0xF) as u8,
                    mask,
                }
            }
        }
        _ => Instr::Unknown(w),
    }
}

// --- 1100xx: STMIA / LDMIA ---

fn decode_load_store_multiple(w: u16) -> Instr {
    let rn = ((w >> 8) & 0x7) as u8;
    let regs = (w & 0xFF) as u8;
    if w & (1 << 11) != 0 {
        Instr::Ldm { rn, regs }
    } else {
        Instr::Stm { rn, regs }
    }
}

// --- 1101xx: conditional branch + SVC ---

fn decode_cond_branch(w: u16) -> Instr {
    let cond = ((w >> 8) & 0xF) as u8;
    match cond {
        0b1110 => Instr::Unknown(w), // permanently undefined (UDF)
        0b1111 => Instr::Svc {
            imm: (w & 0xFF) as u8,
        },
        _ => {
            // 8-bit signed offset, in halfwords → bytes, relative to PC+4.
            let off = ((w & 0xFF) as i8) as i32 * 2;
            Instr::Bcond { cond, offset: off }
        }
    }
}

/// The signed byte offset of an unconditional `B` (11-bit immediate).
fn imm11_offset(w: u16) -> i32 {
    let imm11 = (w & 0x7FF) as u32;
    // Sign-extend 11 bits, then scale halfwords → bytes.
    let signed = ((imm11 << 21) as i32) >> 21;
    signed * 2
}

// --- 11101x / 11110x / 11111x: the 32-bit Thumb-2 space ---

/// Decode a 32-bit Thumb-2 instruction from its two halfwords. Dispatch follows
/// the ARMv7-M ARM §A5.3 top-level split on `w0[12:11]` (and `w1[15]` for the
/// `11110` group), mirroring the structure the 16-bit decoder uses.
fn decode_thumb32(w0: u16, w1: u16) -> Instr {
    match (w0 >> 11) & 0x3 {
        // 11101: load/store multiple & dual, and data-processing (shifted reg).
        0b01 => decode_t32_11101(w0, w1),
        // 11110: data-processing (immediate) and branches / misc control.
        0b10 => {
            if w1 & 0x8000 != 0 {
                decode_t32_branch_misc(w0, w1)
            } else if w0 & (1 << 9) == 0 {
                decode_t32_data_mod_imm(w0, w1)
            } else {
                decode_t32_data_plain_imm(w0, w1)
            }
        }
        // 11111: load/store single, data-processing (register), multiply.
        0b11 => decode_t32_11111(w0, w1),
        _ => Instr::Unknown(w0),
    }
}

/// `ThumbExpandImm_C`: expand a 12-bit modified immediate, returning the value
/// and the shifter carry-out (`None` when the expansion leaves carry unchanged).
fn thumb_expand_imm_c(val12: u32) -> (u32, Option<bool>) {
    if (val12 >> 10) & 0x3 == 0 {
        let imm8 = val12 & 0xFF;
        let result = match (val12 >> 8) & 0x3 {
            0b00 => imm8,
            0b01 => (imm8 << 16) | imm8,
            0b10 => (imm8 << 24) | (imm8 << 8),
            _ => (imm8 << 24) | (imm8 << 16) | (imm8 << 8) | imm8,
        };
        (result, None)
    } else {
        let unrotated = 0x80 | (val12 & 0x7F);
        let rot = (val12 >> 7) & 0x1F;
        let result = unrotated.rotate_right(rot);
        (result, Some(result >> 31 != 0))
    }
}

// --- 11110 i 0: data-processing (modified immediate) ---

fn decode_t32_data_mod_imm(w0: u16, w1: u16) -> Instr {
    let op = ((w0 >> 5) & 0xF) as u8;
    let rn = (w0 & 0xF) as u8;
    let setflags = w0 & (1 << 4) != 0;
    let rd = ((w1 >> 8) & 0xF) as u8;
    let i = ((w0 >> 10) & 0x1) as u32;
    let imm3 = ((w1 >> 12) & 0x7) as u32;
    let imm8 = (w1 & 0xFF) as u32;
    let (imm, carry) = thumb_expand_imm_c((i << 11) | (imm3 << 8) | imm8);
    Instr::DpImm {
        op,
        rd,
        rn,
        imm,
        setflags,
        carry,
    }
}

// --- 11110 i 1: data-processing (plain binary immediate) ---

fn decode_t32_data_plain_imm(w0: u16, w1: u16) -> Instr {
    let op = ((w0 >> 4) & 0x1F) as u8;
    let rn = (w0 & 0xF) as u8;
    let rd = ((w1 >> 8) & 0xF) as u8;
    let i = ((w0 >> 10) & 0x1) as u32;
    let imm3 = ((w1 >> 12) & 0x7) as u32;
    let imm8 = (w1 & 0xFF) as u32;
    let imm12 = (i << 11) | (imm3 << 8) | imm8;
    // lsb/width share the imm3:imm2 / bits layout for the bitfield ops.
    let imm2 = ((w1 >> 6) & 0x3) as u8;
    let lsb = ((imm3 as u8) << 2) | imm2;
    let widthm1 = (w1 & 0x1F) as u8;
    match op {
        // ADDW rd, rn, #imm12 (ADR if rn == 15).
        0b00000 => Instr::AddImm {
            rd,
            rn,
            imm: imm12,
            setflags: false,
        },
        // SUBW rd, rn, #imm12.
        0b01010 => Instr::SubImm {
            rd,
            rn,
            imm: imm12,
            setflags: false,
        },
        // MOVW rd, #imm16 (imm4 = rn field).
        0b00100 => Instr::Movw {
            rd,
            imm: ((rn as u32) << 12) | imm12,
        },
        // MOVT rd, #imm16.
        0b01100 => Instr::Movt {
            rd,
            imm: ((rn as u32) << 12) | imm12,
        },
        // SBFX rd, rn, #lsb, #width.
        0b10100 => Instr::Sbfx {
            rd,
            rn,
            lsb,
            width: widthm1 + 1,
        },
        // UBFX rd, rn, #lsb, #width.
        0b11100 => Instr::Ubfx {
            rd,
            rn,
            lsb,
            width: widthm1 + 1,
        },
        // BFI rd, rn, #lsb, #width (BFC if rn == 15); `widthm1` here is `msb`.
        0b10110 => {
            let width = widthm1.wrapping_sub(lsb).wrapping_add(1);
            if rn == 15 {
                Instr::Bfc { rd, lsb, width }
            } else {
                Instr::Bfi { rd, rn, lsb, width }
            }
        }
        _ => Instr::Unknown(w0), // SSAT/USAT and the rest: TODO.
    }
}

// --- 11110 ... (w1[15]=1): branches and miscellaneous control ---

fn decode_t32_branch_misc(w0: u16, w1: u16) -> Instr {
    let op1 = ((w1 >> 12) & 0x7) as u8; // w1 bits 14:12
    let s = ((w0 >> 10) & 0x1) as u32;
    let imm11 = (w1 & 0x7FF) as u32;
    // op1 x1x → BL / BLX (the link forms use w1[14]=1).
    if w1 & (1 << 14) != 0 {
        let imm10 = (w0 & 0x3FF) as u32;
        let j1 = ((w1 >> 13) & 0x1) as u32;
        let j2 = ((w1 >> 11) & 0x1) as u32;
        let i1 = 1 - (j1 ^ s);
        let i2 = 1 - (j2 ^ s);
        let imm = (s << 24) | (i1 << 23) | (i2 << 22) | (imm10 << 12) | (imm11 << 1);
        let offset = ((imm << 7) as i32) >> 7;
        return Instr::Bl { offset };
    }
    // op1 == 0b010 with w0[10:4] == 0111 0xx → MSR/MRS/hints/barriers.
    let op = (w0 >> 4) & 0x7F;
    if (0b0111000..=0b0111111).contains(&op) {
        // MSR (op=0111000x) and MRS (op=0111111x), plus hint/barrier space.
        if op >> 1 == 0b011100 {
            // MSR <spec>, Rn.
            return Instr::Msr {
                rn: (w0 & 0xF) as u8,
                sysm: (w1 & 0xFF) as u8,
            };
        }
        if op >> 1 == 0b011111 {
            // MRS Rd, <spec>.
            return Instr::Mrs {
                rd: ((w1 >> 8) & 0xF) as u8,
                sysm: (w1 & 0xFF) as u8,
            };
        }
        // DSB/DMB/ISB/NOP.W and friends: treat as NOP.
        return Instr::Nop;
    }
    // B.W: encoding T4 (unconditional, w1[12]=1) or T3 (conditional, w1[12]=0).
    if w1 & (1 << 12) != 0 {
        let imm10 = (w0 & 0x3FF) as u32;
        let j1 = ((w1 >> 13) & 0x1) as u32;
        let j2 = ((w1 >> 11) & 0x1) as u32;
        let i1 = 1 - (j1 ^ s);
        let i2 = 1 - (j2 ^ s);
        let imm = (s << 24) | (i1 << 23) | (i2 << 22) | (imm10 << 12) | (imm11 << 1);
        let offset = ((imm << 7) as i32) >> 7;
        Instr::B { offset }
    } else {
        let cond = ((w0 >> 6) & 0xF) as u8;
        let imm6 = (w0 & 0x3F) as u32;
        let j1 = ((w1 >> 13) & 0x1) as u32;
        let j2 = ((w1 >> 11) & 0x1) as u32;
        // offset[20:0] = S:J2:J1:imm6:imm11:0, sign-extended from bit 20.
        let imm = (s << 20) | (j2 << 19) | (j1 << 18) | (imm6 << 12) | (imm11 << 1);
        let offset = ((imm << 11) as i32) >> 11;
        let _ = op1;
        Instr::Bcond { cond, offset }
    }
}

// --- 11101: load/store multiple & dual, data-processing (shifted register) ---

fn decode_t32_11101(w0: u16, w1: u16) -> Instr {
    match (w0 >> 9) & 0x3 {
        // 1110100: load/store multiple (w0[6]=0) or dual (w0[6]=1).
        0b00 => {
            if w0 & (1 << 6) == 0 {
                decode_t32_ldm_stm(w0, w1)
            } else {
                decode_t32_ldrd_strd(w0, w1)
            }
        }
        // 1110101: data-processing (shifted register).
        0b01 => decode_t32_data_shift_reg(w0, w1),
        // 111011x: coprocessor — not modelled.
        _ => Instr::Unknown(w0),
    }
}

fn decode_t32_ldm_stm(w0: u16, w1: u16) -> Instr {
    let rn = (w0 & 0xF) as u8;
    let wback = w0 & (1 << 5) != 0;
    let load = w0 & (1 << 4) != 0;
    // w0[8:7]: 01 = increment-after (IA), 10 = decrement-before (DB).
    let db = (w0 >> 7) & 0x3 == 0b10;
    let regs = w1;
    if load {
        Instr::LdmW {
            rn,
            regs,
            wback,
            db,
        }
    } else {
        Instr::StmW {
            rn,
            regs,
            wback,
            db,
        }
    }
}

fn decode_t32_ldrd_strd(w0: u16, w1: u16) -> Instr {
    let rn = (w0 & 0xF) as u8;
    let index = w0 & (1 << 8) != 0; // P
    let add = w0 & (1 << 7) != 0; // U
    let wback = w0 & (1 << 5) != 0; // W
    let load = w0 & (1 << 4) != 0; // L
    let rt = ((w1 >> 12) & 0xF) as u8;
    let rt2 = ((w1 >> 8) & 0xF) as u8;
    let imm = (w1 & 0xFF) as u32 * 4;
    Instr::MemD {
        load,
        rt,
        rt2,
        rn,
        imm,
        add,
        index,
        wback,
    }
}

fn decode_t32_data_shift_reg(w0: u16, w1: u16) -> Instr {
    let op = ((w0 >> 5) & 0xF) as u8;
    let rn = (w0 & 0xF) as u8;
    let setflags = w0 & (1 << 4) != 0;
    let rd = ((w1 >> 8) & 0xF) as u8;
    let rm = (w1 & 0xF) as u8;
    let stype = ((w1 >> 4) & 0x3) as u8;
    let imm3 = ((w1 >> 12) & 0x7) as u8;
    let imm2 = ((w1 >> 6) & 0x3) as u8;
    let (stype, samt) = decode_imm_shift(stype, (imm3 << 2) | imm2);
    Instr::DpShiftReg {
        op,
        rd,
        rn,
        rm,
        stype,
        samt,
        setflags,
    }
}

/// Decode an immediate shift field (`DecodeImmShift`): LSR/ASR of `#0` means 32,
/// and ROR of `#0` is RRX (signalled as `(3, 0)`).
fn decode_imm_shift(stype: u8, amt: u8) -> (u8, u8) {
    match stype {
        0 => (0, amt),
        1 => (1, if amt == 0 { 32 } else { amt }),
        2 => (2, if amt == 0 { 32 } else { amt }),
        _ => (3, amt), // ROR, or RRX when amt == 0.
    }
}

// --- 11111: load/store single, data-processing (register), multiply ---

fn decode_t32_11111(w0: u16, w1: u16) -> Instr {
    match (w0 >> 9) & 0x3 {
        // 1111100: load/store single word/halfword/byte.
        0b00 => decode_t32_load_store_single(w0, w1),
        // 1111101: data-processing (register) & multiply.
        0b01 => decode_t32_dp_reg_mul(w0, w1),
        // 111111x: coprocessor / floating-point — not modelled.
        _ => Instr::Unknown(w0),
    }
}

fn decode_t32_load_store_single(w0: u16, w1: u16) -> Instr {
    let rn = (w0 & 0xF) as u8;
    let rt = ((w1 >> 12) & 0xF) as u8;
    let signed = w0 & (1 << 8) != 0;
    let size = match (w0 >> 5) & 0x3 {
        0b00 => Size::Byte,
        0b01 => Size::Half,
        0b10 => Size::Word,
        _ => return Instr::Unknown(w0),
    };
    let load = w0 & (1 << 4) != 0;
    // w0[7]=1 → unsigned 12-bit positive offset; else imm8 index / register.
    let addr = if w0 & (1 << 7) != 0 {
        AddrMode::Imm((w1 & 0xFFF) as u32)
    } else if w1 & (1 << 11) != 0 {
        // imm8 form with P/U/W: model only the common P=1,U=1,W=0 (positive,
        // no writeback) case; otherwise leave it unmodelled.
        let p = w1 & (1 << 10) != 0;
        let u = w1 & (1 << 9) != 0;
        let wb = w1 & (1 << 8) != 0;
        if p && u && !wb {
            AddrMode::Imm((w1 & 0xFF) as u32)
        } else {
            return Instr::Unknown(w0);
        }
    } else if (w1 >> 6) & 0x3F == 0 {
        // Register offset, LSL #imm2. Model the shift-by-0 case.
        if (w1 >> 4) & 0x3 == 0 {
            AddrMode::Reg((w1 & 0xF) as u8)
        } else {
            return Instr::Unknown(w0);
        }
    } else {
        return Instr::Unknown(w0);
    };
    Instr::Mem(Mem {
        load,
        size,
        signed,
        rt,
        rn,
        addr,
    })
}

fn decode_t32_dp_reg_mul(w0: u16, w1: u16) -> Instr {
    let rn = (w0 & 0xF) as u8;
    let rm = (w1 & 0xF) as u8;
    let rd = ((w1 >> 8) & 0xF) as u8;
    match (w0 >> 4) & 0x1F {
        // 11111 0100 xxx: register-controlled shift (LSL/LSR/ASR/ROR).
        0b00000..=0b00111 if w1 & 0xF000 == 0xF000 => {
            let stype = ((w0 >> 5) & 0x3) as u8;
            let setflags = w0 & (1 << 4) != 0;
            Instr::ShiftReg {
                rd,
                rn,
                rm,
                stype,
                setflags,
            }
        }
        // 11111 0101 001x (bits[8:4]=01001): REV/REV16/RBIT/REVSH, op2 = w1[5:4].
        0b01001 => match (w1 >> 4) & 0x3 {
            0b00 => Instr::Rev { rd, rm, op: 0 },
            0b01 => Instr::Rev { rd, rm, op: 1 },
            0b10 => Instr::Rbit { rd, rm },
            _ => Instr::Rev { rd, rm, op: 2 },
        },
        // 11111 0101 011x (bits[8:4]=01011): CLZ over Rm (Rn == Rm).
        0b01011 if (w1 >> 4) & 0x3 == 0 => Instr::Clz { rd, rm },
        // 11111 0110 xxx: 32-bit multiply (MUL/MLA/MLS).
        0b10000..=0b10111 => {
            let ra = ((w1 >> 12) & 0xF) as u8;
            if (w0 >> 4) & 0x7 == 0 {
                // op1 = 000: MUL (Ra=1111) or MLA, MLS (op2 selects).
                match (w1 >> 4) & 0x3 {
                    0b00 if ra == 0b1111 => Instr::Mul32 { rd, rn, rm },
                    0b00 => Instr::Mla { rd, rn, rm, ra },
                    0b01 => Instr::Mls { rd, rn, rm, ra },
                    _ => Instr::Unknown(w0),
                }
            } else {
                Instr::Unknown(w0)
            }
        }
        // 11111 0111 xxx: 64-bit multiply (UMULL/SMULL).
        0b11000..=0b11111 => {
            let rdlo = ((w1 >> 12) & 0xF) as u8;
            let rdhi = ((w1 >> 8) & 0xF) as u8;
            match (w0 >> 4) & 0x7 {
                0b000 if w1 & 0xF0 == 0 => Instr::Smull { rdlo, rdhi, rn, rm },
                0b010 if w1 & 0xF0 == 0 => Instr::Umull { rdlo, rdhi, rn, rm },
                _ => Instr::Unknown(w0),
            }
        }
        _ => Instr::Unknown(w0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_movs_immediate() {
        // MOVS r0, #0x2A = 0x202A.
        assert_eq!(
            decode(0x202A, 0).0,
            Instr::MovImm {
                rd: 0,
                imm: 0x2A,
                setflags: true
            }
        );
    }

    #[test]
    fn decodes_add_register() {
        // ADDS r0, r1, r2 = 0x1888.
        assert_eq!(
            decode(0x1888, 0).0,
            Instr::AddReg {
                rd: 0,
                rn: 1,
                rm: 2,
                setflags: true
            }
        );
    }

    #[test]
    fn decodes_high_register_add_without_flags() {
        // ADD r8, r1 (DN=1, Rd=0, Rm=1) = 0x4488.
        assert_eq!(
            decode(0x4488, 0).0,
            Instr::AddReg {
                rd: 8,
                rn: 8,
                rm: 1,
                setflags: false
            }
        );
    }

    #[test]
    fn decodes_bx_lr() {
        // BX LR = 0x4770.
        assert_eq!(decode(0x4770, 0).0, Instr::Bx { rm: 14 });
    }

    #[test]
    fn decodes_bl_as_four_bytes() {
        // BL +0: 0xF000 0xF800.
        let (instr, len) = decode(0xF000, 0xF800);
        assert_eq!(len, 4);
        assert_eq!(instr, Instr::Bl { offset: 0 });
    }

    #[test]
    fn decodes_push_and_pop() {
        // PUSH {r4, lr} = 0xB510.
        assert_eq!(
            decode(0xB510, 0).0,
            Instr::Push {
                regs: 0x10,
                lr: true
            }
        );
        // POP {r4, pc} = 0xBD10.
        assert_eq!(
            decode(0xBD10, 0).0,
            Instr::Pop {
                regs: 0x10,
                pc: true
            }
        );
    }

    #[test]
    fn decodes_ldr_literal() {
        // LDR r0, [pc, #0] = 0x4800.
        assert_eq!(
            decode(0x4800, 0).0,
            Instr::Mem(Mem {
                load: true,
                size: Size::Word,
                signed: false,
                rt: 0,
                rn: 15,
                addr: AddrMode::Imm(0),
            })
        );
    }

    #[test]
    fn unknown_for_unmodelled_encoding() {
        // A VFP/coprocessor encoding (w0[15:11]=11101, w0[10:9]=10) is not
        // modelled and still decodes to Unknown.
        assert_eq!(decode(0xEC00, 0x0A10).0, Instr::Unknown(0xEC00));
    }

    #[test]
    fn decodes_movw_movt() {
        // MOVW r0, #0x1234 = 0xF241 0x2034 (imm4=1,i=0,imm3=2,imm8=0x34).
        assert_eq!(
            decode(0xF241, 0x2034),
            (Instr::Movw { rd: 0, imm: 0x1234 }, 4)
        );
        // MOVT r0, #0xABCD = 0xF6CA 0x30CD.
        assert_eq!(
            decode(0xF6CA, 0x30CD),
            (Instr::Movt { rd: 0, imm: 0xABCD }, 4)
        );
    }

    #[test]
    fn decodes_mul_wide() {
        // MUL r0, r1, r2 = 0xFB01 0xF002 (Ra=1111).
        assert_eq!(
            decode(0xFB01, 0xF002),
            (
                Instr::Mul32 {
                    rd: 0,
                    rn: 1,
                    rm: 2
                },
                4
            )
        );
    }

    #[test]
    fn decodes_ubfx() {
        // UBFX r0, r1, #4, #8 = 0xF3C1 0x1007 (lsb=4 → imm3:imm2=00100, widthm1=7).
        assert_eq!(
            decode(0xF3C1, 0x1007),
            (
                Instr::Ubfx {
                    rd: 0,
                    rn: 1,
                    lsb: 4,
                    width: 8
                },
                4
            )
        );
    }

    #[test]
    fn decodes_it_block() {
        // ITE EQ = 0xBF0C (firstcond=0b0000, mask=0b1100).
        assert_eq!(
            decode(0xBF0C, 0).0,
            Instr::It {
                firstcond: 0b0000,
                mask: 0b1100,
            }
        );
        // A bare NOP in the same encoding group (mask == 0) stays a NOP.
        assert_eq!(decode(0xBF00, 0).0, Instr::Nop);
    }
}
