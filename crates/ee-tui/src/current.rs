//! Branch-current flow for the live overlay (`CP-UX-M6.1`).
//!
//! The operating-point [`Overlay`](crate::Overlay) already paints *node voltages*
//! onto the schematic. This module is the other half of "watch the circuit work":
//! it derives the **conventional current** through each branch from the same DC
//! solution and turns it into an animated directional glyph — the current-flow
//! animation deferred since `CP-UX-M1.3`/M1.
//!
//! Like the rest of `ee-tui` this is pure and has **no dependency on `ee-sim`**:
//! the branch currents are a function of the circuit graph and the
//! [`NodeVoltages`] the overlay already holds. For a two-terminal resistor the
//! current is Ohm's law, `I = (V₀ − V₁) / R`, flowing from the higher-potential
//! node to the lower one. For an independent voltage source the current it
//! *delivers* is recovered by KCL — the total current leaving its `+` terminal
//! through the surrounding resistors — so the arrow reads "current leaves the `+`
//! terminal of a source and runs through the load". Elements whose current the DC
//! op-point does not pin down locally (a capacitor is an open branch at DC, an
//! inductor a short whose current depends on the whole network) are simply left
//! out rather than guessed.
//!
//! The [animation](flow_glyph) is a pure function of `(magnitude, peak, direction,
//! tick)`: magnitude sets the *density* of the marching arrows and `tick` shifts
//! their phase, so a snapshot at a fixed tick is deterministic and testable while
//! successive ticks animate.

use crate::overlay::NodeVoltages;
use ee_core::graph::{Circuit, NetId, PartKind};

/// The widest a flow stream can march before the gap saturates to "solid".
const MAX_GAP: usize = 4;

/// The conventional current through one branch at the DC operating point.
///
/// `amps` is a non-negative magnitude; the signed direction lives in the
/// `(from, to)` pair of **resolved** [`NetId`]s — positive current flows out of
/// `from` and into `to`. For a resistor that is high-potential → low-potential;
/// for a source it is "out of the `+` terminal, into the load".
#[derive(Debug, Clone, PartialEq)]
pub struct BranchCurrent {
    /// The part this current runs through.
    pub reference: String,
    /// The resolved net the (positive) current flows *from*.
    pub from: NetId,
    /// The resolved net the (positive) current flows *to*.
    pub to: NetId,
    /// Current magnitude in amperes (always `>= 0`).
    pub amps: f64,
}

/// Every branch current the op-point determines, keyed by part reference.
#[derive(Debug, Clone, Default)]
pub struct BranchCurrents {
    items: Vec<BranchCurrent>,
}

impl BranchCurrents {
    /// No currents — every branch reads as unknown (a static rail in the overlay).
    pub fn empty() -> Self {
        Self::default()
    }

    /// The branch current through the part named `reference`, if one was derived.
    pub fn get(&self, reference: &str) -> Option<&BranchCurrent> {
        self.items.iter().find(|c| c.reference == reference)
    }

    /// The peak magnitude across every branch — the scale the density ramps to.
    pub fn max_magnitude(&self) -> f64 {
        self.items.iter().map(|c| c.amps).fold(0.0, f64::max)
    }

    /// Every derived branch current (unordered by part, as the circuit lists them).
    pub fn as_slice(&self) -> &[BranchCurrent] {
        &self.items
    }

