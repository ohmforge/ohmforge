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

// --- Full-session shareable links (CP-WEB.4) -------------------------------
//
// The M4 share link (`encode`/`decode`) packs just the netlist. A full session
// also captures the *workbench*: the transient `duration` and `step`, plus the
// `probes` (node names the user is scoping). We serialize the session into a
// small, line-oriented text blob and run it through the very same base64url
// (`encode`/`decode`) so the token still drops straight into `location.hash`.
//
// The blob is a fixed header followed by the netlist *verbatim*:
//
// ```text
// EEWS1\n                       magic + version
// <duration f64>\n              Rust's shortest round-trippable f64 form
// <step f64 | "auto">\n
// <probe count>\n
// <probe>\n   × count           node names (whitespace-free, so one per line)
// <netlist…>                    everything after the last header line, byte-exact
// ```
//
// Putting the netlist last and copying the remaining bytes wholesale is what
// makes the round trip *exact*: its comments, blank lines, CRLFs and trailing
// newline all survive, because nothing re-parses or re-emits them.
//
// A shared link is a point-in-time snapshot (async collaboration). Live
// multi-cursor co-editing — where two browsers *converge* on one circuit — is a
// separate concern handled by the op-based merge layer below (CP-WEB.5); only
// the hosted signaling relay for cross-machine sessions stays deferred.

/// A full workbench snapshot: the circuit (as netlist text) plus the view
/// settings the page holds (transient `duration`/`step` and the scoped `probes`).
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// The circuit, as netlist text (preserved byte-exact across a round trip).
    pub netlist: String,
    /// Transient duration, in seconds.
    pub duration: f64,
    /// Fixed transient timestep in seconds, or `None` for an adaptive step.
    pub step: Option<f64>,
    /// Node names the user is scoping (empty means "show everything").
    pub probes: Vec<String>,
}

/// Encode a [`Session`] into a URL-fragment-safe token (base64url, no padding),
/// round-tripping exactly through [`decode_session`].
pub fn encode_session(session: &Session) -> String {
    let mut buf = String::new();
    buf.push_str("EEWS1\n");
    buf.push_str(&format!("{}\n", session.duration));
    match session.step {
        Some(v) => buf.push_str(&format!("{v}\n")),
        None => buf.push_str("auto\n"),
    }
    buf.push_str(&format!("{}\n", session.probes.len()));
    for p in &session.probes {
        buf.push_str(p);
        buf.push('\n');
    }
    buf.push_str(&session.netlist);
    encode(&buf)
}

/// Decode a token produced by [`encode_session`] back into the full [`Session`].
///
/// Tolerant of a leading `#` (hand it `location.hash` as-is) via [`decode`], but
/// otherwise strict: a bad base64url token, a missing/incorrect header, or a
/// malformed field is a clean `Err`, never a panic.
pub fn decode_session(token: &str) -> Result<Session, String> {
    let raw = decode(token)?;
    let rest = raw
        .strip_prefix("EEWS1\n")
        .ok_or("not a session token (bad magic/version)")?;

    let (duration_s, rest) = take_line(rest)?;
    let duration = duration_s
        .parse::<f64>()
        .map_err(|_| format!("bad session duration: {duration_s:?}"))?;

    let (step_s, rest) = take_line(rest)?;
    let step = if step_s == "auto" {
        None
    } else {
        Some(
            step_s
                .parse::<f64>()
                .map_err(|_| format!("bad session step: {step_s:?}"))?,
        )
    };

    let (count_s, mut rest) = take_line(rest)?;
    let count = count_s
        .parse::<usize>()
        .map_err(|_| format!("bad session probe count: {count_s:?}"))?;
    let mut probes = Vec::with_capacity(count);
    for _ in 0..count {
        let (p, r) = take_line(rest)?;
        probes.push(p.to_string());
        rest = r;
    }

    // Everything left is the netlist, byte-exact.
    Ok(Session {
        netlist: rest.to_string(),
        duration,
        step,
        probes,
    })
}

/// Split off one `\n`-terminated header line, returning `(line, remainder)`.
/// A missing newline means the blob was truncated — a clean `Err`.
fn take_line(s: &str) -> Result<(&str, &str), String> {
    match s.find('\n') {
        Some(i) => Ok((&s[..i], &s[i + 1..])),
        None => Err("truncated session data".to_string()),
    }
}

