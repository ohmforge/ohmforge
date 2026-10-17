//! The AVR status register (`SREG`) and its eight flags.
//!
//! Bit order in the packed byte (LSB first) is `C Z N V S H T I`, matching the
//! hardware so `IN`/`OUT` of address `0x3F` and `PUSH`/`POP` of `SREG` round-trip
//! exactly.

/// Bit index of each `SREG` flag, as the `BSET`/`BCLR`/`BRBS`/`BRBC` encodings
/// number them.
pub mod bit {
    /// Carry.
    pub const C: u8 = 0;
    /// Zero.
    pub const Z: u8 = 1;
    /// Negative.
    pub const N: u8 = 2;
    /// Two's-complement overflow.
    pub const V: u8 = 3;
    /// Sign (`N ^ V`).
    pub const S: u8 = 4;
    /// Half-carry (carry out of bit 3).
    pub const H: u8 = 5;
    /// Bit-copy storage (used by `BST`/`BLD`).
    pub const T: u8 = 6;
    /// Global interrupt enable.
    pub const I: u8 = 7;
}

/// The status register, unpacked into its eight boolean flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sreg {
    /// Carry flag.
    pub c: bool,
    /// Zero flag.
    pub z: bool,
    /// Negative flag.
    pub n: bool,
    /// Two's-complement overflow flag.
    pub v: bool,
    /// Sign flag (`N ^ V`).
    pub s: bool,
    /// Half-carry flag.
    pub h: bool,
    /// Bit-copy storage flag.
    pub t: bool,
    /// Global interrupt enable.
    pub i: bool,
}

impl Sreg {
    /// Pack the flags into the hardware `SREG` byte.
    pub fn to_u8(self) -> u8 {
        (self.c as u8)
            | (self.z as u8) << 1
            | (self.n as u8) << 2
            | (self.v as u8) << 3
            | (self.s as u8) << 4
            | (self.h as u8) << 5
            | (self.t as u8) << 6
            | (self.i as u8) << 7
    }

    /// Unpack a hardware `SREG` byte into flags.
    pub fn from_u8(v: u8) -> Self {
        Self {
            c: v & 1 != 0,
            z: v & 1 << 1 != 0,
            n: v & 1 << 2 != 0,
            v: v & 1 << 3 != 0,
            s: v & 1 << 4 != 0,
            h: v & 1 << 5 != 0,
            t: v & 1 << 6 != 0,
            i: v & 1 << 7 != 0,
        }
    }

    /// Read one flag by its bit index (see [`bit`]).
    pub fn get(self, index: u8) -> bool {
        self.to_u8() & (1 << index) != 0
    }

    /// Set or clear one flag by its bit index (see [`bit`]).
    pub fn set(&mut self, index: u8, value: bool) {
        let mut byte = self.to_u8();
        if value {
            byte |= 1 << index;
        } else {
            byte &= !(1 << index);
        }
        *self = Sreg::from_u8(byte);
    }
}

#[cfg(test)]
mod tests {
    use super::{bit, Sreg};

    #[test]
    fn packs_and_unpacks_round_trip() {
        for v in 0u8..=255 {
            assert_eq!(Sreg::from_u8(v).to_u8(), v, "round-trip {v:#04x}");
        }
    }

    #[test]
    fn bit_indices_match_the_packed_layout() {
        let mut s = Sreg::default();
        s.set(bit::I, true);
        assert!(s.i);
        assert_eq!(s.to_u8(), 0x80);
        s.set(bit::C, true);
        assert!(s.get(bit::C));
        assert_eq!(s.to_u8(), 0x81);
    }
}
