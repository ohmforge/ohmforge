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
//! the thin `wasm` wrapper, gated behind `cfg(target_arch = "wasm32")` and
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

// --- Shareable circuit links (CP-WEB.2) ------------------------------------
//
// A circuit *is* its netlist text, so a shareable link is just that text packed
// into a URL fragment. We pack it with hand-rolled **base64url** (RFC 4648 §5):
// the alphabet is `A–Z a–z 0–9 - _`, i.e. the standard base64 set with `+`→`-`
// and `/`→`_`, and we emit **no `=` padding**. That matters because a URL
// fragment is copy-pasted whole: `+`, `/` and `=` are all reserved or
// ambiguous in a fragment (`+` can read as a space, `/` splits paths, `=` reads as a
// key/value separator), whereas `-` and `_` are unreserved and padding-free
// output never introduces an `=`. So `encode` output drops straight into
// `location.hash` and survives a round-trip through copy-paste and the browser.
//
// This lives in plain host Rust (no dep, ~40 lines) so it is unit-tested in CI
// without a browser; the wasm layer is a thin wrapper, exactly like `simulate`.

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Encode a netlist into a URL-fragment-safe string: base64url of its UTF-8
/// bytes, no padding. Round-trips exactly through [`decode`].
pub fn encode(netlist: &str) -> String {
    let bytes = netlist.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL[(n >> 18 & 63) as usize] as char);
        out.push(B64URL[(n >> 12 & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64URL[(n >> 6 & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL[(n & 63) as usize] as char);
        }
    }
    out
}

/// Decode a fragment produced by [`encode`] back into the netlist text.
///
/// Tolerant of a single leading `#` (so you can hand it `location.hash` as-is)
/// and of any trailing `=` padding (in case a link was produced elsewhere), but
/// otherwise strict: any character outside the base64url alphabet, or a byte
/// sequence that is not valid UTF-8, is a clean `Err` — never a panic.
pub fn decode(fragment: &str) -> Result<String, String> {
    let s = fragment.strip_prefix('#').unwrap_or(fragment);
    let mut bytes = Vec::with_capacity(s.len() / 4 * 3 + 2);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for ch in s.chars() {
        if ch == '=' {
            continue; // tolerate (ignore) any padding
        }
        let val = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            'a'..='z' => ch as u32 - 'a' as u32 + 26,
            '0'..='9' => ch as u32 - '0' as u32 + 52,
            '-' => 62,
            '_' => 63,
            _ => return Err(format!("invalid base64url character: {ch:?}")),
        };
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buf >> bits) as u8);
        }
    }
    String::from_utf8(bytes).map_err(|e| format!("decoded bytes are not valid UTF-8: {e}"))
}

// --- Structured circuit edit model (CP-WEB.3) ------------------------------
//
// The page lets you *build* a circuit, not just paste one: a parts form adds a
// part (by ref / nodes / value), lists the parts, and removes one. The real
// logic is this pure, host-tested model — a title plus an ordered list of
// [`Part`]s — that round-trips to netlist text. `from_netlist` lifts the
// textarea into the model (preserving the raw node names and engineering-
// notation value strings that `ee_fmt`'s `Circuit` discards once it resolves
// them to `NetId`s), and `to_netlist` emits a clean, canonical netlist that
// `run` parses and simulates. The `#[wasm_bindgen]` layer stays thin: it takes
// netlist text in and hands netlist text (or a JSON part list) back out, so the
// page keeps the textarea as the single source of truth and just transforms it.

/// One editable part line: `REF  NODE_A  NODE_B  VALUE`.
///
/// The value is kept as its raw string (e.g. `"1k"`, `"100n"`) so a round trip
/// through the model never rewrites the engineering notation the user typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// Reference designator; its first letter fixes the kind (`R`/`C`/`L`/`V`).
    pub reference: String,
    /// First terminal's node name.
    pub node_a: String,
    /// Second terminal's node name.
    pub node_b: String,
    /// Raw value field, verbatim (`"1k"`, `"100n"`, `"5"`).
    pub value: String,
}

