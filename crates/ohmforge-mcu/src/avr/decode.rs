//! A table-driven decoder from flash words to [`Instr`].
//!
//! AVR instructions are one or two 16-bit words. [`decode`] takes the opcode word
//! `w0` and the following word `w1` (needed only by the 32-bit forms: `LDS`,
//! `STS`, `CALL`, `JMP`) and returns the decoded [`Instr`] together with how many
//! words it occupies (`1` or `2`).
//!
//! # Coverage
//!
//! Implemented: `NOP`; the register ALU ops `ADD ADC SUB SBC AND OR EOR CP CPC
//! CPSE MOV MOVW`; the immediate ops `SUBI SBCI ANDI ORI CPI LDI`; the word-
//! immediate ops `ADIW SBIW`; the unary ops
//! `COM NEG INC DEC SWAP LSR ASR ROR`; data transfer `LDS STS LD/ST (X/Y/Z with
//! post-increment / pre-decrement) LDD/STD (Y/Z + q) IN OUT PUSH POP`; bit ops
//! `SBI CBI SBIC SBIS SBRC SBRS BSET BCLR BLD BST`; and control flow `RJMP RCALL
//! IJMP ICALL JMP CALL RET RETI` plus the full `BRBS`/`BRBC` conditional-branch
//! family.
//!
//! Not yet implemented (decode to [`Instr::Unknown`], a documented TODO): the
//! multiply family (`MUL`/`MULS`/`FMUL`…), program-memory access (`LPM`/`SPM`),
//! `SLEEP`/`WDR`/`BREAK`, and `EIJMP`/`EICALL`.

use super::isa::{Instr, Ptr, PtrMode};

/// Decode one instruction. Returns the instruction and its length in words.
pub fn decode(w0: u16, w1: u16) -> (Instr, u8) {
    match w0 >> 12 {
        // Nibbles 0x0–0x2 hold the two-register ops, distinguished by bits 15:10.
        0x0..=0x2 => decode_two_reg(w0),

        // Register/immediate ops: the destination is r16..r31.
        0x3 => (Instr::Cpi(rd_imm(w0), k8(w0)), 1),
        0x4 => (Instr::Sbci(rd_imm(w0), k8(w0)), 1),
        0x5 => (Instr::Subi(rd_imm(w0), k8(w0)), 1),
        0x6 => (Instr::Ori(rd_imm(w0), k8(w0)), 1),
        0x7 => (Instr::Andi(rd_imm(w0), k8(w0)), 1),
        0xE => (Instr::Ldi(rd_imm(w0), k8(w0)), 1),

        // LDD/STD with displacement (and the q=0 "LD Rd, Y/Z" forms).
        0x8 | 0xA => decode_ldd_std(w0),

        // The dense 0x9 group: LD/ST, unary ALU, CALL/JMP, RET/RETI, bit I/O…
        0x9 => decode_9(w0, w1),

        // IN / OUT.
        0xB => decode_in_out(w0),

        // Relative jump / call (12-bit signed word offset).
        0xC => (Instr::Rjmp(sext(w0 & 0x0FFF, 12) as i16), 1),
        0xD => (Instr::Rcall(sext(w0 & 0x0FFF, 12) as i16), 1),

        // Conditional branches and single-bit skips/copies.
        0xF => decode_f(w0),

        _ => (Instr::Unknown(w0), 1),
    }
}

/// Two-register ALU ops in nibbles 0x0–0x2, keyed on bits 15:10.
fn decode_two_reg(w0: u16) -> (Instr, u8) {
    let (d, r) = (rd5(w0), rr5(w0));
    let instr = match w0 >> 10 {
        0x00 => {
            // 0000 00xx: NOP, MOVW, or an unimplemented multiply form.
            if w0 == 0 {
                Instr::Nop
            } else if w0 & 0xFF00 == 0x0100 {
                // MOVW: register-pair operands are the 4-bit fields doubled.
                Instr::Movw((w0 >> 4 & 0xF) as u8 * 2, (w0 & 0xF) as u8 * 2)
            } else {
                Instr::Unknown(w0)
            }
        }
        0x01 => Instr::Cpc(d, r),
        0x02 => Instr::Sbc(d, r),
        0x03 => Instr::Add(d, r),
        0x04 => Instr::Cpse(d, r),
        0x05 => Instr::Cp(d, r),
        0x06 => Instr::Sub(d, r),
        0x07 => Instr::Adc(d, r),
        0x08 => Instr::And(d, r),
        0x09 => Instr::Eor(d, r),
        0x0A => Instr::Or(d, r),
        0x0B => Instr::Mov(d, r),
        _ => Instr::Unknown(w0),
    };
    (instr, 1)
}

