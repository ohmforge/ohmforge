//! Physical units. Every quantity in ohmforge carries one, so results print
//! correctly and dimensionally-wrong wiring can be caught early.

/// The physical dimension of a [`crate::Value`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unit {
    Volt,
    Ampere,
    Ohm,
    Farad,
    Henry,
    Hertz,
    Second,
    Watt,
    /// Dimensionless (gain, count, ratio).
    None,
}

impl Unit {
    /// The SI symbol used when formatting values.
    pub const fn symbol(self) -> &'static str {
        match self {
            Unit::Volt => "V",
            Unit::Ampere => "A",
            Unit::Ohm => "Ω",
            Unit::Farad => "F",
            Unit::Henry => "H",
            Unit::Hertz => "Hz",
            Unit::Second => "s",
            Unit::Watt => "W",
            Unit::None => "",
        }
    }
}