impl Session {
    /// Serialize to JSON for the page: `{netlist, duration, step, probes:[…]}`.
    /// `step` is `null` for an adaptive step. Hand-rolled, like [`Report::to_json`].
    pub fn to_json(&self) -> String {
        let mut s = String::from("{\"netlist\":");
        push_json_str(&mut s, &self.netlist);
        s.push_str(",\"duration\":");
        push_json_num(&mut s, self.duration);
        s.push_str(",\"step\":");
        match self.step {
            Some(v) => push_json_num(&mut s, v),
            None => s.push_str("null"),
        }
        s.push_str(",\"probes\":[");
        for (i, p) in self.probes.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            push_json_str(&mut s, p);
        }
        s.push_str("]}");
        s
    }
}

// --- Real-time co-edit merge layer (CP-WEB.5) ------------------------------
//
// CP-WEB.4 left real-time co-editing deferred because a shared *snapshot* link
// can't converge two people typing at once. This is the merge layer that lets
// them: an **op-based** scheme where every edit is a small, clock-tagged
// [`EditOp`], replicas exchange ops over any transport in any order, and each
// replica deterministically derives the same [`CircuitModel`] — hence the same
// netlist — from the same *set* of ops.
//
// ## Why it converges (the deterministic rule)
//
// Each op carries a [`Clock`] = (Lamport counter, site id). A [`Replica`] keeps
// its ops in a log deduplicated by clock; the model is **materialized** by
// replaying that log in `(lamport, site)` order. Two things make this provably
// convergent without a CRDT library:
//
//  * **Order independence.** Final state is a pure function of the *set* of ops
//    (they get sorted into one canonical order before replay), so ops may arrive
//    in any order, be redelivered, or be merged in batches — the result is the
//    same.
//  * **Last-writer-wins.** When two ops touch the same part/field, replay visits
//    them in clock order, so the higher `(lamport, site)` wins. The site id
//    breaks ties between truly concurrent edits at the *same* Lamport time, so
//    the winner is identical on every replica regardless of delivery order.
//
// Parts are keyed by reference (a `BTreeMap`), so the emitted netlist is ordered
// deterministically too. The whole layer is plain host Rust (one `BTreeMap`, no
// new deps) and is unit-tested in CI without a browser; the `wasm` module adds
// only a thin [`BroadcastChannel`] outbound binding (see `CoEdit`).

use std::collections::BTreeMap;

/// A logical clock: a Lamport counter plus the id of the site that stamped it.
///
/// The derived `Ord` compares `lamport` first, then `site`, giving a **total**
/// order over ops from every replica — the backbone of the last-writer-wins
/// rule. The `site` field is what makes two concurrent edits (same `lamport`)
/// resolve to the same winner everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Clock {
    /// Lamport timestamp: bumped on every local edit, pulled forward to
    /// `max(local, incoming)` on merge so causally-later ops sort later.
    pub lamport: u64,
    /// Originating site (replica) id; the deterministic tiebreak for a tie on
    /// `lamport`.
    pub site: u64,
}

/// What one edit does, with the target part identified by its `reference`.
///
/// This is the vocabulary the spec calls for — add / remove / update a part,
/// set a value, re-wire — plus a title set, since the netlist carries a title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpKind {
    /// Add (or overwrite) a whole part.
    AddPart {
        /// Reference designator (`R1`, `C2`, …); the part's identity.
        reference: String,
        /// First terminal node name.
        node_a: String,
        /// Second terminal node name.
        node_b: String,
        /// Raw value field (`"1k"`, `"100n"`).
        value: String,
    },
    /// Delete the part with this reference.
    RemovePart {
        /// Reference designator to remove.
        reference: String,
    },
    /// Change just the value of an existing part.
    SetValue {
        /// Reference designator to retune.
        reference: String,
        /// New raw value field.
        value: String,
    },
    /// Re-wire an existing part's two terminals.
    Wire {
        /// Reference designator to re-wire.
        reference: String,
        /// New first terminal node name.
        node_a: String,
        /// New second terminal node name.
        node_b: String,
    },
    /// Set (or clear, with `None`) the circuit title.
    SetTitle {
        /// The new `.title` text, or `None` to drop it.
        title: Option<String>,
    },
}

/// A clock-tagged edit — the unit two replicas exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditOp {
    /// When/where this edit was stamped; the sort key for replay.
    pub clock: Clock,
    /// The edit itself.
    pub kind: OpKind,
}