/// A circuit as the editor sees it: an optional title and an ordered part list.
/// Round-trips to netlist text via [`CircuitModel::from_netlist`] /
/// [`CircuitModel::to_netlist`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CircuitModel {
    /// The `.title` directive's text, if any.
    pub title: Option<String>,
    /// The parts, in the order they should be emitted.
    pub parts: Vec<Part>,
}

/// Map a reference's leading letter to a human-readable kind, rejecting anything
/// the netlist grammar (and thus the engine) does not accept.
fn kind_label(reference: &str) -> Result<&'static str, String> {
    let c = reference
        .chars()
        .next()
        .ok_or_else(|| "empty part reference".to_string())?;
    Ok(match c.to_ascii_uppercase() {
        'R' => "resistor",
        'C' => "capacitor",
        'L' => "inductor",
        'V' => "voltage source",
        other => {
            return Err(format!(
                "unknown part type '{other}' (expected R, C, L, or V)"
            ))
        }
    })
}

/// Everything on a line before the first `#` or `//` comment marker.
fn strip_netlist_comment(line: &str) -> &str {
    let hash = line.find('#').unwrap_or(line.len());
    let slashes = line.find("//").unwrap_or(line.len());
    &line[..hash.min(slashes)]
}

impl CircuitModel {
    /// Lift netlist text into the edit model. Comments and blank lines are
    /// dropped, `.title` is captured, and each `REF A B VALUE` line becomes a
    /// [`Part`] with its node names and value string preserved verbatim. A
    /// malformed line (wrong field count, unknown part type, unknown directive)
    /// is a clean `Err` carrying the 1-based source line — never a panic.
    pub fn from_netlist(src: &str) -> Result<CircuitModel, String> {
        let mut model = CircuitModel::default();
        for (i, raw) in src.lines().enumerate() {
            let line = i + 1;
            let text = strip_netlist_comment(raw).trim();
            if text.is_empty() {
                continue;
            }
            if let Some(rest) = text.strip_prefix('.') {
                let mut it = rest.splitn(2, char::is_whitespace);
                let name = it.next().unwrap_or("");
                let arg = it.next().unwrap_or("").trim();
                if name.eq_ignore_ascii_case("title") {
                    model.title = Some(arg.to_string());
                } else {
                    return Err(format!("line {line}: unknown directive .{name}"));
                }
                continue;
            }
            let fields: Vec<&str> = text.split_whitespace().collect();
            if fields.len() != 4 {
                return Err(format!(
                    "line {line}: expected 4 fields (REF A B VALUE), found {}",
                    fields.len()
                ));
            }
            kind_label(fields[0]).map_err(|e| format!("line {line}: {e}"))?;
            model.parts.push(Part {
                reference: fields[0].to_string(),
                node_a: fields[1].to_string(),
                node_b: fields[2].to_string(),
                value: fields[3].to_string(),
            });
        }
        Ok(model)
    }

    /// Emit a clean, canonical netlist: a `.title` line (if set) followed by one
    /// `REF A B VALUE` line per part. The result parses and simulates via [`run`].
    pub fn to_netlist(&self) -> String {
        let mut out = String::new();
        if let Some(title) = &self.title {
            out.push_str(".title ");
            out.push_str(title);
            out.push('\n');
        }
        for p in &self.parts {
            out.push_str(&p.reference);
            out.push(' ');
            out.push_str(&p.node_a);
            out.push(' ');
            out.push_str(&p.node_b);
            out.push(' ');
            out.push_str(&p.value);
            out.push('\n');
        }
        out
    }

