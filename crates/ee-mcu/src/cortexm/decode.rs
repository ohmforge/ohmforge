//! A table-driven decoder from Thumb halfwords to [`Instr`].
//!
//! ARMv6-M instructions are almost all a single 16-bit halfword; the one 32-bit
//! form this core decodes is `BL`. [`decode`] takes the opcode halfword `w0`
//! and the following halfword `w1` (used only by `BL`) and returns the decoded
//! [`Instr`] together with how many **bytes** it occupies (`2` or `4`).
//!
//! The decode is structured by the top bits of `w0`, mirroring the ARMv6-M ARM's
//! Thumb encoding map:
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
//! - `11110x` — `BL` (32-bit).
//!
//! # Coverage
//!
//! Implemented: the full data-processing register group (`AND EOR LSL LSR ASR
//! ADC SBC ROR TST RSB CMP CMN ORR MUL BIC MVN`); `MOV`/`ADD`/`SUB`/`CMP` in
//! their immediate, register and high-register forms; the shift-by-immediate
//! moves; `LDR`/`STR` (+ halfword/byte, signed loads) in literal, immediate,
//! register and SP-relative forms; `ADR`; `ADD`/`SUB SP`; `PUSH`/`POP`;
//! `STMIA`/`LDMIA`; `SXT*`/`UXT*`; `REV*`; `B`/`B<cond>`/`BL`/`BX`/`BLX`;
//! `SVC`; and `NOP`/hints.
//!
//! Not yet implemented (decode to [`Instr::Unknown`], a documented TODO): the
//! system register access `MRS`/`MSR`, the barriers `DSB`/`DMB`/`ISB`, `CPS`,
//! `BKPT`, `UDF`, and `WFI`/`WFE`/`SEV`/`YIELD` beyond treating the hint space
//! as `NOP`.

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
        // 11100x: unconditional branch.
        0b111000 | 0b111001 => (
            Instr::B {
                offset: imm11_offset(w0),
            },
            2,
        ),
        // 11110x: BL (32-bit) and other 32-bit encodings.
        0b111100 | 0b111101 => (decode_bl(w0, w1), 4),
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
        // 1111xxxx: hints (NOP/WFI/…). Treated as NOP; bits 7:4 pick the hint.
        0b111100..=0b111111 => Instr::Nop,
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

// --- 11110x: BL (32-bit) ---

fn decode_bl(w0: u16, w1: u16) -> Instr {
    // Only the BL form (w1's bits 15:14 = 11) is modelled; other 32-bit
    // encodings (MRS/MSR/DSB/…) decode to Unknown.
    if (w1 >> 14) & 0x3 != 0b11 {
        return Instr::Unknown(w0);
    }
    let s = ((w0 >> 10) & 0x1) as u32;
    let imm10 = (w0 & 0x3FF) as u32;
    let j1 = ((w1 >> 13) & 0x1) as u32;
    let j2 = ((w1 >> 11) & 0x1) as u32;
    let imm11 = (w1 & 0x7FF) as u32;
    // I1 = NOT(J1 EOR S), I2 = NOT(J2 EOR S).
    let i1 = 1 - (j1 ^ s);
    let i2 = 1 - (j2 ^ s);
    // offset[24:0] = S:I1:I2:imm10:imm11:0, sign-extended from bit 24.
    let imm = (s << 24) | (i1 << 23) | (i2 << 22) | (imm10 << 12) | (imm11 << 1);
    let offset = ((imm << 7) as i32) >> 7;
    Instr::Bl { offset }
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
        // An MRS (32-bit, w1 top bits != 11) is not modelled.
        assert_eq!(decode(0xF3EF, 0x8000).0, Instr::Unknown(0xF3EF));
    }
}