/// An op-based replica of a circuit: a site id, a Lamport counter, and the
/// merged op log. Local edits ([`add_part`](Replica::add_part), …) return the
/// [`EditOp`] to broadcast; incoming ops go through [`merge`](Replica::merge).
/// Any two replicas fed the **same set of ops** (in any order, with duplicates)
/// [`materialize`](Replica::materialize) to an identical [`CircuitModel`] and
/// hence an identical [`to_netlist`](Replica::to_netlist).
#[derive(Debug, Clone)]
pub struct Replica {
    site: u64,
    lamport: u64,
    /// Ops sorted by `clock`, deduplicated by `clock` (idempotent redelivery).
    log: Vec<EditOp>,
}

/// Reject an empty or whitespace-bearing token (the netlist is whitespace-
/// delimited), mirroring [`CircuitModel::add_part`]'s validation.
fn check_token(what: &str, tok: &str) -> Result<(), String> {
    if tok.is_empty() {
        return Err(format!("{what} must not be empty"));
    }
    if tok.split_whitespace().count() != 1 {
        return Err(format!("{what} {tok:?} must not contain whitespace"));
    }
    Ok(())
}

impl Replica {
    /// A fresh replica for `site`. Distinct replicas MUST use distinct site ids
    /// (that is what makes the last-writer-wins tiebreak deterministic).
    pub fn new(site: u64) -> Replica {
        Replica {
            site,
            lamport: 0,
            log: Vec::new(),
        }
    }

    /// This replica's site id.
    pub fn site(&self) -> u64 {
        self.site
    }

    /// Stamp the next local clock (bump the Lamport counter).
    fn tick(&mut self) -> Clock {
        self.lamport += 1;
        Clock {
            lamport: self.lamport,
            site: self.site,
        }
    }

    /// Record a locally-stamped op into the log and hand it back to broadcast.
    fn commit(&mut self, kind: OpKind) -> EditOp {
        let op = EditOp {
            clock: self.tick(),
            kind,
        };
        self.merge(op.clone());
        op
    }

    /// Add a part locally. Validates like [`CircuitModel::add_part`] (known kind,
    /// no empty/whitespace tokens) but — unlike the single-user editor — does
    /// **not** reject a duplicate reference: a concurrent add of the same ref on
    /// another replica is a legitimate conflict the merge rule resolves by clock.
    pub fn add_part(
        &mut self,
        reference: &str,
        node_a: &str,
        node_b: &str,
        value: &str,
    ) -> Result<EditOp, String> {
        kind_label(reference.trim())?;
        check_token("reference", reference.trim())?;
        check_token("first node", node_a.trim())?;
        check_token("second node", node_b.trim())?;
        check_token("value", value.trim())?;
        Ok(self.commit(OpKind::AddPart {
            reference: reference.trim().to_string(),
            node_a: node_a.trim().to_string(),
            node_b: node_b.trim().to_string(),
            value: value.trim().to_string(),
        }))
    }

    /// Remove a part locally.
    pub fn remove_part(&mut self, reference: &str) -> Result<EditOp, String> {
        check_token("reference", reference.trim())?;
        Ok(self.commit(OpKind::RemovePart {
            reference: reference.trim().to_string(),
        }))
    }

    /// Retune a part's value locally.
    pub fn set_value(&mut self, reference: &str, value: &str) -> Result<EditOp, String> {
        check_token("reference", reference.trim())?;
        check_token("value", value.trim())?;
        Ok(self.commit(OpKind::SetValue {
            reference: reference.trim().to_string(),
            value: value.trim().to_string(),
        }))
    }

    /// Re-wire a part's terminals locally.
    pub fn wire(&mut self, reference: &str, node_a: &str, node_b: &str) -> Result<EditOp, String> {
        check_token("reference", reference.trim())?;
        check_token("first node", node_a.trim())?;
        check_token("second node", node_b.trim())?;
        Ok(self.commit(OpKind::Wire {
            reference: reference.trim().to_string(),
            node_a: node_a.trim().to_string(),
            node_b: node_b.trim().to_string(),
        }))
    }

    /// Set (or clear) the title locally.
    pub fn set_title(&mut self, title: Option<&str>) -> EditOp {
        self.commit(OpKind::SetTitle {
            title: title.map(str::to_string),
        })
    }

    /// Merge one op (local or remote) into the log. Pulls the Lamport counter
    /// forward past the op's clock, then inserts it in sorted position — unless
    /// an op with the identical clock is already present, making redelivery and
    /// double-merge idempotent.
    pub fn merge(&mut self, op: EditOp) {
        if op.clock.lamport > self.lamport {
            self.lamport = op.clock.lamport;
        }
        match self.log.binary_search_by(|e| e.clock.cmp(&op.clock)) {
            Ok(_) => {} // already have this exact op (dedup) → idempotent
            Err(pos) => self.log.insert(pos, op),
        }
    }