/// `LDD`/`STD` (and the displacement-zero `LD/ST Rd, Y/Z`) in nibbles 0x8/0xA.
/// Encoding: `10q0 qqsd dddd yqqq` — `s` selects store, `y` selects `Y` over `Z`.
fn decode_ldd_std(w0: u16) -> (Instr, u8) {
    let d = rd5(w0);
    let ptr = if w0 & 0x0008 != 0 { Ptr::Y } else { Ptr::Z };
    let q = (((w0 >> 13) & 1) << 5 | ((w0 >> 10) & 3) << 3 | (w0 & 7)) as u8;
    let store = w0 & 0x0200 != 0;
    let instr = if store {
        Instr::Std(ptr, q, d)
    } else {
        Instr::Ldd(d, ptr, q)
    };
    (instr, 1)
}

/// The 0x9 opcode group, sub-dispatched on bits 15:10.
fn decode_9(w0: u16, w1: u16) -> (Instr, u8) {
    match w0 >> 10 {
        0x24 => decode_ld_st(w0, w1),  // 1001 00x.: load/store data transfer
        0x25 => decode_9_misc(w0, w1), // 1001 01x.: unary ALU, CALL/JMP, RET…
        0x26 | 0x27 => decode_sbi(w0), // 1001 10x.: SBI/CBI/SBIC/SBIS
        _ => (Instr::Unknown(w0), 1),  // 1001 11..: MUL family (unimplemented)
    }
}

/// `LD`/`ST` family: `1001 00sd dddd nnnn`, `s` = store, `nnnn` = pointer+mode.
fn decode_ld_st(w0: u16, w1: u16) -> (Instr, u8) {
    let d = rd5(w0);
    let store = w0 & 0x0200 != 0;
    match w0 & 0x000F {
        // LDS/STS are 32-bit: the data address is the whole next word.
        0x0 => {
            let instr = if store {
                Instr::Sts(w1, d)
            } else {
                Instr::Lds(d, w1)
            };
            (instr, 2)
        }
        0x1 => (ld_or_st(store, d, Ptr::Z, PtrMode::PostInc), 1),
        0x2 => (ld_or_st(store, d, Ptr::Z, PtrMode::PreDec), 1),
        0x9 => (ld_or_st(store, d, Ptr::Y, PtrMode::PostInc), 1),
        0xA => (ld_or_st(store, d, Ptr::Y, PtrMode::PreDec), 1),
        0xC => (ld_or_st(store, d, Ptr::X, PtrMode::None), 1),
        0xD => (ld_or_st(store, d, Ptr::X, PtrMode::PostInc), 1),
        0xE => (ld_or_st(store, d, Ptr::X, PtrMode::PreDec), 1),
        0xF => {
            let instr = if store { Instr::Push(d) } else { Instr::Pop(d) };
            (instr, 1)
        }
        _ => (Instr::Unknown(w0), 1), // LPM/ELPM forms
    }
}

fn ld_or_st(store: bool, d: u8, ptr: Ptr, mode: PtrMode) -> Instr {
    if store {
        Instr::St(ptr, mode, d)
    } else {
        Instr::Ld(d, ptr, mode)
    }
}

/// `1001 01..`: unary ALU ops, `CALL`/`JMP`, `RET`/`RETI`, `BSET`/`BCLR`,
/// `IJMP`/`ICALL`.
fn decode_9_misc(w0: u16, w1: u16) -> (Instr, u8) {
    // Exact-match singletons first.
    match w0 {
        0x9508 => return (Instr::Ret, 1),
        0x9518 => return (Instr::Reti, 1),
        0x9409 => return (Instr::Ijmp, 1),
        0x9509 => return (Instr::Icall, 1),
        _ => {}
    }
    // ADIW = 1001 0110 KKdd KKKK ; SBIW = 1001 0111 KKdd KKKK. The register pair
    // is one of r24/r26/r28/r30 and K is a 6-bit immediate.
    if w0 & 0xFF00 == 0x9600 {
        return (Instr::Adiw(adiw_reg(w0), adiw_k(w0)), 1);
    }
    if w0 & 0xFF00 == 0x9700 {
        return (Instr::Sbiw(adiw_reg(w0), adiw_k(w0)), 1);
    }
    // BSET s = 1001 0100 0sss 1000 ; BCLR s = 1001 0100 1sss 1000.
    if w0 & 0xFF8F == 0x9408 {
        return (Instr::Bset((w0 >> 4 & 7) as u8), 1);
    }
    if w0 & 0xFF8F == 0x9488 {
        return (Instr::Bclr((w0 >> 4 & 7) as u8), 1);
    }
    // JMP = 1001 010k kkkk 110k ; CALL = 1001 010k kkkk 111k (+ 16-bit address).
    if w0 & 0xFE0E == 0x940C {
        return (Instr::Jmp(jmp_addr(w0, w1)), 2);
    }
    if w0 & 0xFE0E == 0x940E {
        return (Instr::Call(jmp_addr(w0, w1)), 2);
    }
    // Otherwise a unary ALU op, keyed on the low nibble.
    let d = rd5(w0);
    let instr = match w0 & 0x000F {
        0x0 => Instr::Com(d),
        0x1 => Instr::Neg(d),
        0x2 => Instr::Swap(d),
        0x3 => Instr::Inc(d),
        0x5 => Instr::Asr(d),
        0x6 => Instr::Lsr(d),
        0x7 => Instr::Ror(d),
        0xA => Instr::Dec(d),
        _ => Instr::Unknown(w0),
    };
    (instr, 1)
}

