# ee-web — ee-assist in the browser (CP-WEB.1–CP-WEB.4)

`wasm-bindgen` glue that runs the (already wasm-clean) `ee-sim` engine in a web
browser. You paste a netlist (or pick a demo), and the page parses it with
`ee_fmt::netlist::parse`, runs an **operating point** and a **transient** with
`ee_sim::Engine`, and shows the result: a braille scope string in a `<pre>` plus
the operating-point node voltages.

You can also **build** the circuit, not just paste it (CP-WEB.3): a parts form
adds a part (ref / nodes / value), lists the parts, and removes one, staying in
sync with the netlist text and re-simulating on every edit.

A circuit is also a **shareable link** (CP-WEB.2, extended to a **full session**
in CP-WEB.4): **Copy session link** packs the whole workbench — the netlist plus
the probes and the duration/step view settings — into the URL fragment, and
opening that link restores someone's exact workbench and re-runs it. "Open in a
browser, poke it", shareable by copy-paste. Real-time co-editing is out of scope
(see below).

## Architecture — pure core, thin wasm shell

The whole "netlist → run → result" pipeline is plain Rust (`ee_web::run` →
`ee_web::Report`) that compiles and is unit-tested **on the host**. The only
browser-specific code is a thin `#[wasm_bindgen]` wrapper (`simulate`, `scope`)
gated behind `cfg(target_arch = "wasm32")`.

This matters for the workspace: `wasm-bindgen` / `web-sys` / `js-sys` do **not**
build for the host target (`aarch64-apple-darwin`). So the wasm dependency is
declared under `[target.'cfg(target_arch = "wasm32")'.dependencies]` and is
absent from the host dependency graph. On the host, `ee-web` pulls in only the
three wasm-clean compute crates (`ee-core`, `ee-sim`, `ee-fmt`), so
`cargo build/clippy/test --workspace` build and test it like any other member —
the host gates stay green, and `cargo test --workspace` runs the pipeline test in
`src/lib.rs` without needing a browser.

> Note: we deliberately do **not** depend on `ee-tui` for its braille `Plot`.
> `ee-tui` pulls in `crossterm`, which does not compile for
> `wasm32-unknown-unknown`. `ee-web` carries its own small, dependency-free
> braille renderer (`src/braille.rs`), a port of the same technique.

## Build

### With `wasm-pack` (recommended)

```sh
# from the workspace root
wasm-pack build crates/ee-web --target web --out-dir www/pkg
```

This writes the JS bindings + optimized `.wasm` into `crates/ee-web/www/pkg/`
(git-ignored — it is a build artifact). `crates/ee-web/www/index.html` imports
`./pkg/ee_web.js`.

### Without `wasm-pack` (plain cargo)

The crate also builds straight to a `.wasm` with cargo:

```sh
cargo build -p ee-web --target wasm32-unknown-unknown
```

To use that artifact in the browser you still need the JS bindings; install the
matching CLI (`cargo install wasm-bindgen-cli`) and run `wasm-bindgen` on the
`.wasm` under `target/wasm32-unknown-unknown/…`. `wasm-pack` just automates both
steps, so it is the recommended path.

## Serve

`--target web` modules load over `fetch`, so open the page through a web server
(not `file://`):

```sh
cd crates/ee-web/www
python3 -m http.server 8080
# then open http://localhost:8080/
```

## Binding API

Both functions parse the netlist, run op-point + transient, and throw a JS
exception (the error string) on a parse/solver failure.

| JS export | Signature | Returns |
|-----------|-----------|---------|
| `simulate(netlist, duration, step)` | `(string, number, number) → string` | JSON: `{title, x, nodes:[{name,y}], op:[{name,v}], scope}` |
| `scope(netlist, duration, step)` | `(string, number, number) → string` | just the braille scope string |
| `encode_share(netlist)` | `(string) → string` | a URL-fragment-safe share token for the netlist |
| `decode_share(fragment)` | `(string) → string` | the netlist for a token (leading `#` ok), or `""` if the fragment is empty/invalid |
| `list_parts(netlist)` | `(string) → string` | JSON `{title, parts:[{reference,kind,node_a,node_b,value}]}`; throws on a malformed netlist |
| `edit_add(netlist, ref, a, b, value)` | `(string, …) → string` | the netlist with a part appended; throws on an invalid/duplicate part |
| `edit_remove(netlist, ref)` | `(string, string) → string` | the netlist with that reference removed (a no-op if absent) |
| `encode_session_share(netlist, duration, step, probes)` | `(string, number, number, string) → string` | a session token (netlist + settings + newline-separated probes); `step ≤ 0` = adaptive |
| `decode_session_share(token)` | `(string) → string` | JSON `{netlist, duration, step, probes:[…]}` for a session token (leading `#` ok); throws on an invalid token |

