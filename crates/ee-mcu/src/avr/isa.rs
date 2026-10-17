//! The decoded-instruction type and its operand shapes.
//!
//! [`Instr`] is what [`decode`](super::decode::decode) produces and
//! [`Avr`](super::core::Avr) executes. Register operands are plain register
//! numbers (`0..32`); immediates are the already-extracted constant; branch and
//! jump offsets are signed **word** offsets relative to the instruction that
//! follows (the AVR program counter counts words).

/// A pointer register pair used by the indirect load/store instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ptr {
    /// `X` = `r27:r26`.
    X,
    /// `Y` = `r29:r28`.
    Y,
    /// `Z` = `r31:r30`.
    Z,
}

impl Ptr {
    /// The low register of the pair (`X` → 26, `Y` → 28, `Z` → 30).
    pub fn lo(self) -> usize {
        match self {
            Ptr::X => 26,
            Ptr::Y => 28,
            Ptr::Z => 30,
        }
    }
}

/// How an indirect load/store updates its pointer register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtrMode {
    /// Use the pointer as-is.
    None,
    /// Use the pointer, then increment it.
    PostInc,
    /// Decrement the pointer, then use it.
    PreDec,
}

/// A decoded AVR instruction.
///
/// Operand conventions: `d` is the destination register, `r` the source
/// register, `k` an immediate constant, `a` an I/O-space address, `b` a bit
/// index, `s` a `SREG` bit index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instr {
    /// No operation.
    Nop,

    // --- Two-register ALU ---
    /// `Rd ← Rd + Rr`.
    Add(u8, u8),
    /// `Rd ← Rd + Rr + C`.
    Adc(u8, u8),
    /// `Rd ← Rd - Rr`.
    Sub(u8, u8),
    /// `Rd ← Rd - Rr - C`.
    Sbc(u8, u8),
    /// `Rd ← Rd & Rr`.
    And(u8, u8),
    /// `Rd ← Rd | Rr`.
    Or(u8, u8),
    /// `Rd ← Rd ^ Rr`.
    Eor(u8, u8),
    /// `Rd - Rr` (compare, flags only).
    Cp(u8, u8),
    /// `Rd - Rr - C` (compare with carry, flags only).
    Cpc(u8, u8),
    /// Skip the next instruction if `Rd == Rr`.
    Cpse(u8, u8),
    /// `Rd ← Rr`.
    Mov(u8, u8),
    /// `Rd+1:Rd ← Rr+1:Rr` (register-pair move; `d`, `r` are even).
    Movw(u8, u8),

    // --- Register / immediate ---
    /// `Rd ← Rd - K`.
    Subi(u8, u8),
    /// `Rd ← Rd - K - C`.
    Sbci(u8, u8),
    /// `Rd ← Rd & K`.
    Andi(u8, u8),
    /// `Rd ← Rd | K`.
    Ori(u8, u8),
    /// `Rd - K` (compare with immediate, flags only).
    Cpi(u8, u8),
    /// `Rd ← K`.
    Ldi(u8, u8),

    // --- Single register ---
    /// `Rd ← $FF - Rd` (one's complement).
    Com(u8),
    /// `Rd ← $00 - Rd` (two's complement).
    Neg(u8),
    /// `Rd ← Rd + 1`.
    Inc(u8),
    /// `Rd ← Rd - 1`.
    Dec(u8),
    /// Swap the nibbles of `Rd`.
    Swap(u8),
    /// Logical shift right.
    Lsr(u8),
    /// Arithmetic shift right (keeps bit 7).
    Asr(u8),
    /// Rotate right through carry.
    Ror(u8),

    // --- Data transfer ---
    /// `Rd ← (k)` (load direct from data space).
    Lds(u8, u16),
    /// `(k) ← Rr` (store direct to data space).
    Sts(u16, u8),
    /// `Rd ← (ptr)` with the given pointer update mode.
    Ld(u8, Ptr, PtrMode),
    /// `(ptr) ← Rr` with the given pointer update mode.
    St(Ptr, PtrMode, u8),
    /// `Rd ← (ptr + q)` (load with displacement; `Y`/`Z` only).
    Ldd(u8, Ptr, u8),
    /// `(ptr + q) ← Rr` (store with displacement; `Y`/`Z` only).
    Std(Ptr, u8, u8),
    /// `Rd ← I/O[a]`.
    In(u8, u8),
    /// `I/O[a] ← Rr`.
    Out(u8, u8),
    /// Push `Rr` onto the stack.
    Push(u8),
    /// Pop into `Rd` from the stack.
    Pop(u8),

    // --- Bit manipulation ---
    /// Set bit `b` in I/O register `a`.
    Sbi(u8, u8),
    /// Clear bit `b` in I/O register `a`.
    Cbi(u8, u8),
    /// Skip the next instruction if bit `b` of I/O register `a` is clear.
    Sbic(u8, u8),
    /// Skip the next instruction if bit `b` of I/O register `a` is set.
    Sbis(u8, u8),
    /// Skip the next instruction if bit `b` of `Rr` is clear.
    Sbrc(u8, u8),
    /// Skip the next instruction if bit `b` of `Rr` is set.
    Sbrs(u8, u8),
    /// Set `SREG` bit `s` (`SEI` is `Bset(7)`).
    Bset(u8),
    /// Clear `SREG` bit `s` (`CLI` is `Bclr(7)`).
    Bclr(u8),
    /// `Rd` bit `b` ← `T`.
    Bld(u8, u8),
    /// `T` ← `Rd` bit `b`.
    Bst(u8, u8),

    // --- Control flow ---
    /// Relative jump by a signed word offset.
    Rjmp(i16),
    /// Relative call by a signed word offset.
    Rcall(i16),
    /// Indirect jump to `Z`.
    Ijmp,
    /// Indirect call to `Z`.
    Icall,
    /// Absolute jump to a word address.
    Jmp(u32),
    /// Absolute call to a word address.
    Call(u32),
    /// Return from subroutine.
    Ret,
    /// Return from interrupt (also sets the global interrupt enable).
    Reti,
    /// Branch by a signed word offset if `SREG` bit `s` is set (`BREQ`, `BRCS`…).
    Brbs(u8, i8),
    /// Branch by a signed word offset if `SREG` bit `s` is clear (`BRNE`…).
    Brbc(u8, i8),

    /// An encoding this subset does not implement; carries the raw opcode word.
    Unknown(u16),
}
