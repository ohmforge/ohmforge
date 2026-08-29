//! The circuit graph — the single source of truth the shell, TUI, and AI all
//! mutate through the same API.
//!
//! A [`Circuit`] is a set of **parts** connected at **nets** (electrically
//! common nodes). Ground is always net 0.

use crate::value::Value;

/// A net (electrical node). Net 0 is ground by convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NetId(pub u32);

impl NetId {
    pub const GROUND: NetId = NetId(0);
}

/// A placed part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PartId(pub u32);

/// The kind of a part. This is deliberately small at M0; `ee-devices` will grow
/// the real model library (see docs/ROADMAP.md).
#[derive(Debug, Clone)]
pub enum PartKind {
    Resistor,
    Capacitor,
    Inductor,
    VoltageSource,
    /// Anything not yet modeled natively, identified by name.
    Other(String),
}

/// A part instance: a kind, a human reference (`R1`), a primary value, and the
/// nets its terminals connect to.
#[derive(Debug, Clone)]
pub struct Part {
    pub id: PartId,
    pub reference: String,
    pub kind: PartKind,
    pub value: Option<Value>,
    pub terminals: Vec<NetId>,
}

/// The whole circuit. `TODO(M0)`: net-merging on `wire`, name lookup, iteration
/// helpers for the solver's stamping pass.
#[derive(Debug, Clone)]
pub struct Circuit {
    pub name: String,
    parts: Vec<Part>,
    next_net: u32,
    next_part: u32,
}

// `Default` is hand-written (not derived) so that `next_net` starts at 1: a
// derived default would make the first `new_net()` return `NetId(0)`, silently
// aliasing the first net with `NetId::GROUND`.
impl Default for Circuit {
    fn default() -> Self {
        Self {
            name: String::new(),
            parts: Vec::new(),
            next_net: 1, // 0 reserved for ground
            next_part: 0,
        }
    }
}

impl Circuit {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Allocate a fresh net id.
    pub fn new_net(&mut self) -> NetId {
        let id = NetId(self.next_net);
        self.next_net += 1;
        id
    }

    /// Add a part; returns its assigned [`PartId`].
    pub fn add_part(
        &mut self,
        reference: impl Into<String>,
        kind: PartKind,
        value: Option<Value>,
        terminals: Vec<NetId>,
    ) -> PartId {
        let id = PartId(self.next_part);
        self.next_part += 1;
        self.parts.push(Part {
            id,
            reference: reference.into(),
            kind,
            value,
            terminals,
        });
        id
    }

    pub fn parts(&self) -> &[Part] {
        &self.parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_net_is_never_ground() {
        // Both constructors must reserve net 0 for ground.
        assert_ne!(Circuit::new("c").new_net(), NetId::GROUND);
        assert_ne!(Circuit::default().new_net(), NetId::GROUND);
    }
}