/// `SBI`/`CBI`/`SBIC`/`SBIS`: `1001 10xx AAAA Abbb`.
fn decode_sbi(w0: u16) -> (Instr, u8) {
    let a = (w0 >> 3 & 0x1F) as u8;
    let b = (w0 & 7) as u8;
    let instr = match w0 >> 8 & 0xFF {
        0x98 => Instr::Cbi(a, b),
        0x99 => Instr::Sbic(a, b),
        0x9A => Instr::Sbi(a, b),
        0x9B => Instr::Sbis(a, b),
        _ => Instr::Unknown(w0),
    };
    (instr, 1)
}

/// `IN`/`OUT`: `1011 0AAd dddd AAAA` / `1011 1AAr rrrr AAAA`.
fn decode_in_out(w0: u16) -> (Instr, u8) {
    let reg = rd5(w0);
    let a = ((w0 >> 9 & 3) << 4 | (w0 & 0xF)) as u8;
    let instr = if w0 & 0x0800 == 0 {
        Instr::In(reg, a)
    } else {
        Instr::Out(a, reg)
    };
    (instr, 1)
}

/// Nibble 0xF: conditional branches (`BRBS`/`BRBC`) and the single-bit
/// skip/copy ops (`SBRC`/`SBRS`, `BLD`/`BST`).
fn decode_f(w0: u16) -> (Instr, u8) {
    let instr = match w0 >> 10 {
        0x3C => Instr::Brbs((w0 & 7) as u8, sext(w0 >> 3 & 0x7F, 7) as i8),
        0x3D => Instr::Brbc((w0 & 7) as u8, sext(w0 >> 3 & 0x7F, 7) as i8),
        0x3E => {
            // 1111 10xd dddd 0bbb: BLD (bit9=0) / BST (bit9=1).
            let (d, b) = (rd5(w0), (w0 & 7) as u8);
            if w0 & 0x0200 == 0 {
                Instr::Bld(d, b)
            } else {
                Instr::Bst(d, b)
            }
        }
        0x3F => {
            // 1111 11xr rrrr 0bbb: SBRC (bit9=0) / SBRS (bit9=1).
            let (r, b) = (rd5(w0), (w0 & 7) as u8);
            if w0 & 0x0200 == 0 {
                Instr::Sbrc(r, b)
            } else {
                Instr::Sbrs(r, b)
            }
        }
        _ => Instr::Unknown(w0),
    };
    (instr, 1)
}

// --- Bit-field extraction helpers ---

/// The 5-bit destination register in bits 8:4.
fn rd5(w: u16) -> u8 {
    (w >> 4 & 0x1F) as u8
}

/// The 5-bit source register: bit 9 is its high bit, bits 3:0 the low nibble.
fn rr5(w: u16) -> u8 {
    (((w >> 9 & 1) << 4) | (w & 0xF)) as u8
}

/// The r16..r31 destination of an immediate op (4-bit field + 16).
fn rd_imm(w: u16) -> u8 {
    16 + (w >> 4 & 0xF) as u8
}

/// The low register of an `ADIW`/`SBIW` pair: `dd` selects r24/r26/r28/r30.
fn adiw_reg(w: u16) -> u8 {
    24 + ((w >> 4) & 3) as u8 * 2
}

/// The 6-bit immediate of an `ADIW`/`SBIW`: bits 7:6 are the high two bits.
fn adiw_k(w: u16) -> u8 {
    (((w >> 6) & 3) << 4 | (w & 0xF)) as u8
}

/// The 8-bit immediate of a register/immediate op: high nibble in bits 11:8,
/// low nibble in bits 3:0.
fn k8(w: u16) -> u8 {
    ((w >> 4 & 0xF0) | (w & 0x0F)) as u8
}

/// The absolute word address of a `JMP`/`CALL`: the high bits live in `w0`, the
/// low 16 in `w1`.
fn jmp_addr(w0: u16, w1: u16) -> u32 {
    let hi = ((w0 >> 4 & 0x1F) << 1 | (w0 & 1)) as u32;
    hi << 16 | w1 as u32
}

