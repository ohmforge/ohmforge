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

/// The whole circuit — the single source of truth every front end mutates.
///
/// Nets are merged with a **union-find**: [`wire`](Circuit::wire) unions two net
/// ids in near-constant time regardless of order, and [`resolve`](Circuit::resolve)
/// maps any net id to the representative the solver stamps against. Raw ids stay
/// stable for the UI; resolution happens only at stamp time.
#[derive(Debug, Clone)]
pub struct Circuit {
    pub name: String,
    parts: Vec<Part>,
    /// Union-find parent per net id, indexed by id; `net_parent[i] == i` is a root.
    /// `net_parent[0]` is ground and always its own root.
    net_parent: Vec<u32>,
    next_part: u32,
}

// `Default` is hand-written (not derived) so net 0 is pre-seeded as ground: the
// first `new_net()` therefore returns `NetId(1)`, never aliasing `NetId::GROUND`.
impl Default for Circuit {
    fn default() -> Self {
        Self {
            name: String::new(),
            parts: Vec::new(),
            net_parent: vec![0], // net 0 = ground, its own root
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

    /// Allocate a fresh net id (its own union-find root until wired).
    pub fn new_net(&mut self) -> NetId {
        let id = self.net_parent.len() as u32; // ids are dense from 0
        self.net_parent.push(id);
        NetId(id)
    }

    /// Electrically merge two nets into one common node. Order-independent and
    /// idempotent; ground (net 0) always remains the representative of its group.
    pub fn wire(&mut self, a: NetId, b: NetId) {
        let (ra, rb) = (self.find(a.0), self.find(b.0));
        if ra == rb {
            return;
        }
        // Attach the larger id under the smaller so `NetId::GROUND` (0) stays a root.
        let (root, child) = if ra < rb { (ra, rb) } else { (rb, ra) };
        self.net_parent[child as usize] = root;
    }

    /// Resolve a net id to its group representative — what the solver stamps.
    pub fn resolve(&self, net: NetId) -> NetId {
        NetId(self.find(net.0))
    }

    /// Union-find root walk (no path compression; `&self` for stamp-time use).
    fn find(&self, mut n: u32) -> u32 {
        while self.net_parent[n as usize] != n {
            n = self.net_parent[n as usize];
        }
        n
    }

    /// Find a part by its reference designator (e.g. `"R1"`).
    pub fn part_by_ref(&self, reference: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.reference == reference)
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

    #[test]
    fn wire_merges_nets_order_independently() {
        let mut c = Circuit::new("c");
        let (a, b, d) = (c.new_net(), c.new_net(), c.new_net());
        c.wire(b, a); // order shouldn't matter
        c.wire(d, b); // chain a—b—d into one node
        assert_eq!(c.resolve(a), c.resolve(b));
        assert_eq!(c.resolve(a), c.resolve(d));
        // Idempotent and self-consistent.
        c.wire(a, d);
        assert_eq!(c.resolve(d), c.resolve(a));
    }

    #[test]
    fn ground_stays_the_representative() {
        let mut c = Circuit::new("c");
        let n = c.new_net();
        c.wire(n, NetId::GROUND);
        assert_eq!(
            c.resolve(n),
            NetId::GROUND,
            "wiring to ground pulls the net to ground"
        );
        assert_eq!(c.resolve(NetId::GROUND), NetId::GROUND);
    }

    #[test]
    fn part_lookup_by_reference() {
        let mut c = Circuit::new("c");
        let (a, b) = (c.new_net(), c.new_net());
        c.add_part("R1", PartKind::Resistor, None, vec![a, b]);
        assert!(c.part_by_ref("R1").is_some());
        assert!(c.part_by_ref("R2").is_none());
    }
}