    /// Add a part. Validates that the reference is a known kind, that nothing is
    /// empty or carries embedded whitespace (the netlist is whitespace-delimited),
    /// and that the reference is not already used. Returns a message on any of
    /// those, leaving the model unchanged.
    pub fn add_part(
        &mut self,
        reference: &str,
        node_a: &str,
        node_b: &str,
        value: &str,
    ) -> Result<(), String> {
        let reference = reference.trim();
        let node_a = node_a.trim();
        let node_b = node_b.trim();
        let value = value.trim();
        kind_label(reference)?;
        for (what, tok) in [
            ("reference", reference),
            ("first node", node_a),
            ("second node", node_b),
            ("value", value),
        ] {
            if tok.is_empty() {
                return Err(format!("{what} must not be empty"));
            }
            if tok.split_whitespace().count() != 1 {
                return Err(format!("{what} {tok:?} must not contain whitespace"));
            }
        }
        if self.parts.iter().any(|p| p.reference == reference) {
            return Err(format!("duplicate part reference {reference:?}"));
        }
        self.parts.push(Part {
            reference: reference.to_string(),
            node_a: node_a.to_string(),
            node_b: node_b.to_string(),
            value: value.to_string(),
        });
        Ok(())
    }

    /// Remove every part with this reference. Returns whether anything was removed.
    pub fn remove_part(&mut self, reference: &str) -> bool {
        let before = self.parts.len();
        self.parts.retain(|p| p.reference != reference);
        self.parts.len() != before
    }