/// Sign-extend the low `bits` of `val` to a 32-bit signed integer.
fn sext(val: u16, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((val as i32) << shift) >> shift
}

#[cfg(test)]
mod tests {
    use super::decode;
    use crate::avr::isa::{Instr, Ptr, PtrMode};

    #[test]
    fn decodes_representative_opcodes() {
        // ALU: ADD r1, r2 = 0x0C12 ; the two-reg field packs d=1, r=2.
        assert_eq!(decode(0x0C12, 0).0, Instr::Add(1, 2));
        // EOR r1, r1 (a common "clear register") = 0x2411.
        assert_eq!(decode(0x2411, 0).0, Instr::Eor(1, 1));
        // LDI r16, 0xFF = 0xEF0F.
        assert_eq!(decode(0xEF0F, 0).0, Instr::Ldi(16, 0xFF));
        // SUBI r16, 1 = 0x5001.
        assert_eq!(decode(0x5001, 0).0, Instr::Subi(16, 1));
        // MOV r0, r16 = 0x2E00 (bit 9 carries r's high bit).
        assert_eq!(decode(0x2E00, 0).0, Instr::Mov(0, 16));
        // NOP.
        assert_eq!(decode(0x0000, 0).0, Instr::Nop);
        // RJMP .-2 (branch to self) = 0xCFFF, offset -1.
        assert_eq!(decode(0xCFFF, 0).0, Instr::Rjmp(-1));
    }

    #[test]
    fn decodes_branches_and_flow() {
        // BRNE is BRBC with s=1 (Z). BRNE .-4 → offset -2.
        // 1111 01kk kkkk k001, k = -2 → 0xF7F1.
        assert_eq!(decode(0xF7F1, 0).0, Instr::Brbc(1, -2));
        // BREQ is BRBS, s=1.
        assert_eq!(decode(0xF3F1, 0).0, Instr::Brbs(1, -2));
        // SEI = 0x9478 = Bset(7) ; CLI = 0x94F8 = Bclr(7).
        assert_eq!(decode(0x9478, 0).0, Instr::Bset(7));
        assert_eq!(decode(0x94F8, 0).0, Instr::Bclr(7));
        // ADIW r24, 1 = 1001 0110 0000 0001 → 0x9601 (pair dd=00 → r24, K=1).
        assert_eq!(decode(0x9601, 0).0, Instr::Adiw(24, 1));
        // SBIW r24, 1 = 1001 0111 0000 0001 → 0x9701.
        assert_eq!(decode(0x9701, 0).0, Instr::Sbiw(24, 1));
        // ADIW r30, 63 = pair dd=11 → r30, K=63 (0x3F) → 0x96FF.
        assert_eq!(decode(0x96FF, 0).0, Instr::Adiw(30, 63));
        // RET / RETI.
        assert_eq!(decode(0x9508, 0).0, Instr::Ret);
        assert_eq!(decode(0x9518, 0).0, Instr::Reti);
        // CALL 0x1234 is a 2-word instruction.
        assert_eq!(decode(0x940E, 0x1234), (Instr::Call(0x1234), 2));
        // JMP 0x0100.
        assert_eq!(decode(0x940C, 0x0100), (Instr::Jmp(0x0100), 2));
    }

    #[test]
    fn decodes_memory_and_bit_ops() {
        // LD r16, X+ = 1001 000d dddd 1101 → 0x910D.
        assert_eq!(decode(0x910D, 0).0, Instr::Ld(16, Ptr::X, PtrMode::PostInc));
        // ST -Y, r17 = 1001 001d dddd 1010 → 0x931A.
        assert_eq!(decode(0x931A, 0).0, Instr::St(Ptr::Y, PtrMode::PreDec, 17));
        // LDS r20, 0x0200 (2-word).
        assert_eq!(decode(0x9140, 0x0200), (Instr::Lds(20, 0x0200), 2));
        // PUSH r0 = 0x920F ; POP r0 = 0x900F.
        assert_eq!(decode(0x920F, 0).0, Instr::Push(0));
        assert_eq!(decode(0x900F, 0).0, Instr::Pop(0));
        // SBI 0x05, 3 = 1001 1010 00101 011 → 0x9A2B.
        assert_eq!(decode(0x9A2B, 0).0, Instr::Sbi(5, 3));
        // IN r0, 0x3F (SREG) = 1011 0AAd dddd AAAA → 0xB60F.
        assert_eq!(decode(0xB60F, 0).0, Instr::In(0, 0x3F));
        // OUT 0x3F, r0 = 0xBE0F.
        assert_eq!(decode(0xBE0F, 0).0, Instr::Out(0x3F, 0));
    }
}