    /// Whether no branch current was derived at all.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Derive the per-branch currents of `circuit` at the operating point `volts`.
///
/// Pure and deterministic. Resistors use Ohm's law; independent voltage sources
/// use KCL at their `+` terminal (see the module docs); any other element, or one
/// whose nodes are unsolved, is omitted. The direction is folded into each
/// [`BranchCurrent`]'s `(from, to)` so `amps` is always non-negative.
pub fn branch_currents(circuit: &Circuit, volts: &NodeVoltages) -> BranchCurrents {
    let mut items = Vec::new();
    for p in circuit.parts() {
        let [t0, t1] = match p.terminals.as_slice() {
            [a, b] => [*a, *b],
            _ => continue, // only two-terminal branches carry a single current
        };
        let (n0, n1) = (circuit.resolve(t0), circuit.resolve(t1));
        // Signed in the n0 → n1 sense (positive = current flows out of n0).
        let signed = match &p.kind {
            PartKind::Resistor => match (p.value, volts.get(n0), volts.get(n1)) {
                (Some(v), Some(a), Some(b)) if v.magnitude > 0.0 => Some((a - b) / v.magnitude),
                _ => None,
            },
            PartKind::VoltageSource => source_delivery(circuit, volts, &p.reference, n0),
            _ => None,
        };
        let Some(i) = signed else { continue };
        let (from, to, amps) = if i >= 0.0 { (n0, n1, i) } else { (n1, n0, -i) };
        items.push(BranchCurrent {
            reference: p.reference.clone(),
            from,
            to,
            amps,
        });
    }
    BranchCurrents { items }
}

/// Current an independent source delivers out of its `+` node, by KCL.
///
/// The node `plus` is the source's `+` terminal; the current it pushes into the
/// circuit there equals the sum of the currents leaving `plus` through every
/// *other* resistor. Positive means current genuinely leaves the `+` terminal.
/// `None` when the node is unsolved or no resistor is incident, so a source with
/// nothing computable is omitted rather than drawn as zero.
fn source_delivery(
    circuit: &Circuit,
    volts: &NodeVoltages,
    this_ref: &str,
    plus: NetId,
) -> Option<f64> {
    let vp = volts.get(plus)?;
    let mut sum = 0.0;
    let mut any = false;
    for p in circuit.parts() {
        if p.reference == this_ref || !matches!(p.kind, PartKind::Resistor) {
            continue;
        }
        let (Some(v), [a, b]) = (p.value, p.terminals.as_slice()) else {
            continue;
        };
        if v.magnitude <= 0.0 {
            continue;
        }
        let (ra, rb) = (circuit.resolve(*a), circuit.resolve(*b));
        let other = if ra == plus {
            Some(rb)
        } else if rb == plus {
            Some(ra)
        } else {
            None
        };
        if let Some(o) = other {
            if let Some(vo) = volts.get(o) {
                sum += (vp - vo) / v.magnitude; // current leaving `plus` toward `o`
                any = true;
            }
        }
    }
    any.then_some(sum)
}

/// A marching-arrow flow stream of exactly `cells` characters.
///
/// Pure and deterministic. `mag` is this branch's magnitude and `peak` the largest
/// in view; their ratio sets the *density* — a faint trickle at a tenth of the
/// peak places one arrow every few cells, a full-scale branch fills every
/// cell. `forward` points the arrow with the net order (`→`) or against it (`←`),
/// and `tick` shifts the phase so consecutive frames animate (a leftward stream
/// marches the opposite way). A branch with no current, or no peak to scale
/// against, is a static dotted rail.
pub fn flow_glyph(mag: f64, peak: f64, forward: bool, tick: u64, cells: usize) -> String {
    if cells == 0 {
        return String::new();
    }
    if peak <= 0.0 || mag <= 0.0 {
        return "·".repeat(cells);
    }
    let arrow = if forward { '→' } else { '←' };
    let frac = (mag / peak).clamp(0.0, 1.0);
    // level 1..=MAX_GAP; a bigger fraction => bigger level => smaller gap => denser.
    let level = (frac * MAX_GAP as f64).ceil().max(1.0) as usize;
    let gap = ((MAX_GAP + 1).saturating_sub(level)).max(1) as u64;
    let t = tick % gap;
    (0..cells as u64)
        .map(|i| {
            let phase = if forward {
                (i + t) % gap
            } else {
                (i + gap - t) % gap
            };
            if phase == 0 {
                arrow
            } else {
                ' '
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee_core::units::Unit;
    use ee_core::value::Value;

    /// V1 = 5 V at n1, R1 = 1 kΩ from n1 to ground. The 5 mA loop current leaves
    /// V1's `+` terminal (n1) and runs through R1 to ground.
    fn source_and_load() -> (Circuit, NodeVoltages) {
        let mut c = Circuit::new("vr");
        let n1 = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![n1, NetId::GROUND],
        );
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(1000.0, Unit::Ohm)),
            vec![n1, NetId::GROUND],
        );
        let mut v = NodeVoltages::new();
        v.set(c.resolve(n1), 5.0);
        (c, v)
    }

    #[test]
    fn resistor_current_is_ohms_law_from_high_to_low_potential() {
        let (c, v) = source_and_load();
        let n1 = c.resolve(NetId(1));
        let currents = branch_currents(&c, &v);
        let r1 = currents.get("R1").expect("R1 carries a current");
        assert!((r1.amps - 5e-3).abs() < 1e-12, "I = 5 V / 1 kΩ = 5 mA");
        // Conventional current runs from the high node (n1) down to ground.
        assert_eq!(r1.from, n1, "flows from n1 (the source's + node)");
        assert_eq!(r1.to, NetId::GROUND, "through the load to ground");
    }

    #[test]
    fn source_current_leaves_the_plus_terminal() {
        let (c, v) = source_and_load();
        let n1 = c.resolve(NetId(1));
        let currents = branch_currents(&c, &v);
        let v1 = currents.get("V1").expect("V1 delivers a current");
        assert!((v1.amps - 5e-3).abs() < 1e-12, "the source delivers 5 mA");
        // The gate: current leaves the + terminal (n1) of the source.
        assert_eq!(v1.from, n1, "current leaves V1's + terminal (n1)");
        assert_eq!(v1.to, NetId::GROUND);
    }

    #[test]
    fn reversed_polarity_flips_the_arrow() {
        // Same magnitude, opposite node order: n1 is now the *low* side of R1.
        let mut c = Circuit::new("rev");
        let n1 = c.new_net();
        c.add_part(
            "R1",
            PartKind::Resistor,
            Some(Value::new(1000.0, Unit::Ohm)),
            vec![NetId::GROUND, n1], // terminals reversed
        );
        let mut v = NodeVoltages::new();
        v.set(c.resolve(n1), 5.0);
        let r1 = branch_currents(&c, &v).get("R1").cloned().unwrap();
        assert_eq!(
            r1.from,
            c.resolve(n1),
            "current still flows from the high node"
        );
        assert_eq!(r1.to, NetId::GROUND);
    }

    #[test]
    fn unsolved_or_zero_value_branches_are_omitted() {
        let mut c = Circuit::new("part");
        let n1 = c.new_net();
        let n2 = c.new_net();
        c.add_part("R1", PartKind::Resistor, None, vec![n1, n2]); // no value
        c.add_part(
            "C1",
            PartKind::Capacitor,
            Some(Value::new(1e-6, Unit::Farad)),
            vec![n1, n2],
        ); // open at DC
        let mut v = NodeVoltages::new();
        v.set(c.resolve(n1), 5.0); // n2 left unsolved
        let currents = branch_currents(&c, &v);
        assert!(currents.is_empty(), "nothing computable: {currents:?}");
    }

    #[test]
    fn density_encodes_magnitude_at_a_fixed_tick() {
        let strong = flow_glyph(5e-3, 5e-3, true, 0, 8);
        let weak = flow_glyph(0.5e-3, 5e-3, true, 0, 8);
        let n_strong = strong.chars().filter(|&c| c == '→').count();
        let n_weak = weak.chars().filter(|&c| c == '→').count();
        assert!(
            n_strong > n_weak,
            "a bigger current is denser: {n_strong} vs {n_weak} ({strong:?} / {weak:?})"
        );
        // Full-scale is a solid stream.
        assert_eq!(
            strong.chars().filter(|&c| c == '→').count(),
            8,
            "{strong:?}"
        );
    }

    #[test]
    fn direction_picks_the_arrow_glyph() {
        let fwd = flow_glyph(5e-3, 5e-3, true, 0, 4);
        let rev = flow_glyph(5e-3, 5e-3, false, 0, 4);
        assert!(fwd.contains('→') && !fwd.contains('←'), "{fwd:?}");
        assert!(rev.contains('←') && !rev.contains('→'), "{rev:?}");
    }

    #[test]
    fn animation_is_deterministic_and_phase_shifts_with_tick() {
        // Same inputs -> identical frame (deterministic).
        assert_eq!(
            flow_glyph(2e-3, 5e-3, true, 3, 8),
            flow_glyph(2e-3, 5e-3, true, 3, 8)
        );
        // A mid-density stream (gap > 1) shifts as the tick advances.
        let a = flow_glyph(2e-3, 5e-3, true, 0, 8);
        let b = flow_glyph(2e-3, 5e-3, true, 1, 8);
        assert_ne!(a, b, "the pattern marches: {a:?} -> {b:?}");
    }

    #[test]
    fn zero_current_is_a_static_rail() {
        assert_eq!(flow_glyph(0.0, 5e-3, true, 7, 5), "·····");
        assert_eq!(flow_glyph(1e-3, 0.0, true, 0, 3), "···");
    }
}
