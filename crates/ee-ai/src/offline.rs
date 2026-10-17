//! Offline mode (`CP-AF-M3.2`) — a deterministic copilot that needs **no API
//! key, no network, and no model** at all.
//!
//! Every other mode in this crate reaches the model through a [`Transport`]
//! (even the tests, via the [`MockTransport`]). This module reaches *nothing*:
//! it is pure functions over an [`ee_core::graph::Circuit`] and a static parts
//! catalog, so it runs identically on a plane, in CI, or in a browser with no
//! TLS stack — and it is the honest fallback when
//! [`api_key_from_env`](crate::config::api_key_from_env) returns
//! [`AiError::MissingApiKey`](crate::error::AiError::MissingApiKey).
//!
//! It imports no [`Transport`], [`Client`](crate::client::Client), or
//! [`config`](crate::config) — the "no key needed" property is structural, not a
//! runtime check. Three capabilities:
//!
//! * **Design-rule checks** ([`check`]) — [`drc`] walks the circuit graph and
//!   flags a floating node, a node with no path to ground, an LED/diode with no
//!   series resistor, and a short across a source.
//! * **Value calculators** ([`calc`]) — divider output, RC/RL time constant and
//!   cutoff, series/parallel resistance, and the series resistor for an LED at a
//!   target current. Pure `f64 -> Result`, with a friendly [`CalcError`].
//! * **Parts index** ([`parts`]) — a tiny static catalog with a keyword search,
//!   a RAG-style lookup with no embeddings and no network.
//!
//! [`Transport`]: crate::transport::Transport
//! [`MockTransport`]: crate::transport::MockTransport

use ee_core::graph::{Circuit, NetId, PartKind};

pub use calc::CalcError;
pub use drc::{check, Finding, Rule, Severity};
pub use parts::{catalog, search, CatalogEntry};

/// Run every offline design-rule check and summarize the findings as plain text,
/// deterministically. An empty report (no findings) reads as a clean bill.
///
/// This is the one-call "offline review" surface; [`check`] returns the
/// structured [`Finding`]s if a caller wants to render them itself.
pub fn review(circuit: &Circuit) -> String {
    let findings = check(circuit);
    if findings.is_empty() {
        return "No design-rule issues found.".to_string();
    }
    let mut out = format!(
        "{} design-rule {} found:\n",
        findings.len(),
        if findings.len() == 1 {
            "issue"
        } else {
            "issues"
        }
    );
    for f in &findings {
        out.push_str(&format!("  [{}] {}\n", f.severity.tag(), f.message));
    }
    out
}

// ── Design-rule checks ───────────────────────────────────────────────────────

