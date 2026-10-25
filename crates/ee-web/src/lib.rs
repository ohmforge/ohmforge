//! # ee-web
//!
//! The WASM browser front end for `ee-assist` (`CP-WEB.1`): parse a netlist, run
//! the (already wasm-clean) `ee-sim` engine, and hand the result back to
//! JavaScript.
//!
//! ## Shape — pure core, thin wasm shell
//!
//! The whole "netlist → run → result" pipeline lives in plain Rust
//! ([`run`], [`Report`]) that compiles and is unit-tested on the **host**, so the
//! pipeline is proven in CI without a browser. The only browser-specific code is
//! the thin [`wasm`] wrapper, gated behind `cfg(target_arch = "wasm32")` and
//! compiled only for the `wasm32-unknown-unknown` target. On the host this crate
//! therefore pulls in nothing but the three wasm-clean compute crates
//! (`ee-core`, `ee-sim`, `ee-fmt`), which is what keeps the host workspace gates
//! green while `wasm-bindgen` stays out of the host dependency graph.
//!
//! See `crates/ee-web/README.md` for the build + serve steps.

mod braille;

use ee_sim::{Analysis, Engine};

/// A finished simulation of one netlist: the DC operating point, the transient
/// node traces, and a ready-to-display braille scope string.
#[derive(Debug, Clone)]
pub struct Report {
    /// Circuit title (`.title` directive, else `"netlist"`).
    pub title: String,
    /// DC operating-point node voltages, as `(node name, volts)`.
    pub op: Vec<(String, f64)>,
    /// Transient time axis, in seconds (shared by every trace in `nodes`).
    pub x: Vec<f64>,
    /// Transient node-voltage traces, `(node name, samples)`.
    pub nodes: Vec<(String, Vec<f64>)>,
    /// A braille scope of the transient traces, ready to drop into a `<pre>`.
    pub scope: String,
}

/// Parse `netlist`, run an op-point **and** a transient, and bundle the result.
///
/// `duration` is the transient length in seconds; `step` is the fixed timestep
/// (`None` lets the engine choose an adaptive step). Any parse or solver failure
/// comes back as a human-readable `String` (the wasm layer turns it into a JS
/// exception; the host test asserts on it).
pub fn run(netlist: &str, duration: f64, step: Option<f64>) -> Result<Report, String> {
    let circuit = ee_fmt::netlist::parse(netlist).map_err(|e| e.to_string())?;
    let engine = Engine::new();

    let op_sigs = engine
        .run(&circuit, &Analysis::OperatingPoint)
        .map_err(|e| e.to_string())?;
    let op = op_sigs
        .iter()
        .map(|s| {
            let v = s
                .real()
                .and_then(|y| y.first().copied())
                .unwrap_or(f64::NAN);
            (s.name.clone(), v)
        })
        .collect();

    let tr_sigs = engine
        .run(&circuit, &Analysis::Transient { duration, step })
        .map_err(|e| e.to_string())?;
    let x = tr_sigs.first().map(|s| s.x.clone()).unwrap_or_default();
    let nodes: Vec<(String, Vec<f64>)> = tr_sigs
        .iter()
        .map(|s| {
            (
                s.name.clone(),
                s.real().map(|y| y.to_vec()).unwrap_or_default(),
            )
        })
        .collect();

    let scope = render_scope(&circuit.name, &x, &nodes);

    Ok(Report {
        title: circuit.name,
        op,
        x,
        nodes,
        scope,
    })
}

/// Render one braille frame per transient trace, stacked, for a `<pre>`.
fn render_scope(title: &str, x: &[f64], nodes: &[(String, Vec<f64>)]) -> String {
    if nodes.is_empty() || x.len() < 2 {
        return format!("  {title}\n  (no transient samples)\n");
    }
    // A readable time axis: seconds unless the run is sub-second, then ms / µs.
    let span = x.last().copied().unwrap_or(0.0);
    let (x_unit, x_scale) = if span >= 1.0 {
        ("s", 1.0)
    } else if span >= 1e-3 {
        ("ms", 1e3)
    } else {
        ("us", 1e6)
    };
    let xs: Vec<f64> = x.iter().map(|&t| t * x_scale).collect();

    let mut out = String::new();
    for (name, y) in nodes {
        out.push_str(&braille::render(name, &xs, y, 64, 12, "V", x_unit));
        out.push('\n');
    }
    out
}

impl Report {
    /// Serialize to a compact JSON object: `{title, x, nodes:[{name,y}], op:[{name,v}], scope}`.
    ///
    /// Hand-rolled (no `serde`) to keep the wasm bundle small. Non-finite samples
    /// (an inf/NaN from a diverged solve) serialize as JSON `null`, since JSON has
    /// no `NaN`/`Infinity` literal.
    pub fn to_json(&self) -> String {
        let mut s = String::from("{\"title\":");
        push_json_str(&mut s, &self.title);

        s.push_str(",\"x\":");
        push_json_nums(&mut s, &self.x);

        s.push_str(",\"nodes\":[");
        for (i, (name, y)) in self.nodes.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str("{\"name\":");
            push_json_str(&mut s, name);
            s.push_str(",\"y\":");
            push_json_nums(&mut s, y);
            s.push('}');
        }
        s.push_str("],\"op\":[");
        for (i, (name, v)) in self.op.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str("{\"name\":");
            push_json_str(&mut s, name);
            s.push_str(",\"v\":");
            push_json_num(&mut s, *v);
            s.push('}');
        }
        s.push_str("],\"scope\":");
        push_json_str(&mut s, &self.scope);
        s.push('}');
        s
    }
}

