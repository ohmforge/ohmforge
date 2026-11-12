//! A table-driven decoder from Xtensa instruction bytes to [`Instr`].
//!
//! Xtensa instructions are **variable length**. [`decode`] takes the three bytes
//! at the program counter (`b0` lowest-addressed) and returns the decoded
//! [`Instr`] together with its length in bytes (`2` for a Code-Density narrow
//! form, `3` for a core instruction). The length is a function of the major
//! opcode `op0` (the low nibble of `b0`): `op0` in `0x8..=0xD` selects a two-byte
//! narrow instruction, everything else a three-byte core instruction.
//!
//! # Coverage
//!
//! Implemented core (3-byte) instructions:
//! - **RRR ALU** (`op0 = 0x0`, `op1 = 0`): `ADD SUB AND OR XOR` (and `MOV` as
//!   `OR ar, as, as`); the `ST0` control forms `RET`, `JX`, `CALLX0`.
//! - **RRI8 / LSAI** (`op0 = 0x2`): `MOVI ADDI`, and the load/store family
//!   `L8UI L16UI L16SI L32I S8I S16I S32I`.
//! - **L32R** (`op0 = 0x1`): PC-relative literal load.
//! - **CALL** (`op0 = 0x5`, `n = 0`): `CALL0`.
//! - **SI** (`op0 = 0x6`): `J` (`n = 0`) and the `BZ` family `BEQZ BNEZ BLTZ
//!   BGEZ` (`n = 1`).
//!
//! Implemented narrow (2-byte, Code-Density option) instructions:
//! - **ST3** (`op0 = 0xD`): `MOV.N`, `RET.N`, `NOP.N`.
//!
//! Not implemented — these decode to [`Instr::Unknown`], a documented TODO:
//! - the **windowed-call ABI**: `CALL4/8/12`, `CALLX4/8/12`, `ENTRY`, `RETW`,
//!   `RETW.N`, `MOVSP`, `ROTW`, and the 64-entry physical register file with its
//!   `WindowBase`/`WindowStart` rotation (this core models the flat CALL0 ABI);
//! - the register-register compare-and-branch families (`BEQ`/`BNE`/`BLT`/`BALL`
//!   /`BBC`/`BBS`…, `op0 = 0x7`) and the immediate-compare branches (`BEQI`/`BNEI`
//!   /…, `op0 = 0x6`, `n = 2`/`3`) plus `LOOP`;
//! - shifts and extended arithmetic (`SLL`/`SRL`/`SRA`/`SSL`/`EXTUI`/`ADDX2/4/8`
//!   /`SUBX2/4/8`/`NEG`/`ABS`), the `MUL`/`MAC16` option, and the processor-state
//!   ops (`RSR`/`WSR`/`RUR`/`WUR`, `MEMW`/`EXTW`, `ISYNC`/`RSYNC`, `WAITI`, `BREAK`);
//! - the other Code-Density narrow forms (`L32I.N`/`S32I.N`/`ADD.N`/`ADDI.N`,
//!   `MOVI.N`/`BEQZ.N`/`BNEZ.N`);
//! - the floating-point (`LSCI`/`FP0`) encodings.

use super::isa::{Instr, Size};

/// Decode one instruction from the three bytes at the program counter (`b0`
/// lowest-addressed; `b2` is unused by the two-byte narrow forms). Returns the
/// instruction and its length in bytes.
pub fn decode(b0: u8, b1: u8, b2: u8) -> (Instr, u8) {
    let op0 = b0 & 0x0F;
    if (0x8..=0xD).contains(&op0) {
        // Code-Density narrow (2-byte) instruction.
        let w = b0 as u32 | (b1 as u32) << 8;
        return (decode_narrow(op0, w), 2);
    }
    let w = b0 as u32 | (b1 as u32) << 8 | (b2 as u32) << 16;
    (decode_core(op0, w), 3)
}

/// Decode a 3-byte core instruction, dispatched on the major opcode `op0`.
fn decode_core(op0: u8, w: u32) -> Instr {
    match op0 {
        0x0 => decode_qrst(w),
        0x1 => Instr::L32r {
            t: field_t(w),
            imm16: ((w >> 8) & 0xFFFF) as u16,
        },
        0x2 => decode_lsai(w),
        0x5 => decode_call(w),
        0x6 => decode_si(w),
        _ => Instr::Unknown(w),
    }
}