/// Design-rule checks over a [`Circuit`]. Pure, deterministic graph analysis —
/// no solve, no model.
pub mod drc {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    /// How serious a [`Finding`] is.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub enum Severity {
        /// A problem that will stop the circuit working (a short, a dead node).
        Error,
        /// A likely mistake worth a look (a part with no series resistor).
        Warning,
    }

    impl Severity {
        /// A short uppercase tag for text reports (`ERROR` / `WARN`).
        pub fn tag(self) -> &'static str {
            match self {
                Severity::Error => "ERROR",
                Severity::Warning => "WARN",
            }
        }
    }

    /// Which rule produced a [`Finding`]. The order here is the order findings
    /// are reported in.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub enum Rule {
        /// A voltage source whose two terminals are the same node (a dead short),
        /// or a ~0 Ω part bridging a source's two nodes.
        ShortAcrossSource,
        /// A node with no conductive path to ground.
        NoPathToGround,
        /// A node wired to only one terminal — it can carry no steady current.
        FloatingNode,
        /// A diode/LED with no current-limiting resistor in series.
        LedNoSeriesResistor,
    }

    /// One design-rule finding: the rule, its severity, the parts (and/or node)
    /// involved, and a human-readable message.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Finding {
        /// The rule that fired.
        pub rule: Rule,
        /// How serious it is.
        pub severity: Severity,
        /// The reference designators involved, sorted. May be empty for a
        /// node-only finding.
        pub refs: Vec<String>,
        /// A one-line explanation naming the node and/or parts.
        pub message: String,
    }

    /// Run every design-rule check over `circuit` and return the findings in a
    /// deterministic order (by [`Rule`], then by the node/part named). An empty
    /// vec means the circuit passed every rule.
    pub fn check(circuit: &Circuit) -> Vec<Finding> {
        let mut findings = Vec::new();
        findings.extend(short_across_source(circuit));
        findings.extend(no_path_to_ground(circuit));
        findings.extend(floating_nodes(circuit));
        findings.extend(led_without_series_resistor(circuit));
        // Stable, rule-major order; messages already carry the node/part so the
        // secondary sort on them is deterministic.
        findings.sort_by(|a, b| a.rule.cmp(&b.rule).then_with(|| a.message.cmp(&b.message)));
        findings
    }

    /// The resolved net a terminal lands on.
    fn res(circuit: &Circuit, net: NetId) -> u32 {
        circuit.resolve(net).0
    }

    /// Count how many terminal occurrences land on each resolved net.
    fn pin_counts(circuit: &Circuit) -> BTreeMap<u32, usize> {
        let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
        for part in circuit.parts() {
            for &t in &part.terminals {
                *counts.entry(res(circuit, t)).or_default() += 1;
            }
        }
        counts
    }

    /// A node wired to exactly one terminal carries no steady current — flag it
    /// (ground is exempt; it is the reference, not a dangling stub).
    fn floating_nodes(circuit: &Circuit) -> Vec<Finding> {
        // Which part reference touches each net, so the message can name it.
        let mut touch: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        for part in circuit.parts() {
            for &t in &part.terminals {
                touch
                    .entry(res(circuit, t))
                    .or_default()
                    .push(part.reference.clone());
            }
        }
        let mut out = Vec::new();
        for (net, count) in pin_counts(circuit) {
            if net == NetId::GROUND.0 || count != 1 {
                continue;
            }
            let refs = touch.remove(&net).unwrap_or_default();
            let who = refs.first().cloned().unwrap_or_default();
            out.push(Finding {
                rule: Rule::FloatingNode,
                severity: Severity::Warning,
                refs,
                message: format!(
                    "floating node n{net}: wired only to {who} — it carries no steady current"
                ),
            });
        }
        out
    }

    /// Build net adjacency (each part bridges all its terminal nets) and BFS from
    /// ground; any net present in the circuit but unreached has no path to ground.
    fn no_path_to_ground(circuit: &Circuit) -> Vec<Finding> {
        let mut adj: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        let mut all: BTreeSet<u32> = BTreeSet::new();
        all.insert(NetId::GROUND.0);
        for part in circuit.parts() {
            let nets: Vec<u32> = part.terminals.iter().map(|&t| res(circuit, t)).collect();
            for &n in &nets {
                all.insert(n);
            }
            for (i, &a) in nets.iter().enumerate() {
                for &b in &nets[i + 1..] {
                    adj.entry(a).or_default().insert(b);
                    adj.entry(b).or_default().insert(a);
                }
            }
        }

        // BFS from ground.
        let mut seen: BTreeSet<u32> = BTreeSet::new();
        let mut queue: VecDeque<u32> = VecDeque::new();
        seen.insert(NetId::GROUND.0);
        queue.push_back(NetId::GROUND.0);
        while let Some(n) = queue.pop_front() {
            if let Some(neighbors) = adj.get(&n) {
                for &m in neighbors {
                    if seen.insert(m) {
                        queue.push_back(m);
                    }
                }
            }
        }

        all.into_iter()
            .filter(|&n| n != NetId::GROUND.0 && !seen.contains(&n))
            .map(|net| Finding {
                rule: Rule::NoPathToGround,
                severity: Severity::Error,
                refs: Vec::new(),
                message: format!("node n{net} has no path to ground"),
            })
            .collect()
    }

    /// A diode with no current-limiting resistor sharing either of its nodes is
    /// an LED about to let out its magic smoke — flag it.
    fn led_without_series_resistor(circuit: &Circuit) -> Vec<Finding> {
        // Nets that have a resistor attached.
        let mut resistor_nets: BTreeSet<u32> = BTreeSet::new();
        for part in circuit.parts() {
            if matches!(part.kind, PartKind::Resistor) {
                for &t in &part.terminals {
                    resistor_nets.insert(res(circuit, t));
                }
            }
        }

        let mut out = Vec::new();
        for part in circuit.parts() {
            if !matches!(part.kind, PartKind::Diode) {
                continue;
            }
            let has_series_r = part
                .terminals
                .iter()
                .any(|&t| resistor_nets.contains(&res(circuit, t)));
            if !has_series_r {
                out.push(Finding {
                    rule: Rule::LedNoSeriesResistor,
                    severity: Severity::Warning,
                    refs: vec![part.reference.clone()],
                    message: format!(
                        "{} is a diode/LED with no series resistor — add one to limit current",
                        part.reference
                    ),
                });
            }
        }
        out
    }

    /// A voltage source whose two terminals are the same node is a dead short; so
    /// is a ~0 Ω resistor/inductor bridging a source's two nodes.
    fn short_across_source(circuit: &Circuit) -> Vec<Finding> {
        let mut out = Vec::new();
        for part in circuit.parts() {
            if !matches!(part.kind, PartKind::VoltageSource) || part.terminals.len() < 2 {
                continue;
            }
            let a = res(circuit, part.terminals[0]);
            let b = res(circuit, part.terminals[1]);
            if a == b {
                out.push(Finding {
                    rule: Rule::ShortAcrossSource,
                    severity: Severity::Error,
                    refs: vec![part.reference.clone()],
                    message: format!(
                        "{} is shorted: both terminals are the same node (n{a})",
                        part.reference
                    ),
                });
                continue;
            }
            // A near-zero-ohm part directly across the same two nodes shorts it.
            for other in circuit.parts() {
                if other.reference == part.reference || other.terminals.len() < 2 {
                    continue;
                }
                let is_zero_ohmic = matches!(other.kind, PartKind::Resistor | PartKind::Inductor)
                    && other.value.map(|v| v.magnitude == 0.0).unwrap_or(false);
                if !is_zero_ohmic {
                    continue;
                }
                let oa = res(circuit, other.terminals[0]);
                let ob = res(circuit, other.terminals[1]);
                if (oa == a && ob == b) || (oa == b && ob == a) {
                    out.push(Finding {
                        rule: Rule::ShortAcrossSource,
                        severity: Severity::Error,
                        refs: vec![other.reference.clone(), part.reference.clone()],
                        message: format!(
                            "{} shorts {} with a 0 Ω part across its terminals",
                            other.reference, part.reference
                        ),
                    });
                }
            }
        }
        out
    }
}

