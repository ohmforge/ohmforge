//! Four-valued logic (IEEE-1164-style `0`/`1`/`X`/`Z`) with multi-driver
//! resolution and `X`-propagating truth tables.
//!
//! `X` is an unknown or contended level; `Z` is high-impedance (not driven).
//! Gate evaluation treats a floating (`Z`) input as unknown (`X`): a gate cannot
//! read a value nobody is driving. Resolution, by contrast, is only meaningful
//! between the *drivers* of a shared net (wired-logic / tri-state buses), which
//! is why it lives apart from the gate truth tables.

/// A four-valued logic level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Logic {
    /// Driven low (`0`).
    Low,
    /// Driven high (`1`).
    High,
    /// Unknown or contended.
    X,
    /// High-impedance (undriven).
    Z,
}

impl Default for Logic {
    /// An unconnected net reads as high-impedance until something drives it.
    fn default() -> Self {
        Logic::Z
    }
}

impl Logic {
    /// Collapse a floating (`Z`) level to unknown (`X`) for gate evaluation; a
    /// definite `0`/`1` is returned unchanged. A gate (or flip-flop) input that
    /// nobody drives is unknowable, not a logic `0`.
    #[inline]
    pub fn sensed(self) -> Logic {
        match self {
            Logic::Z => Logic::X,
            other => other,
        }
    }

    /// Resolve two drivers contending for the same net (wired logic).
    ///
    /// A lone driver wins over high-impedance (`Z`); two drivers that agree yield
    /// that level; any genuine conflict (including with `X`) resolves to `X`.
    ///
    /// ```
    /// # use ohmforge_digital::Logic;
    /// assert_eq!(Logic::Low.resolve(Logic::Z), Logic::Low);      // driver beats Z
    /// assert_eq!(Logic::High.resolve(Logic::High), Logic::High); // agreement
    /// assert_eq!(Logic::Low.resolve(Logic::High), Logic::X);     // conflict → X
    /// assert_eq!(Logic::Z.resolve(Logic::Z), Logic::Z);          // nobody driving
    /// ```
    pub fn resolve(self, other: Logic) -> Logic {
        match (self, other) {
            (Logic::Z, b) => b,
            (a, Logic::Z) => a,
            (a, b) if a == b => a,
            _ => Logic::X,
        }
    }

    /// Logical NOT, propagating `X` (and treating a floating `Z` input as `X`).
    //
    // A plain named method (not `std::ops::Not`) so the four truth-table helpers
    // read symmetrically — `a.not()`, `a.and(b)`, `a.or(b)`, `a.xor(b)` — and
    // because inversion of a *four-valued* level is not boolean `!`.
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Logic {
        match self.sensed() {
            Logic::Low => Logic::High,
            Logic::High => Logic::Low,
            _ => Logic::X,
        }
    }

    /// Logical AND. A single `0` forces the output low even when the other input
    /// is unknown (`0 AND X = 0`); otherwise unknown propagates (`1 AND X = X`).
    pub fn and(self, other: Logic) -> Logic {
        match (self.sensed(), other.sensed()) {
            (Logic::Low, _) | (_, Logic::Low) => Logic::Low,
            (Logic::High, Logic::High) => Logic::High,
            _ => Logic::X,
        }
    }

    /// Logical OR. A single `1` forces the output high even when the other input
    /// is unknown (`1 OR X = 1`); otherwise unknown propagates (`0 OR X = X`).
    pub fn or(self, other: Logic) -> Logic {
        match (self.sensed(), other.sensed()) {
            (Logic::High, _) | (_, Logic::High) => Logic::High,
            (Logic::Low, Logic::Low) => Logic::Low,
            _ => Logic::X,
        }
    }

    /// Logical XOR. Any unknown (or floating) input makes the result unknown.
    pub fn xor(self, other: Logic) -> Logic {
        match (self.sensed(), other.sensed()) {
            (Logic::Low, Logic::Low) | (Logic::High, Logic::High) => Logic::Low,
            (Logic::Low, Logic::High) | (Logic::High, Logic::Low) => Logic::High,
            _ => Logic::X,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Logic::{High, Low, X, Z};

    #[test]
    fn resolution_table() {
        // (a, b, expected). Resolution is symmetric, so each case is checked both
        // ways round.
        let cases = [
            (Z, Z, Z),       // nobody driving
            (Low, Z, Low),   // lone driver beats high-impedance
            (High, Z, High), //
            (Low, Low, Low), // agreement
            (High, High, High),
            (Low, High, X), // two drivers conflict → X
            (X, Low, X),    // X contends with anything → X
            (X, High, X),
            (X, Z, X), // X beats Z (Z is the no-op identity)
            (X, X, X),
        ];
        for (a, b, want) in cases {
            assert_eq!(a.resolve(b), want, "resolve({a:?}, {b:?})");
            assert_eq!(b.resolve(a), want, "resolve({b:?}, {a:?}) (symmetry)");
        }
    }

    #[test]
    fn not_table() {
        assert_eq!(Low.not(), High);
        assert_eq!(High.not(), Low);
        assert_eq!(X.not(), X);
        assert_eq!(Z.not(), X); // floating input is unknown
    }

    #[test]
    fn and_table_propagates_x() {
        let cases = [
            (Low, Low, Low),
            (Low, High, Low),
            (High, High, High),
            (Low, X, Low), // 0 AND X = 0  (0 dominates)
            (High, X, X),  // 1 AND X = X
            (X, X, X),
            (Low, Z, Low), // floating treated as X, but 0 still dominates
            (High, Z, X),
        ];
        for (a, b, want) in cases {
            assert_eq!(a.and(b), want, "and({a:?}, {b:?})");
            assert_eq!(b.and(a), want, "and({b:?}, {a:?}) (symmetry)");
        }
    }

    #[test]
    fn or_table_propagates_x() {
        let cases = [
            (Low, Low, Low),
            (Low, High, High),
            (High, High, High),
            (High, X, High), // 1 OR X = 1  (1 dominates)
            (Low, X, X),     // 0 OR X = X
            (X, X, X),
            (High, Z, High),
            (Low, Z, X),
        ];
        for (a, b, want) in cases {
            assert_eq!(a.or(b), want, "or({a:?}, {b:?})");
            assert_eq!(b.or(a), want, "or({b:?}, {a:?}) (symmetry)");
        }
    }

    #[test]
    fn xor_table_propagates_x() {
        let cases = [
            (Low, Low, Low),
            (Low, High, High),
            (High, High, Low),
            (High, X, X), // any unknown input → unknown
            (Low, X, X),
            (X, X, X),
            (High, Z, X),
        ];
        for (a, b, want) in cases {
            assert_eq!(a.xor(b), want, "xor({a:?}, {b:?})");
            assert_eq!(b.xor(a), want, "xor({b:?}, {a:?}) (symmetry)");
        }
    }
}