/// The `QRST` group (`op0 = 0x0`): the RRR ALU and control ops. Only the `RST0`
/// sub-group (`op1 = 0`) is modelled.
fn decode_qrst(w: u32) -> Instr {
    if field_op1(w) != 0 {
        return Instr::Unknown(w); // RST1/RST2/RST3 (shifts, MUL, RSR…) not modelled
    }
    let (r, s, t) = (field_r(w), field_s(w), field_t(w));
    match field_op2(w) {
        0x0 => decode_st0(w, s, t),
        0x1 => Instr::And { r, s, t },
        0x2 => Instr::Or { r, s, t },
        0x3 => Instr::Xor { r, s, t },
        0x8 => Instr::Add { r, s, t },
        0xC => Instr::Sub { r, s, t },
        _ => Instr::Unknown(w), // ADDX2/4/8, SUBX2/4/8, NEG/ABS, TLB…
    }
}

/// The `ST0` control sub-group (`op0 = 0`, `op1 = 0`, `op2 = 0`). The `t` field
/// packs `m = t[3:2]` (operation class) and `n = t[1:0]` (variant).
fn decode_st0(w: u32, s: u8, t: u8) -> Instr {
    let (m, n) = (t >> 2, t & 0x3);
    match m {
        // SNM0: JR class (RET / RETW / JX) and CALLX class.
        0x2 => match n {
            0 => Instr::Ret,      // RET
            2 => Instr::Jx { s }, // JX as
            _ => Instr::Unknown(w),
        },
        0x3 => match n {
            0 => Instr::Callx0 { s }, // CALLX0 as
            _ => Instr::Unknown(w),   // CALLX4/8/12 (windowed)
        },
        _ => Instr::Unknown(w), // MOVSP, SYNC, RFEI…
    }
}

/// The `LSAI` group (`op0 = 0x2`): loads/stores plus `MOVI`/`ADDI`, dispatched on
/// the `r` field.
fn decode_lsai(w: u32) -> Instr {
    let (s, t) = (field_s(w), field_t(w));
    let imm8 = (w >> 16) & 0xFF;
    let load = |size, signed| Instr::Load {
        t,
        s,
        off: imm8 * size_scale(size),
        size,
        signed,
    };
    let store = |size| Instr::Store {
        t,
        s,
        off: imm8 * size_scale(size),
        size,
    };
    match field_r(w) {
        0x0 => load(Size::Byte, false), // L8UI
        0x1 => load(Size::Half, false), // L16UI
        0x2 => load(Size::Word, false), // L32I
        0x4 => store(Size::Byte),       // S8I
        0x5 => store(Size::Half),       // S16I
        0x6 => store(Size::Word),       // S32I
        0x9 => load(Size::Half, true),  // L16SI
        0xA => Instr::Movi {
            t,
            imm: sext(((w >> 8) & 0xF) << 8 | imm8, 12),
        },
        0xC => Instr::Addi {
            t,
            s,
            imm: sext(imm8, 8),
        },
        _ => Instr::Unknown(w), // ADDMI, L32AI, S32C1I, S32RI, CACHE…
    }
}

/// The `CALL` group (`op0 = 0x5`): only the windowless `CALL0` (`n = 0`) is
/// modelled; `CALL4`/`CALL8`/`CALL12` are windowed.
fn decode_call(w: u32) -> Instr {
    if field_n(w) == 0 {
        // The 18-bit offset is word-scaled (<<2) off the aligned PC.
        Instr::Call0 {
            offset: sext((w >> 6) & 0x3FFFF, 18) << 2,
        }
    } else {
        Instr::Unknown(w)
    }
}

/// The `SI` group (`op0 = 0x6`): `J` (`n = 0`) and the `BZ` compare-with-zero
/// branches (`n = 1`). The immediate-compare branches (`n = 2`/`3`) are not
/// modelled.
fn decode_si(w: u32) -> Instr {
    match field_n(w) {
        0 => Instr::J {
            offset: sext((w >> 6) & 0x3FFFF, 18),
        },
        1 => {
            let s = field_s(w);
            let offset = sext((w >> 12) & 0xFFF, 12);
            // m = w[7:6] selects the comparison.
            match (w >> 6) & 0x3 {
                0 => Instr::Beqz { s, offset },
                1 => Instr::Bnez { s, offset },
                2 => Instr::Bltz { s, offset },
                _ => Instr::Bgez { s, offset },
            }
        }
        _ => Instr::Unknown(w), // BI0 (BEQI/BNEI/…), BI1 (ENTRY/LOOP/…)
    }
}

/// Decode a 2-byte narrow (Code-Density) instruction. Only the `ST3` group
/// (`op0 = 0xD`) is modelled.
fn decode_narrow(op0: u8, w: u32) -> Instr {
    if op0 != 0xD {
        return Instr::Unknown(w); // L32I.N/S32I.N/ADD.N/ADDI.N/MOVI.N/BEQZ.N…
    }
    let (r, s, t) = (field_r(w), field_s(w), field_t(w));
    match r {
        0x0 => Instr::MovN { t, s }, // MOV.N at, as
        0xF => match t {
            0 => Instr::Ret, // RET.N
            3 => Instr::Nop, // NOP.N
            _ => Instr::Unknown(w),
        },
        _ => Instr::Unknown(w),
    }
}