// ── Value calculators ─────────────────────────────────────────────────────────

/// Pure value calculators for the back-of-the-envelope sums the copilot does
/// without a model. Every function returns a [`CalcError`] for a degenerate or
/// non-physical input rather than a silent `NaN`/`inf`.
pub mod calc {
    use std::error::Error;
    use std::fmt;

    /// Why a calculator rejected its inputs.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CalcError {
        /// A value that must be finite was `NaN` or infinite.
        NotFinite,
        /// A value that must be positive was zero or negative. Names the quantity.
        NonPositive(&'static str),
        /// A value that must be non-negative was negative. Names the quantity.
        Negative(&'static str),
        /// A voltage divider's two resistances summed to zero (no divider).
        DegenerateDivider,
        /// An LED's forward voltage was not below the supply (no headroom).
        ForwardExceedsSupply,
        /// A list input (series/parallel) was empty.
        Empty,
    }

    impl fmt::Display for CalcError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                CalcError::NotFinite => write!(f, "a value was not finite"),
                CalcError::NonPositive(q) => write!(f, "{q} must be greater than zero"),
                CalcError::Negative(q) => write!(f, "{q} must not be negative"),
                CalcError::DegenerateDivider => {
                    write!(f, "the two divider resistances sum to zero — no divider")
                }
                CalcError::ForwardExceedsSupply => {
                    write!(f, "the LED forward voltage is not below the supply voltage")
                }
                CalcError::Empty => write!(f, "no values were given"),
            }
        }
    }

    impl Error for CalcError {}

    fn finite(x: f64) -> Result<f64, CalcError> {
        if x.is_finite() {
            Ok(x)
        } else {
            Err(CalcError::NotFinite)
        }
    }

    fn positive(x: f64, name: &'static str) -> Result<f64, CalcError> {
        let x = finite(x)?;
        if x > 0.0 {
            Ok(x)
        } else {
            Err(CalcError::NonPositive(name))
        }
    }

    fn non_negative(x: f64, name: &'static str) -> Result<f64, CalcError> {
        let x = finite(x)?;
        if x >= 0.0 {
            Ok(x)
        } else {
            Err(CalcError::Negative(name))
        }
    }

    /// The output of an unloaded resistive divider: `vin · r_bottom /
    /// (r_top + r_bottom)`, where `r_bottom` is the leg across the output. Both
    /// resistances must be finite and non-negative and must not sum to zero.
    pub fn voltage_divider(vin: f64, r_top: f64, r_bottom: f64) -> Result<f64, CalcError> {
        let vin = finite(vin)?;
        let r_top = non_negative(r_top, "the top resistance")?;
        let r_bottom = non_negative(r_bottom, "the bottom resistance")?;
        let total = r_top + r_bottom;
        if total == 0.0 {
            return Err(CalcError::DegenerateDivider);
        }
        Ok(vin * r_bottom / total)
    }

    /// The RC time constant `τ = R · C` (seconds). Both must be positive.
    pub fn rc_time_constant(r: f64, c: f64) -> Result<f64, CalcError> {
        Ok(positive(r, "the resistance")? * positive(c, "the capacitance")?)
    }

    /// The RL time constant `τ = L / R` (seconds). Both must be positive.
    pub fn rl_time_constant(r: f64, l: f64) -> Result<f64, CalcError> {
        Ok(positive(l, "the inductance")? / positive(r, "the resistance")?)
    }

    /// The −3 dB cutoff of a first-order RC, `f_c = 1 / (2π·R·C)` (hertz). Both
    /// must be positive.
    pub fn rc_cutoff_hz(r: f64, c: f64) -> Result<f64, CalcError> {
        let tau = rc_time_constant(r, c)?;
        Ok(1.0 / (std::f64::consts::TAU * tau))
    }

    /// The total of resistances in series — their sum. The list must be non-empty
    /// and every value finite and non-negative.
    pub fn series_resistance(resistances: &[f64]) -> Result<f64, CalcError> {
        if resistances.is_empty() {
            return Err(CalcError::Empty);
        }
        let mut sum = 0.0;
        for &r in resistances {
            sum += non_negative(r, "a resistance")?;
        }
        Ok(sum)
    }

    /// The equivalent of resistances in parallel, `1 / Σ(1/Rᵢ)`. The list must be
    /// non-empty and every value strictly positive (a 0 Ω leg shorts the group).
    pub fn parallel_resistance(resistances: &[f64]) -> Result<f64, CalcError> {
        if resistances.is_empty() {
            return Err(CalcError::Empty);
        }
        let mut recip = 0.0;
        for &r in resistances {
            recip += 1.0 / positive(r, "a resistance")?;
        }
        Ok(1.0 / recip)
    }

    /// The series resistor for an LED: `(V_supply − V_forward) / I_target` (ohms).
    /// The supply must be finite, the forward voltage finite and below it, and the
    /// target current positive.
    pub fn led_series_resistor(
        v_supply: f64,
        v_forward: f64,
        i_target: f64,
    ) -> Result<f64, CalcError> {
        let v_supply = finite(v_supply)?;
        let v_forward = finite(v_forward)?;
        let i_target = positive(i_target, "the target current")?;
        if v_forward >= v_supply {
            return Err(CalcError::ForwardExceedsSupply);
        }
        Ok((v_supply - v_forward) / i_target)
    }
}