    /// Serialize the model to JSON for the parts form:
    /// `{title, parts:[{reference,kind,node_a,node_b,value}]}`. Hand-rolled (no
    /// `serde`) to match the rest of this crate and keep the wasm bundle small.
    pub fn to_json(&self) -> String {
        let mut s = String::from("{\"title\":");
        match &self.title {
            Some(t) => push_json_str(&mut s, t),
            None => s.push_str("null"),
        }
        s.push_str(",\"parts\":[");
        for (i, p) in self.parts.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str("{\"reference\":");
            push_json_str(&mut s, &p.reference);
            s.push_str(",\"kind\":");
            push_json_str(&mut s, kind_label(&p.reference).unwrap_or("?"));
            s.push_str(",\"node_a\":");
            push_json_str(&mut s, &p.node_a);
            s.push_str(",\"node_b\":");
            push_json_str(&mut s, &p.node_b);
            s.push_str(",\"value\":");
            push_json_str(&mut s, &p.value);
            s.push('}');
        }
        s.push_str("]}");
        s
    }
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

    /// Encode a netlist into a URL-fragment-safe share token (base64url, no
    /// padding). Drop the result into `location.hash` to make a shareable link.
    #[wasm_bindgen]
    pub fn encode_share(netlist: &str) -> String {
        super::encode(netlist)
    }

    /// Decode a share token (the value from `location.hash`, with or without its
    /// leading `#`) back into netlist text. Returns `""` when the fragment is
    /// empty or not a valid token, so the page can simply ignore a bad hash.
    #[wasm_bindgen]
    pub fn decode_share(fragment: &str) -> String {
        super::decode(fragment).unwrap_or_default()
    }

    // --- Structured editing (CP-WEB.3) -------------------------------------

    /// List the parts of a netlist as JSON for the editor form:
    /// `{title, parts:[{reference,kind,node_a,node_b,value}]}`. A malformed
    /// netlist throws its error message.
    #[wasm_bindgen]
    pub fn list_parts(netlist: &str) -> Result<String, JsValue> {
        super::CircuitModel::from_netlist(netlist)
            .map(|m| m.to_json())
            .map_err(|e| JsValue::from_str(&e))
    }

    /// Add a part to a netlist and return the updated netlist text. Throws on a
    /// malformed netlist or an invalid/duplicate part.
    #[wasm_bindgen]
    pub fn edit_add(
        netlist: &str,
        reference: &str,
        node_a: &str,
        node_b: &str,
        value: &str,
    ) -> Result<String, JsValue> {
        let mut model =
            super::CircuitModel::from_netlist(netlist).map_err(|e| JsValue::from_str(&e))?;
        model
            .add_part(reference, node_a, node_b, value)
            .map_err(|e| JsValue::from_str(&e))?;
        Ok(model.to_netlist())
    }

    /// Remove the part with this reference and return the updated netlist text.
    /// A no-op if the reference is absent. Throws only on a malformed netlist.
    #[wasm_bindgen]
    pub fn edit_remove(netlist: &str, reference: &str) -> Result<String, JsValue> {
        let mut model =
            super::CircuitModel::from_netlist(netlist).map_err(|e| JsValue::from_str(&e))?;
        model.remove_part(reference);
        Ok(model.to_netlist())
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

    // --- share-link encode/decode -----------------------------------------

    #[test]
    fn encode_decode_round_trips_exactly() {
        // Comments, blank lines, trailing newline, CRLF, tabs, and unicode all
        // survive a byte-exact round trip.
        let cases = [
            RC,
            "",
            "V1 in gnd 5\n",
            "# comment only\n",
            "line1\r\nline2\twith\ttabs\r\n",
            ".title café-π\n# Ω resistor, 5µF cap — 漢字\nV1 in gnd 5\n",
            "no trailing newline",
            "a",
            "ab",
            "abc",
            "abcd",
        ];
        for netlist in cases {
            let enc = encode(netlist);
            let dec = decode(&enc).expect("valid token decodes");
            assert_eq!(dec, netlist, "round trip must be exact for {netlist:?}");
        }
    }

    #[test]
    fn encoding_is_url_fragment_safe() {
        // Over a byte range that exercises every 6-bit group, the output must
        // never contain a character that breaks a URL fragment.
        let raw: String = (0u8..=255).map(|b| b as char).collect();
        let enc = encode(&raw);
        assert!(
            enc.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "token must be pure base64url: {enc}"
        );
        assert!(
            !enc.contains('+') && !enc.contains('/') && !enc.contains('='),
            "token must not contain +, / or = (all unsafe/ambiguous in a fragment)"
        );
    }

    #[test]
    fn decode_tolerates_leading_hash_and_padding() {
        let enc = encode(RC);
        assert_eq!(decode(&format!("#{enc}")).unwrap(), RC, "leading # ignored");
        // base64url of "foob" is "Zm9vYg"; standard base64 pads it to "Zm9vYg==".
        assert_eq!(decode("Zm9vYg==").unwrap(), "foob", "= padding tolerated");
    }

    #[test]
    fn decode_of_garbage_is_a_clean_error() {
        for junk in ["!!!!", "has spaces", "plus+sign", "slash/here", "@#$%"] {
            let err = decode(junk);
            assert!(
                err.is_err(),
                "garbage {junk:?} must be a clean Err, got {err:?}"
            );
        }
    }

    #[test]
    fn decoded_link_runs_through_the_pipeline() {
        // The whole point: a shared link decodes back to a netlist that runs.
        let token = encode(RC);
        let netlist = decode(&token).unwrap();
        let r = run(&netlist, 5e-3, Some(5e-6)).expect("decoded netlist simulates");
        assert_eq!(r.title, "rc-lowpass");
        assert_eq!(r.nodes.len(), 2);
    }

    // --- edit model: from/to netlist round-trip (CP-WEB.3) -----------------

    #[test]
    fn model_lifts_title_parts_and_values() {
        let m = CircuitModel::from_netlist(RC).expect("RC lifts into the model");
        assert_eq!(m.title.as_deref(), Some("rc-lowpass"));
        assert_eq!(m.parts.len(), 3);
        // Node names and the raw engineering-notation value strings survive.
        let r1 = m.parts.iter().find(|p| p.reference == "R1").unwrap();
        assert_eq!((r1.node_a.as_str(), r1.node_b.as_str()), ("in", "out"));
        assert_eq!(r1.value, "1k", "raw value notation is preserved verbatim");
        let c1 = m.parts.iter().find(|p| p.reference == "C1").unwrap();
        assert_eq!(c1.value, "1u");
    }

    #[test]
    fn model_round_trips_through_netlist_and_still_simulates() {
        // Lift → emit → the emitted netlist parses and simulates exactly like RC.
        let m = CircuitModel::from_netlist(RC).unwrap();
        let netlist = m.to_netlist();
        // Re-lifting the emitted text yields the identical model (semantic round trip).
        assert_eq!(CircuitModel::from_netlist(&netlist).unwrap(), m);
        let r = run(&netlist, 5e-3, Some(5e-6)).expect("emitted netlist simulates");
        assert_eq!(r.title, "rc-lowpass");
        assert_eq!(r.nodes.len(), 2);
    }

    #[test]
    fn add_part_changes_the_netlist_and_still_simulates() {
        // The gate: adding a part changes the netlist as expected, and the
        // result still parses + simulates via `run`.
        let mut m = CircuitModel::from_netlist(RC).unwrap();
        assert!(!m.to_netlist().contains("R2"));
        m.add_part("R2", "out", "gnd", "2k")
            .expect("a well-formed resistor is accepted");
        let netlist = m.to_netlist();
        assert!(
            netlist.contains("R2 out gnd 2k"),
            "the new part appears in the netlist: {netlist}"
        );
        let r = run(&netlist, 5e-3, Some(5e-6)).expect("edited netlist simulates");
        // R2 bleeds `out` to ground, so it no longer sits at the full 5 V rail.
        assert_eq!(r.title, "rc-lowpass");
        assert_eq!(r.nodes.len(), 2);
    }

    #[test]
    fn remove_part_drops_it_from_the_netlist_and_still_simulates() {
        let mut m = CircuitModel::from_netlist(RC).unwrap();
        assert!(m.remove_part("C1"), "C1 was present and removed");
        assert!(!m.remove_part("C1"), "removing again is a no-op");
        let netlist = m.to_netlist();
        assert!(!netlist.contains("C1"), "C1 is gone: {netlist}");
        assert!(netlist.contains("R1 in out 1k"), "other parts remain");
        // A bare V–R divider still parses and simulates.
        let r = run(&netlist, 5e-3, Some(5e-6)).expect("reduced netlist simulates");
        assert_eq!(r.title, "rc-lowpass");
    }

    #[test]
    fn add_part_rejects_bad_input() {
        let mut m = CircuitModel::from_netlist(RC).unwrap();
        assert!(m.add_part("R1", "a", "b", "1k").is_err(), "duplicate ref");
        assert!(m.add_part("X9", "a", "b", "1k").is_err(), "unknown kind");
        assert!(m.add_part("R9", "", "b", "1k").is_err(), "empty node");
        assert!(
            m.add_part("R9", "a b", "c", "1k").is_err(),
            "embedded whitespace"
        );
        assert_eq!(m.parts.len(), 3, "no bad part leaked into the model");
    }

    #[test]
    fn model_rejects_malformed_netlist() {
        assert!(
            CircuitModel::from_netlist("R1 a b").is_err(),
            "too few fields"
        );
        assert!(
            CircuitModel::from_netlist("X1 a b 1k").is_err(),
            "unknown part type"
        );
        assert!(
            CircuitModel::from_netlist(".nope foo").is_err(),
            "unknown directive"
        );
    }

    #[test]
    fn model_to_json_is_wellformed() {
        let json = CircuitModel::from_netlist(RC).unwrap().to_json();
        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(json.contains("\"title\":\"rc-lowpass\""));
        assert!(json.contains("\"reference\":\"R1\""));
        assert!(json.contains("\"kind\":\"resistor\""));
        assert!(json.contains("\"kind\":\"capacitor\""));
        assert!(json.contains("\"value\":\"1k\""));
    }
}