// --- Bit-field extraction helpers (shared field positions) ---

/// The `op0` major opcode is the caller's responsibility; these read the common
/// RRR/RRI8 register and sub-opcode fields.
fn field_t(w: u32) -> u8 {
    ((w >> 4) & 0xF) as u8
}
fn field_s(w: u32) -> u8 {
    ((w >> 8) & 0xF) as u8
}
fn field_r(w: u32) -> u8 {
    ((w >> 12) & 0xF) as u8
}
fn field_op1(w: u32) -> u8 {
    ((w >> 16) & 0xF) as u8
}
fn field_op2(w: u32) -> u8 {
    ((w >> 20) & 0xF) as u8
}
/// The `n` sub-format field (`w[5:4]`) of the CALL / SI formats.
fn field_n(w: u32) -> u8 {
    ((w >> 4) & 0x3) as u8
}

/// The byte scale of an access width (for RRI8 offsets: byte ×1, half ×2, word ×4).
fn size_scale(size: Size) -> u32 {
    match size {
        Size::Byte => 1,
        Size::Half => 2,
        Size::Word => 4,
    }
}

/// Sign-extend the low `bits` of `val` to a 32-bit signed integer.
fn sext(val: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((val << shift) as i32) >> shift
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_rrr_alu_and_mov() {
        // ADD a3, a4, a5 = r=3,s=4,t=5,op2=8 → 0x803450 (bytes 50 34 80).
        assert_eq!(decode(0x50, 0x34, 0x80).0, Instr::Add { r: 3, s: 4, t: 5 });
        // SUB a3, a4, a5 (op2=0xC) → 0xC03450.
        assert_eq!(decode(0x50, 0x34, 0xC0).0, Instr::Sub { r: 3, s: 4, t: 5 });
        // AND / OR / XOR (op2 = 1 / 2 / 3).
        assert_eq!(decode(0x50, 0x34, 0x10).0, Instr::And { r: 3, s: 4, t: 5 });
        assert_eq!(decode(0x50, 0x34, 0x20).0, Instr::Or { r: 3, s: 4, t: 5 });
        assert_eq!(decode(0x50, 0x34, 0x30).0, Instr::Xor { r: 3, s: 4, t: 5 });
    }

    #[test]
    fn decodes_control_flow_rrr() {
        // RET = 0x000080 (t=8 → m=2,n=0).
        assert_eq!(decode(0x80, 0x00, 0x00), (Instr::Ret, 3));
        // JX a5 = 0x0005A0 (t=0xA → m=2,n=2 ; s=5).
        assert_eq!(decode(0xA0, 0x05, 0x00).0, Instr::Jx { s: 5 });
        // CALLX0 a3 = 0x0003C0 (t=0xC → m=3,n=0 ; s=3).
        assert_eq!(decode(0xC0, 0x03, 0x00).0, Instr::Callx0 { s: 3 });
    }

    #[test]
    fn decodes_movi_and_addi() {
        // MOVI a2, 4 = 0x04A022 (bytes 22 A0 04).
        assert_eq!(decode(0x22, 0xA0, 0x04).0, Instr::Movi { t: 2, imm: 4 });
        // MOVI a2, -1 → 12-bit 0xFFF: s=0xF, imm8=0xFF → 0xFFA02F? compute:
        // imm12=0xFFF → s=0xF, imm8=0xFF ; op0=2,t=2,r=0xA → bytes 22 AF FF.
        assert_eq!(decode(0x22, 0xAF, 0xFF).0, Instr::Movi { t: 2, imm: -1 });
        // ADDI a3, a4, 5 = op0=2,t=3,s=4,r=0xC,imm8=5 → bytes 32 C4 05.
        assert_eq!(
            decode(0x32, 0xC4, 0x05).0,
            Instr::Addi { t: 3, s: 4, imm: 5 }
        );
        // ADDI a3, a4, -1 (imm8 = 0xFF) → bytes 32 C4 FF.
        assert_eq!(
            decode(0x32, 0xC4, 0xFF).0,
            Instr::Addi {
                t: 3,
                s: 4,
                imm: -1
            }
        );
    }

    #[test]
    fn decodes_loads_and_stores() {
        // S32I a3, a2, 0x08 → imm8 = 8/4 = 2 ; op0=2,t=3,s=2,r=6 → bytes 32 62 02.
        assert_eq!(
            decode(0x32, 0x62, 0x02).0,
            Instr::Store {
                t: 3,
                s: 2,
                off: 0x08,
                size: Size::Word
            }
        );
        // L32I a4, a2, 0x04 → imm8 = 1 ; r=2 → bytes 42 22 01.
        assert_eq!(
            decode(0x42, 0x22, 0x01).0,
            Instr::Load {
                t: 4,
                s: 2,
                off: 0x04,
                size: Size::Word,
                signed: false
            }
        );
        // L8UI a4, a2, 0x07 → r=0, offset unscaled = 7 → bytes 42 02 07.
        assert_eq!(
            decode(0x42, 0x02, 0x07).0,
            Instr::Load {
                t: 4,
                s: 2,
                off: 0x07,
                size: Size::Byte,
                signed: false
            }
        );
        // S8I a3, a2, 0x07 → r=4 → bytes 32 42 07.
        assert_eq!(
            decode(0x32, 0x42, 0x07).0,
            Instr::Store {
                t: 3,
                s: 2,
                off: 0x07,
                size: Size::Byte
            }
        );
        // L16SI a4, a2, 0x04 → r=9, offset = 2*2 = 4, signed → bytes 42 92 02.
        assert_eq!(
            decode(0x42, 0x92, 0x02).0,
            Instr::Load {
                t: 4,
                s: 2,
                off: 0x04,
                size: Size::Half,
                signed: true
            }
        );
    }

    #[test]
    fn decodes_l32r_j_and_branches() {
        // L32R a2, -4 → imm16 = 0xFFFF ; op0=1,t=2 → bytes 21 FF FF.
        assert_eq!(
            decode(0x21, 0xFF, 0xFF).0,
            Instr::L32r {
                t: 2,
                imm16: 0xFFFF
            }
        );
        // J back 10 bytes: offset = -10, imm18 = 0x3FFF6 → 0xFFFD86 (bytes 86 FD FF).
        assert_eq!(decode(0x86, 0xFD, 0xFF).0, Instr::J { offset: -10 });
        // BEQZ a3, +8 : op0=6,n=1,m=0,s=3,imm12=8.
        // w = 6 | (1<<4) | (0<<6) | (3<<8) | (8<<12) = 0x08316 → bytes 16 83 00.
        assert_eq!(decode(0x16, 0x83, 0x00).0, Instr::Beqz { s: 3, offset: 8 });
        // BNEZ a3, +8 : m=1 → w = 6|0x10|(1<<6)|0x300|0x8000 = 0x08356 → 56 83 00.
        assert_eq!(decode(0x56, 0x83, 0x00).0, Instr::Bnez { s: 3, offset: 8 });
    }

    #[test]
    fn decodes_narrow_forms() {
        // MOV.N a3, a4 : op0=0xD, r=0, s=4, t=3 → w16 = 0xD|0x30|0x400 = 0x043D.
        assert_eq!(decode(0x3D, 0x04, 0x00), (Instr::MovN { t: 3, s: 4 }, 2));
        // NOP.N = 0xF03D (bytes 3D F0), two bytes.
        assert_eq!(decode(0x3D, 0xF0, 0x00), (Instr::Nop, 2));
        // RET.N = 0xF00D (bytes 0D F0), two bytes.
        assert_eq!(decode(0x0D, 0xF0, 0x00), (Instr::Ret, 2));
    }

    #[test]
    fn unimplemented_encodings_decode_to_unknown() {
        // CALL4 (op0=5, n=1) — a windowed call — is not modelled.
        assert!(matches!(decode(0x15, 0x00, 0x00).0, Instr::Unknown(_)));
        // A register-register branch (op0=7, BEQ) is not modelled.
        assert!(matches!(decode(0x07, 0x00, 0x00).0, Instr::Unknown(_)));
        // An immediate-compare branch (op0=6, n=2 → BEQI) is not modelled.
        assert!(matches!(decode(0x26, 0x00, 0x00).0, Instr::Unknown(_)));
        // A shift (RST1, op1=1) is not modelled.
        assert!(matches!(decode(0x00, 0x10, 0x00).0, Instr::Unknown(_)));
        // A narrow L32I.N (op0=8) is not executed.
        assert!(matches!(decode(0x08, 0x00, 0x00).0, Instr::Unknown(_)));
        // A reserved major opcode (op0=0xE).
        assert!(matches!(decode(0x0E, 0x00, 0x00).0, Instr::Unknown(_)));
    }

    #[test]
    fn length_tracks_the_major_opcode() {
        // Core instructions are three bytes; narrow (op0 0x8..=0xD) are two.
        assert_eq!(decode(0x50, 0x34, 0x80).1, 3); // ADD
        assert_eq!(decode(0x3D, 0xF0, 0x00).1, 2); // NOP.N
        assert_eq!(decode(0x08, 0x00, 0x00).1, 2); // narrow (Unknown)
    }
}