fn push_json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_json_num(out: &mut String, v: f64) {
    if v.is_finite() {
        out.push_str(&format!("{v}"));
    } else {
        out.push_str("null");
    }
}

fn push_json_nums(out: &mut String, xs: &[f64]) {
    out.push('[');
    for (i, &v) in xs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_json_num(out, v);
    }
    out.push(']');
}

/// The browser-facing API — a thin `#[wasm_bindgen]` shell over [`run`]. Compiled
/// only for `wasm32`, so `wasm-bindgen` never enters the host build.
#[cfg(target_arch = "wasm32")]
mod wasm {
    use wasm_bindgen::prelude::*;

    /// Parse a netlist, run op-point + transient, and return the result as a JSON
    /// string: `{title, x, nodes, op, scope}`. A non-positive `step` means "let
    /// the engine pick an adaptive timestep". Parse/solver errors throw.
    #[wasm_bindgen]
    pub fn simulate(netlist: &str, duration: f64, step: f64) -> Result<String, JsValue> {
        let step = (step > 0.0).then_some(step);
        super::run(netlist, duration, step)
            .map(|r| r.to_json())
            .map_err(|e| JsValue::from_str(&e))
    }

    /// Parse a netlist, run op-point + transient, and return just the braille
    /// scope string (handy for a plain `<pre>` demo). Errors throw.
    #[wasm_bindgen]
    pub fn scope(netlist: &str, duration: f64, step: f64) -> Result<String, JsValue> {
        let step = (step > 0.0).then_some(step);
        super::run(netlist, duration, step)
            .map(|r| r.scope)
            .map_err(|e| JsValue::from_str(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RC: &str = "\
# RC low-pass: out charges toward the 5 V rail
.title rc-lowpass
V1 in  gnd 5
R1 in  out 1k
C1 out gnd 1u
";

    #[test]
    fn pipeline_parses_runs_and_charges() {
        let r = run(RC, 5e-3, Some(5e-6)).expect("RC netlist simulates");
        assert_eq!(r.title, "rc-lowpass");
        assert_eq!(r.nodes.len(), 2, "two non-ground nodes: in and out");

        // Identify `out` by behaviour (node names carry raw net ids we don't
        // hard-code): it charges from ~0 V toward the 5 V rail.
        let out = r
            .nodes
            .iter()
            .find(|(_, y)| y.first().is_some_and(|v| v.abs() < 1e-9))
            .expect("a node that starts uncharged (the RC output)");
        assert!(
            *out.1.last().unwrap() > 4.9,
            "out ends near the 5 V rail: {}",
            out.1.last().unwrap()
        );

        // At the op-point the cap is open, so no current flows and every node
        // (in and out) sits at the 5 V rail.
        for (name, v) in &r.op {
            assert!(
                (v - 5.0).abs() < 1e-6,
                "op-point {name} = {v}, expected 5 V"
            );
        }
    }

    #[test]
    fn scope_string_has_braille() {
        let r = run(RC, 5e-3, Some(5e-6)).unwrap();
        assert!(r.scope.contains("rc-lowpass") || r.scope.contains("v(n"));
        assert!(
            r.scope
                .chars()
                .any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)),
            "scope should contain drawn braille dots"
        );
    }

    #[test]
    fn json_is_wellformed_and_contains_data() {
        let r = run(RC, 1e-3, Some(1e-4)).unwrap();
        let json = r.to_json();
        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(json.contains("\"title\":\"rc-lowpass\""));
        assert!(json.contains("\"nodes\":["));
        assert!(json.contains("\"op\":["));
        assert!(json.contains("\"scope\":\""));
        // No raw NaN/Infinity tokens leaked into the JSON.
        assert!(!json.contains("NaN") && !json.contains("inf"));
    }

    #[test]
    fn adaptive_step_runs_when_step_is_none() {
        let r = run(RC, 5e-3, None).expect("adaptive transient runs");
        assert!(r.x.len() >= 2, "adaptive run produced a time axis");
    }

    #[test]
    fn parse_error_is_reported() {
        let err = run("R1 a b", 1e-3, None).unwrap_err();
        assert!(
            err.contains("line 1"),
            "error carries the source line: {err}"
        );
    }

    #[test]
    fn solver_error_is_reported() {
        // A zero-ohm resistor is a bad value the solver rejects.
        let err = run("V1 in gnd 5\nR1 in gnd 0", 1e-3, None).unwrap_err();
        assert!(!err.is_empty(), "solver error surfaced as a message");
    }
}
