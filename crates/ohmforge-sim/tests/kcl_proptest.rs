//! CP-PLAT-M10.2 — a solver property: on random small **linear R/V** networks the
//! operating-point node voltages satisfy **Kirchhoff's current law**. At every
//! node that is a pure resistor junction (i.e. not the voltage-source terminal
//! that carries the branch current), the signed resistor currents must sum to ~0
//! relative to their own magnitude.
//!
//! The circuits are generated connected-by-construction (a spanning tree rooted
//! at the source node, plus random extra edges), so each is always DC-solvable.
//!
//! `proptest` is a dev-dependency only, so `ohmforge-sim` stays dependency-free.

use std::collections::HashMap;

use ohmforge_core::graph::{Circuit, NetId, PartKind};
use ohmforge_core::{Unit, Value};
use ohmforge_sim::{Analysis, Engine};
use proptest::prelude::*;

/// One generated R/V network plus the bookkeeping the KCL check needs.
struct Net {
    circuit: Circuit,
    /// Resistor edges as (node a, node b, resistance Ω).
    edges: Vec<(NetId, NetId, f64)>,
    /// The voltage-source terminal node (skipped — it carries the branch current).
    source_node: NetId,
    /// Every non-ground node (the KCL check runs over these minus `source_node`).
    nodes: Vec<NetId>,
}

/// Build a connected R/V network from a generated spec: a spanning tree (each new
/// node tied by a resistor to an earlier one) rooted at the source node, then a
/// few random extra resistor edges to add loops.
fn build(source_v: f64, tree: &[(usize, f64)], extra: &[(usize, usize, f64)]) -> Net {
    let mut c = Circuit::new("kcl");
    let source_node = c.new_net();
    c.add_part(
        "V1",
        PartKind::VoltageSource,
        Some(Value::new(source_v, Unit::Volt)),
        vec![source_node, NetId::GROUND],
    );

    let mut nodes = vec![source_node];
    let mut edges: Vec<(NetId, NetId, f64)> = Vec::new();

    for (i, &(raw_parent, r)) in tree.iter().enumerate() {
        let parent = nodes[raw_parent % nodes.len()];
        let node = c.new_net();
        c.add_part(
            format!("Rt{i}"),
            PartKind::Resistor,
            Some(Value::new(r, Unit::Ohm)),
            vec![parent, node],
        );
        edges.push((parent, node, r));
        nodes.push(node);
    }

    let total = nodes.len();
    for (j, &(ra, rb, r)) in extra.iter().enumerate() {
        let a = nodes[ra % total];
        let b = nodes[rb % total];
        if a == b {
            continue;
        }
        c.add_part(
            format!("Rx{j}"),
            PartKind::Resistor,
            Some(Value::new(r, Unit::Ohm)),
            vec![a, b],
        );
        edges.push((a, b, r));
    }

    Net {
        circuit: c,
        edges,
        source_node,
        nodes,
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    #[test]
    fn op_point_satisfies_kcl(
        source_v in 1.0f64..20.0,
        // Each entry adds one node tied to an earlier node (parent = raw % count).
        tree in prop::collection::vec((0usize..64, 1.0f64..1e5), 1..7),
        // Extra resistor edges (loops): (node a, node b, Ω).
        extra in prop::collection::vec((0usize..64, 0usize..64, 1.0f64..1e5), 0..6),
    ) {
        let net = build(source_v, &tree, &extra);

        let signals = Engine::new()
            .run(&net.circuit, &Analysis::OperatingPoint)
            .expect("a connected R/V network must have an operating point");

        // name -> node voltage (op-point: one real sample).
        let mut vmap: HashMap<&str, f64> = HashMap::new();
        for s in &signals {
            if let Some(y) = s.real() {
                if let Some(v) = y.first() {
                    vmap.insert(s.name.as_str(), *v);
                }
            }
        }
        let voltage = |net_id: NetId| -> f64 {
            let resolved = net.circuit.resolve(net_id);
            if resolved == NetId::GROUND {
                0.0
            } else {
                let key = format!("v(n{})", resolved.0);
                *vmap
                    .get(key.as_str())
                    .unwrap_or_else(|| panic!("no op-point voltage for {key}"))
            }
        };

        // KCL at every pure-resistor junction (all nodes except the source node).
        for &node in &net.nodes {
            if node == net.source_node {
                continue;
            }
            let vk = voltage(node);
            let mut residual = 0.0f64;
            let mut scale = 0.0f64;
            for &(a, b, r) in &net.edges {
                let other = if a == node {
                    Some(b)
                } else if b == node {
                    Some(a)
                } else {
                    None
                };
                if let Some(other) = other {
                    let i = (vk - voltage(other)) / r; // current leaving `node`
                    residual += i;
                    scale += i.abs();
                }
            }
            prop_assert!(
                residual.abs() <= 1e-6 * scale + 1e-12,
                "KCL residual {residual:e} too large at node {} (scale {scale:e})",
                node.0
            );
        }
    }
}