// ── Parts index (RAG-style, static, offline) ──────────────────────────────────

/// A tiny static parts catalog and a keyword search over it — a RAG-style
/// lookup with no embeddings and no network, just substring keyword scoring.
pub mod parts {
    /// One catalog entry: a canonical part number, a category, a one-line
    /// summary, and the keywords the search matches against.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CatalogEntry {
        /// The canonical part number, e.g. `"1N4148"`.
        pub part_number: &'static str,
        /// The broad category, e.g. `"diode"`.
        pub category: &'static str,
        /// A one-line human summary.
        pub summary: &'static str,
        /// Search keywords (lowercase).
        pub keywords: &'static [&'static str],
    }

    /// The static catalog. Small and representative, enough to ground an
    /// offline "what part should I use?" question.
    pub fn catalog() -> &'static [CatalogEntry] {
        CATALOG
    }

    static CATALOG: &[CatalogEntry] = &[
        CatalogEntry {
            part_number: "1N4148",
            category: "diode",
            summary: "Fast small-signal switching diode, 100 V / 200 mA.",
            keywords: &["diode", "signal", "switching", "fast", "small"],
        },
        CatalogEntry {
            part_number: "1N4007",
            category: "diode",
            summary: "General-purpose rectifier diode, 1000 V / 1 A.",
            keywords: &["diode", "rectifier", "power", "mains", "bridge"],
        },
        CatalogEntry {
            part_number: "1N5819",
            category: "diode",
            summary: "Schottky diode, 40 V / 1 A, low forward drop.",
            keywords: &["diode", "schottky", "low", "drop", "flyback"],
        },
        CatalogEntry {
            part_number: "2N3904",
            category: "transistor",
            summary: "NPN BJT, 40 V / 200 mA, general-purpose small-signal.",
            keywords: &["transistor", "bjt", "npn", "switch", "amplifier"],
        },
        CatalogEntry {
            part_number: "2N7000",
            category: "transistor",
            summary: "N-channel MOSFET, 60 V / 200 mA logic-level switch.",
            keywords: &["transistor", "mosfet", "nmos", "logic", "switch"],
        },
        CatalogEntry {
            part_number: "NE555",
            category: "ic",
            summary: "Timer IC for astable/monostable oscillators and delays.",
            keywords: &["timer", "oscillator", "astable", "monostable", "555", "pwm"],
        },
        CatalogEntry {
            part_number: "LM358",
            category: "ic",
            summary: "Dual op-amp, single-supply, general-purpose.",
            keywords: &["opamp", "op-amp", "amplifier", "dual", "comparator"],
        },
        CatalogEntry {
            part_number: "LM7805",
            category: "regulator",
            summary: "Fixed +5 V linear regulator, 1 A (TO-220).",
            keywords: &["regulator", "linear", "5v", "power", "supply"],
        },
        CatalogEntry {
            part_number: "AMS1117-3.3",
            category: "regulator",
            summary: "Fixed +3.3 V LDO regulator, 1 A.",
            keywords: &["regulator", "ldo", "3v3", "3.3v", "power", "supply"],
        },
    ];

    /// Search the catalog for entries matching `query`, best first. The query is
    /// split into whitespace terms; an entry scores one point per term that
    /// appears (case-insensitively, as a substring) in its part number, category,
    /// summary, or keywords. Entries scoring zero are dropped; ties break by part
    /// number so the result is deterministic.
    pub fn search(query: &str) -> Vec<&'static CatalogEntry> {
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_ascii_lowercase())
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, &'static CatalogEntry)> = CATALOG
            .iter()
            .filter_map(|e| {
                let score = terms.iter().filter(|t| entry_matches(e, t)).count();
                (score > 0).then_some((score, e))
            })
            .collect();
        // Highest score first, then part number ascending for a stable order.
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.part_number.cmp(b.1.part_number))
        });
        scored.into_iter().map(|(_, e)| e).collect()
    }

    /// Whether `term` appears (case-insensitively) anywhere searchable in `e`.
    fn entry_matches(e: &CatalogEntry, term: &str) -> bool {
        e.part_number.to_ascii_lowercase().contains(term)
            || e.category.contains(term)
            || e.summary.to_ascii_lowercase().contains(term)
            || e.keywords.iter().any(|k| k.contains(term))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee_core::graph::{Circuit, NetId, PartKind};
    use ee_core::units::Unit;
    use ee_core::value::Value;

    fn r(mag: f64) -> Option<Value> {
        Some(Value::new(mag, Unit::Ohm))
    }

    // ── DRC ───────────────────────────────────────────────────────────────────

    /// A healthy 10 V divider trips no rules.
    #[test]
    fn clean_divider_has_no_findings() {
        let mut c = Circuit::new("divider");
        let n_in = c.new_net();
        let n_out = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(10.0, Unit::Volt)),
            vec![n_in, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, r(1000.0), vec![n_in, n_out]);
        c.add_part(
            "R2",
            PartKind::Resistor,
            r(1000.0),
            vec![n_out, NetId::GROUND],
        );
        assert!(check(&c).is_empty(), "{:?}", check(&c));
        assert_eq!(review(&c), "No design-rule issues found.");
    }

    /// A dangling node (only one pin) is flagged as floating.
    #[test]
    fn floating_node_is_flagged() {
        let mut c = Circuit::new("floater");
        let n_in = c.new_net();
        let n_dangle = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![n_in, NetId::GROUND],
        );
        // R1's far end goes nowhere else.
        c.add_part("R1", PartKind::Resistor, r(1000.0), vec![n_in, n_dangle]);
        let fs = check(&c);
        assert!(
            fs.iter()
                .any(|f| f.rule == Rule::FloatingNode && f.refs == vec!["R1"]),
            "{fs:?}"
        );
    }

    /// An ungrounded island is flagged as having no path to ground.
    #[test]
    fn no_path_to_ground_is_flagged() {
        let mut c = Circuit::new("island");
        let a = c.new_net();
        let b = c.new_net();
        // Two resistors in a loop, never touching ground.
        c.add_part("R1", PartKind::Resistor, r(1000.0), vec![a, b]);
        c.add_part("R2", PartKind::Resistor, r(1000.0), vec![b, a]);
        let fs = check(&c);
        assert!(
            fs.iter().filter(|f| f.rule == Rule::NoPathToGround).count() >= 1,
            "{fs:?}"
        );
        assert!(fs
            .iter()
            .all(|f| f.severity == Severity::Error || f.rule != Rule::NoPathToGround));
    }

    /// An LED straight across the rail with no resistor is flagged; adding a
    /// series resistor clears it.
    #[test]
    fn led_without_series_resistor_is_flagged_then_cleared() {
        // V -> LED -> GND, no resistor.
        let mut bad = Circuit::new("bad led");
        let n = bad.new_net();
        bad.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![n, NetId::GROUND],
        );
        bad.add_part("D1", PartKind::Diode, None, vec![n, NetId::GROUND]);
        let fs = check(&bad);
        assert!(
            fs.iter()
                .any(|f| f.rule == Rule::LedNoSeriesResistor && f.refs == vec!["D1"]),
            "{fs:?}"
        );

        // V -> R -> mid -> LED -> GND clears the rule.
        let mut good = Circuit::new("good led");
        let n_in = good.new_net();
        let mid = good.new_net();
        good.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![n_in, NetId::GROUND],
        );
        good.add_part("R1", PartKind::Resistor, r(330.0), vec![n_in, mid]);
        good.add_part("D1", PartKind::Diode, None, vec![mid, NetId::GROUND]);
        assert!(
            !check(&good)
                .iter()
                .any(|f| f.rule == Rule::LedNoSeriesResistor),
            "{:?}",
            check(&good)
        );
    }

    /// A source with both terminals on one node is a short.
    #[test]
    fn short_across_source_is_flagged() {
        let mut c = Circuit::new("short");
        let n = c.new_net();
        c.wire(n, NetId::GROUND); // both source terminals collapse to ground
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![n, NetId::GROUND],
        );
        let fs = check(&c);
        assert!(
            fs.iter()
                .any(|f| f.rule == Rule::ShortAcrossSource && f.refs == vec!["V1"]),
            "{fs:?}"
        );
    }

    /// A 0 Ω resistor bridging a source's two nodes is a short across it.
    #[test]
    fn zero_ohm_resistor_shorts_the_source() {
        let mut c = Circuit::new("0R short");
        let n = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![n, NetId::GROUND],
        );
        c.add_part("R1", PartKind::Resistor, r(0.0), vec![n, NetId::GROUND]);
        let fs = check(&c);
        assert!(
            fs.iter().any(|f| f.rule == Rule::ShortAcrossSource
                && f.refs.contains(&"R1".to_string())
                && f.refs.contains(&"V1".to_string())),
            "{fs:?}"
        );
    }

    /// `check` is deterministic: the same circuit yields byte-identical findings.
    #[test]
    fn check_is_deterministic() {
        let mut c = Circuit::new("det");
        let a = c.new_net();
        let b = c.new_net();
        c.add_part("R1", PartKind::Resistor, r(1000.0), vec![a, b]);
        c.add_part("D1", PartKind::Diode, None, vec![a, b]);
        assert_eq!(check(&c), check(&c));
    }

    // ── Calculators ────────────────────────────────────────────────────────────

    #[test]
    fn divider_and_rc_rl_and_cutoff() {
        // 10 V across equal legs -> 5 V.
        assert!((calc::voltage_divider(10.0, 1000.0, 1000.0).unwrap() - 5.0).abs() < 1e-12);
        // 12 V, 1k over 2k -> 8 V.
        assert!((calc::voltage_divider(12.0, 1000.0, 2000.0).unwrap() - 8.0).abs() < 1e-12);
        // 1k * 1µF = 1 ms.
        assert!((calc::rc_time_constant(1000.0, 1e-6).unwrap() - 1e-3).abs() < 1e-15);
        // 1mH / 100Ω = 10 µs.
        assert!((calc::rl_time_constant(100.0, 1e-3).unwrap() - 1e-5).abs() < 1e-15);
        // 1/(2π·1ms) ≈ 159.154 Hz.
        assert!((calc::rc_cutoff_hz(1000.0, 1e-6).unwrap() - 159.154_943_091_895).abs() < 1e-6);
    }

    #[test]
    fn series_and_parallel_and_led_resistor() {
        assert_eq!(
            calc::series_resistance(&[100.0, 220.0, 330.0]).unwrap(),
            650.0
        );
        // Two 1k in parallel -> 500Ω.
        assert!((calc::parallel_resistance(&[1000.0, 1000.0]).unwrap() - 500.0).abs() < 1e-9);
        // (5 - 2) / 20mA = 150Ω.
        assert!((calc::led_series_resistor(5.0, 2.0, 0.02).unwrap() - 150.0).abs() < 1e-9);
    }

    #[test]
    fn calculators_reject_degenerate_inputs() {
        use calc::CalcError::*;
        assert_eq!(calc::voltage_divider(5.0, 0.0, 0.0), Err(DegenerateDivider));
        assert_eq!(
            calc::rc_time_constant(0.0, 1e-6),
            Err(NonPositive("the resistance"))
        );
        assert_eq!(
            calc::rl_time_constant(100.0, -1.0),
            Err(NonPositive("the inductance"))
        );
        assert_eq!(calc::series_resistance(&[]), Err(Empty));
        assert_eq!(
            calc::parallel_resistance(&[1000.0, 0.0]),
            Err(NonPositive("a resistance"))
        );
        assert_eq!(
            calc::led_series_resistor(2.0, 2.0, 0.02),
            Err(ForwardExceedsSupply)
        );
        assert_eq!(
            calc::led_series_resistor(5.0, 2.0, 0.0),
            Err(NonPositive("the target current"))
        );
        assert_eq!(calc::voltage_divider(f64::NAN, 1.0, 1.0), Err(NotFinite));
    }

    // ── Parts index ─────────────────────────────────────────────────────────────

    #[test]
    fn parts_search_scores_and_orders_deterministically() {
        // "schottky diode" ranks the Schottky entry first (two keyword hits).
        let hits = parts::search("schottky diode");
        assert_eq!(hits.first().unwrap().part_number, "1N5819", "{hits:?}");
        // All returned entries actually match, and the ordering repeats.
        assert!(!hits.is_empty());
        assert_eq!(parts::search("schottky diode"), hits);

        // A regulator query finds the regulators and nothing off-topic.
        let regs = parts::search("regulator");
        assert!(regs.iter().all(|e| e.category == "regulator"));
        assert!(regs.iter().any(|e| e.part_number == "LM7805"));

        // A nonsense query returns nothing (no false matches).
        assert!(parts::search("flux capacitor unobtanium").is_empty());
        assert!(parts::search("   ").is_empty());
    }

    /// The whole point of offline mode: these are reached with NO API key and NO
    /// transport/client. This test constructs findings, runs calculators, and
    /// searches the catalog without ever touching `crate::config`,
    /// `crate::transport`, or `crate::client` — the "no key needed" guarantee is
    /// that this module compiles and runs against `ee_core` alone.
    #[test]
    fn offline_needs_no_api_key_or_transport() {
        let mut c = Circuit::new("offline");
        let n = c.new_net();
        c.add_part(
            "V1",
            PartKind::VoltageSource,
            Some(Value::new(5.0, Unit::Volt)),
            vec![n, NetId::GROUND],
        );
        c.add_part("D1", PartKind::Diode, None, vec![n, NetId::GROUND]);

        // Rule checks fire, a calculator computes, the catalog answers — all with
        // no model in sight.
        assert!(!check(&c).is_empty());
        assert!(calc::led_series_resistor(5.0, 2.0, 0.02).is_ok());
        assert!(!parts::search("diode").is_empty());
    }
}