    /// Merge a batch of ops (any order; duplicates fine).
    pub fn merge_many(&mut self, ops: &[EditOp]) {
        for op in ops {
            self.merge(op.clone());
        }
    }

    /// Every op this replica currently holds, in canonical `(lamport, site)`
    /// order — the set to hand another replica so both converge.
    pub fn ops(&self) -> &[EditOp] {
        &self.log
    }

    /// Derive the circuit from the op log by replaying it in clock order, so the
    /// highest-clock edit to each part/field wins (last-writer-wins). Pure in the
    /// log: calling it twice, or on another replica holding the same ops, yields
    /// an identical [`CircuitModel`].
    pub fn materialize(&self) -> CircuitModel {
        let mut title: Option<String> = None;
        let mut parts: BTreeMap<String, Part> = BTreeMap::new();
        for op in &self.log {
            match &op.kind {
                OpKind::AddPart {
                    reference,
                    node_a,
                    node_b,
                    value,
                } => {
                    parts.insert(
                        reference.clone(),
                        Part {
                            reference: reference.clone(),
                            node_a: node_a.clone(),
                            node_b: node_b.clone(),
                            value: value.clone(),
                        },
                    );
                }
                OpKind::RemovePart { reference } => {
                    parts.remove(reference);
                }
                OpKind::SetValue { reference, value } => {
                    if let Some(p) = parts.get_mut(reference) {
                        p.value = value.clone();
                    }
                }
                OpKind::Wire {
                    reference,
                    node_a,
                    node_b,
                } => {
                    if let Some(p) = parts.get_mut(reference) {
                        p.node_a = node_a.clone();
                        p.node_b = node_b.clone();
                    }
                }
                OpKind::SetTitle { title: t } => title = t.clone(),
            }
        }
        CircuitModel {
            title,
            parts: parts.into_values().collect(),
        }
    }

    /// The materialized circuit as netlist text (what two replicas must match).
    pub fn to_netlist(&self) -> String {
        self.materialize().to_netlist()
    }
}

impl EditOp {
    /// Encode an op into a URL/transport-safe token (base64url, no padding),
    /// round-tripping exactly through [`EditOp::decode`]. The payload is a small
    /// line-oriented blob wrapped in the same base64url as the share links, so a
    /// token drops straight into a `BroadcastChannel`/WebSocket message.
    pub fn encode(&self) -> String {
        let mut b = String::new();
        b.push_str("EEOP1\n");
        b.push_str(&format!("{}\n{}\n", self.clock.lamport, self.clock.site));
        match &self.kind {
            OpKind::AddPart {
                reference,
                node_a,
                node_b,
                value,
            } => {
                b.push_str("add\n");
                b.push_str(&format!("{reference}\n{node_a}\n{node_b}\n{value}\n"));
            }
            OpKind::RemovePart { reference } => {
                b.push_str(&format!("rm\n{reference}\n"));
            }
            OpKind::SetValue { reference, value } => {
                b.push_str(&format!("val\n{reference}\n{value}\n"));
            }
            OpKind::Wire {
                reference,
                node_a,
                node_b,
            } => {
                b.push_str(&format!("wire\n{reference}\n{node_a}\n{node_b}\n"));
            }
            OpKind::SetTitle { title } => match title {
                Some(t) => b.push_str(&format!("title\nsome\n{t}\n")),
                None => b.push_str("title\nnone\n"),
            },
        }
        encode(&b)
    }