`duration` is the transient length in seconds; `step` is the fixed timestep in
seconds, or `0` (≤ 0) to let the engine pick an adaptive step. Non-finite samples
serialize as JSON `null`.

## Structured editing (CP-WEB.3)

The browser schematic editor is a structured view over the same netlist text.
The real logic is a pure, host-tested model — `ee_web::CircuitModel`, a title
plus an ordered list of `ee_web::Part` (`reference`, `node_a`, `node_b`,
`value`) — that round-trips to netlist text:

- `CircuitModel::from_netlist(&str)` lifts the textarea into the model,
  preserving the node names and the raw engineering-notation value strings
  (`1k`, `100n`) that `ee_fmt`'s `Circuit` discards once it resolves nets to
  `NetId`s; a malformed line is a clean `Err`, never a panic.
- `to_netlist()` emits a clean, canonical netlist that `run` parses + simulates.
- `add_part` / `remove_part` are the edit operations (add validates the kind,
  rejects empties / embedded whitespace / duplicate references).

The `#[wasm_bindgen]` layer (`list_parts` / `edit_add` / `edit_remove`) stays
thin: it takes netlist text in and hands netlist text (or a JSON part list) back
out, so the page keeps the textarea as the single source of truth. In the page,
the **Parts** form lists the parts with a *remove* button each, the **Add part**
button appends one, typing in the netlist re-lists the parts, and every edit
re-runs the simulation. Host tests prove the edit → netlist round trip:
`add_part_changes_the_netlist_and_still_simulates` and
`remove_part_drops_it_from_the_netlist_and_still_simulates` both assert the text
changes as expected and still parses + simulates via `run`.

## Shareable circuit links (CP-WEB.2)

A circuit is just its netlist text, so a shareable link is that text packed into
the URL fragment (`https://…/#<token>`). The token is hand-rolled **base64url**
(RFC 4648 §5: alphabet `A–Z a–z 0–9 - _`, **no `=` padding**) of the netlist's
UTF-8 bytes. base64url is chosen because a fragment is copy-pasted whole and the
standard base64 extras all misbehave there — `+` can read as a space, `/` splits
paths, `=` reads as a key/value separator — whereas `-` and `_` are unreserved
and the unpadded output never emits `=`. `decode` is tolerant of a leading `#`
and of any `=` padding, but otherwise strict: a non-alphabet character or
non-UTF-8 byte sequence is a clean error (the page treats it as "no circuit"),
never a panic.

The encode/decode pair is **pure host Rust** (`ee_web::encode` / `ee_web::decode`,
no dependency), so it is unit-tested in CI without a browser; `encode_share` /
`decode_share` are the thin `#[wasm_bindgen]` wrappers. The page no longer makes
a netlist-only link itself (it shares a full session, below), but `decode_share`
stays wired so that **older CP-WEB.2 netlist-only links still open**.

## Full-session shareable links (CP-WEB.4)

CP-WEB.4 extends the share link from "just the netlist" to the **full session**,
so a link restores someone's exact workbench: the netlist **plus** the probes
and the transient `duration`/`step` the page holds. The session is serialized to
a small, line-oriented text blob and run through the *same* hand-rolled base64url
as CP-WEB.2, so the token still drops straight into `location.hash`:

```text
EEWS1\n            magic + version
<duration f64>\n   Rust's shortest round-trippable f64 form
<step | auto>\n
<probe count>\n
<probe>\n × count  node names (whitespace-free → one per line)
<netlist…>         everything after the header, byte-exact
```

Putting the netlist last and copying the remaining bytes wholesale is what makes
the round trip **exact** — its comments, blank lines, CRLFs and trailing newline
all survive. `ee_web::encode_session` / `ee_web::decode_session` are **pure host
Rust** (`Session` → token → `Session`), unit-tested in CI; `encode_session_share`
/ `decode_session_share` are the thin wasm wrappers. In the page, **Copy session
link** encodes `{netlist, duration, step, probes}`, and on load (and `hashchange`)
the page decodes it, restores every field, and re-runs — falling back to a legacy
netlist-only token if the fragment is not a session.

**Out of scope: real-time co-editing.** A session link is a point-in-time
snapshot (async collaboration — you send a link, they open it later). Live
multi-cursor co-editing is deliberately *not* built here: it needs a server-side
relay or a CRDT backend to merge concurrent edits, which this static,
backend-free page (just `.wasm` + `.js` + `.html`, served from any file host)
has no place for.