    /// Decode a token produced by [`EditOp::encode`]. Tolerant of a leading `#`
    /// (via [`decode`]) but otherwise strict: a bad base64url token, wrong
    /// magic, a non-numeric clock, an unknown op tag, or a truncated body is a
    /// clean `Err` — **never** a panic. This is the guard that keeps a garbage
    /// message off the wire from crashing the page.
    pub fn decode(token: &str) -> Result<EditOp, String> {
        let raw = decode(token)?;
        let rest = raw
            .strip_prefix("EEOP1\n")
            .ok_or("not an edit op (bad magic/version)")?;

        let (lamport_s, rest) = take_line(rest)?;
        let lamport = lamport_s
            .parse::<u64>()
            .map_err(|_| format!("bad op lamport: {lamport_s:?}"))?;
        let (site_s, rest) = take_line(rest)?;
        let site = site_s
            .parse::<u64>()
            .map_err(|_| format!("bad op site: {site_s:?}"))?;
        let (tag, rest) = take_line(rest)?;

        let kind = match tag {
            "add" => {
                let (reference, rest) = take_line(rest)?;
                let (node_a, rest) = take_line(rest)?;
                let (node_b, rest) = take_line(rest)?;
                let (value, _) = take_line(rest)?;
                OpKind::AddPart {
                    reference: reference.to_string(),
                    node_a: node_a.to_string(),
                    node_b: node_b.to_string(),
                    value: value.to_string(),
                }
            }
            "rm" => {
                let (reference, _) = take_line(rest)?;
                OpKind::RemovePart {
                    reference: reference.to_string(),
                }
            }
            "val" => {
                let (reference, rest) = take_line(rest)?;
                let (value, _) = take_line(rest)?;
                OpKind::SetValue {
                    reference: reference.to_string(),
                    value: value.to_string(),
                }
            }
            "wire" => {
                let (reference, rest) = take_line(rest)?;
                let (node_a, rest) = take_line(rest)?;
                let (node_b, _) = take_line(rest)?;
                OpKind::Wire {
                    reference: reference.to_string(),
                    node_a: node_a.to_string(),
                    node_b: node_b.to_string(),
                }
            }
            "title" => {
                let (present, rest) = take_line(rest)?;
                match present {
                    "none" => OpKind::SetTitle { title: None },
                    "some" => {
                        let (t, _) = take_line(rest)?;
                        OpKind::SetTitle {
                            title: Some(t.to_string()),
                        }
                    }
                    other => return Err(format!("bad title op presence: {other:?}")),
                }
            }
            other => return Err(format!("unknown op tag: {other:?}")),
        };

        Ok(EditOp {
            clock: Clock { lamport, site },
            kind,
        })
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

    // --- Full-session shareable links (CP-WEB.4) ---------------------------

    /// Encode a full session (netlist + probes + view settings) into a
    /// URL-fragment-safe token. `step <= 0` means an adaptive step; `probes` is
    /// newline-separated (blank lines are dropped).
    #[wasm_bindgen]
    pub fn encode_session_share(netlist: &str, duration: f64, step: f64, probes: &str) -> String {
        let step = (step > 0.0).then_some(step);
        let probes = probes
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        super::encode_session(&super::Session {
            netlist: netlist.to_string(),
            duration,
            step,
            probes,
        })
    }

    /// Decode a session token back into JSON `{netlist, duration, step, probes}`
    /// for the page to restore the workbench. Throws on an invalid token.
    #[wasm_bindgen]
    pub fn decode_session_share(token: &str) -> Result<String, JsValue> {
        super::decode_session(token)
            .map(|s| s.to_json())
            .map_err(|e| JsValue::from_str(&e))
    }

    // --- Real-time co-edit relay (CP-WEB.5) --------------------------------
    //
    // A hand-rolled binding to the browser's `BroadcastChannel` (same-origin
    // tabs) — no `web-sys`/`js-sys` dependency, matching the house style. Only
    // the *outbound* `postMessage` is bound here: the page wires the inbound
    // side with `channel.onmessage = e => coedit.receive(e.data)`, so this stays
    // a thin client binding. A hosted signaling/relay **server** (for cross-
    // machine sessions over a WebSocket) stays out of scope — deferred — and
    // would slot in exactly here by swapping the transport for a socket whose
    // `send`/`onmessage` carry the very same op tokens.

    #[wasm_bindgen]
    extern "C" {
        type BroadcastChannel;
        #[wasm_bindgen(constructor)]
        fn new(name: &str) -> BroadcastChannel;
        #[wasm_bindgen(method, js_name = postMessage)]
        fn post_message(this: &BroadcastChannel, msg: &str);
        #[wasm_bindgen(method)]
        fn close(this: &BroadcastChannel);
    }

    /// A live co-editing session: a [`Replica`](super::Replica) wired to a
    /// same-origin `BroadcastChannel`. Local edits mutate the replica, broadcast
    /// their op, and return the updated netlist; [`receive`](CoEdit::receive)
    /// merges an op the page read off the channel. Two tabs sharing a channel
    /// name converge to an identical netlist.
    #[wasm_bindgen]
    pub struct CoEdit {
        replica: super::Replica,
        channel: BroadcastChannel,
    }

    #[wasm_bindgen]
    impl CoEdit {
        /// Join the co-edit session named `channel` as site `site` (give each
        /// tab a distinct id so the merge tiebreak is deterministic).
        #[wasm_bindgen(constructor)]
        pub fn new(site: u64, channel: &str) -> CoEdit {
            CoEdit {
                replica: super::Replica::new(site),
                channel: BroadcastChannel::new(channel),
            }
        }

        /// Broadcast a locally-committed op and return the updated netlist.
        fn emit(&self, op: &super::EditOp) -> String {
            self.channel.post_message(&op.encode());
            self.replica.to_netlist()
        }

        /// Add a part locally, broadcast it, return the new netlist. Throws on
        /// an invalid part (unknown kind / empty / whitespace token).
        pub fn add_part(
            &mut self,
            reference: &str,
            node_a: &str,
            node_b: &str,
            value: &str,
        ) -> Result<String, JsValue> {
            let op = self
                .replica
                .add_part(reference, node_a, node_b, value)
                .map_err(|e| JsValue::from_str(&e))?;
            Ok(self.emit(&op))
        }

        /// Remove a part locally, broadcast it, return the new netlist.
        pub fn remove_part(&mut self, reference: &str) -> Result<String, JsValue> {
            let op = self
                .replica
                .remove_part(reference)
                .map_err(|e| JsValue::from_str(&e))?;
            Ok(self.emit(&op))
        }

        /// Retune a part's value locally, broadcast it, return the new netlist.
        pub fn set_value(&mut self, reference: &str, value: &str) -> Result<String, JsValue> {
            let op = self
                .replica
                .set_value(reference, value)
                .map_err(|e| JsValue::from_str(&e))?;
            Ok(self.emit(&op))
        }

        /// Re-wire a part locally, broadcast it, return the new netlist.
        pub fn wire(
            &mut self,
            reference: &str,
            node_a: &str,
            node_b: &str,
        ) -> Result<String, JsValue> {
            let op = self
                .replica
                .wire(reference, node_a, node_b)
                .map_err(|e| JsValue::from_str(&e))?;
            Ok(self.emit(&op))
        }

        /// Set (or clear, with an empty string) the title locally and broadcast.
        pub fn set_title(&mut self, title: &str) -> String {
            let title = (!title.is_empty()).then_some(title);
            let op = self.replica.set_title(title);
            self.emit(&op)
        }

        /// Merge an op token read off the channel and return the updated
        /// netlist. Throws (never panics) on a malformed/garbage token, so the
        /// page can ignore a bad message.
        pub fn receive(&mut self, token: &str) -> Result<String, JsValue> {
            let op = super::EditOp::decode(token).map_err(|e| JsValue::from_str(&e))?;
            self.replica.merge(op);
            Ok(self.replica.to_netlist())
        }

        /// The current materialized netlist.
        pub fn netlist(&self) -> String {
            self.replica.to_netlist()
        }

        /// Close the underlying channel (call before dropping the session).
        pub fn close(&self) {
            self.channel.close();
        }
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

    // --- full-session encode/decode round-trip (CP-WEB.4) ------------------

    #[test]
    fn session_round_trips_exactly() {
        let cases = [
            Session {
                netlist: RC.to_string(),
                duration: 5e-3,
                step: Some(5e-6),
                probes: vec!["out".into(), "in".into()],
            },
            Session {
                netlist: "V1 in gnd 5\n".to_string(),
                duration: 1.0,
                step: None, // adaptive
                probes: vec![],
            },
            Session {
                // CRLF, tabs, trailing content, and unicode in the netlist all
                // survive byte-exact; empty-probe and odd settings too.
                netlist: ".title café-π\r\nV1 in gnd 5\tno trailing newline".to_string(),
                duration: 0.000123,
                step: Some(1e-9),
                probes: vec!["a".into(), "b".into(), "c".into()],
            },
        ];
        for s in cases {
            let token = encode_session(&s);
            let back = decode_session(&token).expect("valid session token decodes");
            assert_eq!(back, s, "session round trip must be exact");
        }
    }

    #[test]
    fn session_token_is_url_fragment_safe() {
        let token = encode_session(&Session {
            netlist: ".title Ω\nV1 in gnd 5\n".to_string(),
            duration: 5e-3,
            step: Some(5e-6),
            probes: vec!["out".into()],
        });
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "token must be pure base64url: {token}"
        );
    }

    #[test]
    fn decoded_session_netlist_runs() {
        let s = Session {
            netlist: RC.to_string(),
            duration: 5e-3,
            step: Some(5e-6),
            probes: vec!["out".into()],
        };
        let back = decode_session(&format!("#{}", encode_session(&s))).expect("leading # ok");
        let r = run(&back.netlist, back.duration, back.step).expect("session netlist simulates");
        assert_eq!(r.title, "rc-lowpass");
        assert_eq!(r.nodes.len(), 2);
    }

    #[test]
    fn decode_session_of_garbage_is_a_clean_error() {
        assert!(decode_session("has spaces").is_err(), "bad base64url");
        // Valid base64url, but not a session blob (wrong magic).
        assert!(
            decode_session(&encode("not a session")).is_err(),
            "wrong magic"
        );
        // Right magic, truncated body.
        assert!(decode_session(&encode("EEWS1\n")).is_err(), "truncated");
    }

    #[test]
    fn session_to_json_is_wellformed() {
        let json = Session {
            netlist: "V1 in gnd 5\n".into(),
            duration: 5e-3,
            step: None,
            probes: vec!["out".into()],
        }
        .to_json();
        assert!(json.contains("\"netlist\":\"V1 in gnd 5\\n\""));
        assert!(json.contains("\"duration\":0.005"));
        assert!(json.contains("\"step\":null"));
        assert!(json.contains("\"probes\":[\"out\"]"));
    }

    // --- co-edit merge layer (CP-WEB.5) ------------------------------------

    /// Seed a replica from the RC netlist so two sites can start equal: lift
    /// the netlist into a model and replay its parts/title as ops. Returns the
    /// seeding ops so the other replica can merge the identical set.
    fn seed(site: u64) -> (Replica, Vec<EditOp>) {
        let model = CircuitModel::from_netlist(RC).unwrap();
        let mut r = Replica::new(site);
        let mut ops = Vec::new();
        if let Some(t) = &model.title {
            ops.push(r.set_title(Some(t)));
        }
        for p in &model.parts {
            ops.push(
                r.add_part(&p.reference, &p.node_a, &p.node_b, &p.value)
                    .unwrap(),
            );
        }
        (r, ops)
    }

    #[test]
    fn op_encode_decode_round_trips() {
        let ops = [
            EditOp {
                clock: Clock {
                    lamport: 7,
                    site: 2,
                },
                kind: OpKind::AddPart {
                    reference: "R9".into(),
                    node_a: "out".into(),
                    node_b: "gnd".into(),
                    value: "2k".into(),
                },
            },
            EditOp {
                clock: Clock {
                    lamport: 1,
                    site: 0,
                },
                kind: OpKind::RemovePart {
                    reference: "C1".into(),
                },
            },
            EditOp {
                clock: Clock {
                    lamport: 42,
                    site: 9,
                },
                kind: OpKind::SetValue {
                    reference: "R1".into(),
                    value: "3k3".into(),
                },
            },
            EditOp {
                clock: Clock {
                    lamport: 3,
                    site: 1,
                },
                kind: OpKind::Wire {
                    reference: "R1".into(),
                    node_a: "a".into(),
                    node_b: "b".into(),
                },
            },
            EditOp {
                clock: Clock {
                    lamport: 5,
                    site: 4,
                },
                kind: OpKind::SetTitle {
                    title: Some("café-π".into()),
                },
            },
            EditOp {
                clock: Clock {
                    lamport: 6,
                    site: 4,
                },
                kind: OpKind::SetTitle { title: None },
            },
        ];
        for op in ops {
            let token = op.encode();
            // URL/transport-safe (same guarantee as the share links).
            assert!(
                token
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "op token must be pure base64url: {token}"
            );
            assert_eq!(EditOp::decode(&token).unwrap(), op, "op round trip exact");
            // Leading '#' tolerated, like the other decoders.
            assert_eq!(EditOp::decode(&format!("#{token}")).unwrap(), op);
        }
    }

    #[test]
    fn two_replicas_converge_to_identical_netlist() {
        // Both sites start from the identical seed (same op set).
        let (mut a, seed_ops) = seed(1);
        let mut b = Replica::new(2);
        b.merge_many(&seed_ops);
        assert_eq!(a.to_netlist(), b.to_netlist(), "equal after seeding");

        // Each site makes independent local edits.
        let a_ops = [
            a.add_part("R2", "out", "gnd", "2k").unwrap(),
            a.set_value("R1", "4k7").unwrap(),
        ];
        let b_ops = [
            b.remove_part("C1").unwrap(),
            b.add_part("L1", "out", "gnd", "10m").unwrap(),
        ];

        // Exchange the ops — deliberately in *different* orders on each side,
        // including a redundant redelivery — and they still converge.
        a.merge_many(&b_ops);
        b.merge(a_ops[1].clone());
        b.merge(a_ops[0].clone());
        b.merge(a_ops[1].clone()); // duplicate delivery is idempotent

        let na = a.to_netlist();
        let nb = b.to_netlist();
        assert_eq!(na, nb, "replicas converge to one netlist:\n{na}\n---\n{nb}");
        // And the converged netlist reflects every edit and still simulates.
        assert!(na.contains("R2 out gnd 2k"));
        assert!(na.contains("R1 in out 4k7"));
        assert!(na.contains("L1 out gnd 10m"));
        assert!(!na.contains("C1"), "C1 was removed on b: {na}");
        run(&na, 5e-3, Some(5e-6)).expect("converged netlist simulates");
    }

    #[test]
    fn conflicting_edits_resolve_deterministically_either_order() {
        // Two replicas start equal, then concurrently edit the SAME part/value.
        let (mut a, seed_ops) = seed(1);
        let mut b = Replica::new(2);
        b.merge_many(&seed_ops);

        // Concurrent writes to R1's value. They may land at the same Lamport
        // time; the site id is the deterministic tiebreak (higher site wins).
        let op_a = a.set_value("R1", "100").unwrap();
        let op_b = b.set_value("R1", "200").unwrap();

        // Deliver in opposite orders on the two replicas.
        a.merge(op_b.clone());
        b.merge(op_a.clone());

        let na = a.to_netlist();
        let nb = b.to_netlist();
        assert_eq!(na, nb, "same winner regardless of delivery order");

        // The winner is a pure function of the clocks: higher (lamport, site).
        let winner = if op_a.clock > op_b.clock {
            "100"
        } else {
            "200"
        };
        assert!(
            na.contains(&format!("R1 in out {winner}")),
            "deterministic LWW winner {winner}: {na}"
        );

        // A third, independent delivery order reaches the identical state.
        let (mut c, _) = seed(1);
        c.merge(op_b);
        c.merge(op_a);
        assert_eq!(c.to_netlist(), na, "a third order converges to the same");
    }

    #[test]
    fn concurrent_add_remove_of_same_part_is_deterministic() {
        // One site re-adds a part the other removes, concurrently. LWW by clock
        // must pick the same outcome on both regardless of delivery order.
        let (mut a, seed_ops) = seed(1);
        let mut b = Replica::new(2);
        b.merge_many(&seed_ops);

        let add = a.add_part("C1", "out", "gnd", "5u").unwrap(); // overwrite C1
        let rm = b.remove_part("C1").unwrap();

        a.merge(rm.clone());
        b.merge(add.clone());
        assert_eq!(
            a.to_netlist(),
            b.to_netlist(),
            "converge on add/remove race"
        );
    }

    #[test]
    fn garbage_ops_decode_to_clean_error() {
        let junk = [
            "has spaces",                           // not base64url
            "plus+sign",                            // reserved char
            &encode("not an op"),                   // valid base64url, wrong magic
            &encode("EEOP1\n"),                     // right magic, truncated
            &encode("EEOP1\nnotanum\n0\nrm\nR1\n"), // non-numeric lamport
            &encode("EEOP1\n1\n0\nbogus\nR1\n"),    // unknown op tag
            &encode("EEOP1\n1\n0\ntitle\nmaybe\n"), // bad title presence
            &encode("EEOP1\n1\n0\nadd\nR1\nin\n"),  // add missing fields
        ];
        for token in junk {
            let got = EditOp::decode(token);
            assert!(
                got.is_err(),
                "garbage {token:?} must be clean Err, got {got:?}"
            );
        }
    }

    #[test]
    fn merge_is_idempotent_and_order_free_on_the_same_set() {
        // The same set of ops, fed in forward vs reverse vs duplicated order,
        // always materializes to the identical netlist (order independence).
        let (donor, mut ops) = seed(7);
        ops.push({
            let mut d = donor;
            d.set_value("R1", "9k1").unwrap()
        });

        let mut forward = Replica::new(0);
        forward.merge_many(&ops);

        let mut reverse = Replica::new(0);
        let mut rev: Vec<EditOp> = ops.clone();
        rev.reverse();
        reverse.merge_many(&rev);

        let mut dupes = Replica::new(0);
        dupes.merge_many(&ops);
        dupes.merge_many(&ops); // every op twice

        assert_eq!(forward.to_netlist(), reverse.to_netlist());
        assert_eq!(forward.to_netlist(), dupes.to_netlist());
        assert_eq!(
            forward.ops().len(),
            dupes.ops().len(),
            "dedup collapsed dupes"
        );
    }
}
