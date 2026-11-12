# ohmforge-web — ohmforge in the browser (CP-WEB.1–CP-WEB.4)

`wasm-bindgen` glue that runs the (already wasm-clean) `ohmforge-sim` engine in a web
browser. You paste a netlist (or pick a demo), and the page parses it with
`ohmforge_fmt::netlist::parse`, runs an **operating point** and a **transient** with
`ohmforge_sim::Engine`, and shows the result: a braille scope string in a `<pre>` plus
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

The whole "netlist → run → result" pipeline is plain Rust (`ohmforge_web::run` →
`ohmforge_web::Report`) that compiles and is unit-tested **on the host**. The only
browser-specific code is a thin `#[wasm_bindgen]` wrapper (`simulate`, `scope`)
gated behind `cfg(target_arch = "wasm32")`.

This matters for the workspace: `wasm-bindgen` / `web-sys` / `js-sys` do **not**
build for the host target (`aarch64-apple-darwin`). So the wasm dependency is
declared under `[target.'cfg(target_arch = "wasm32")'.dependencies]` and is
absent from the host dependency graph. On the host, `ohmforge-web` pulls in only the
three wasm-clean compute crates (`ohmforge-core`, `ohmforge-sim`, `ohmforge-fmt`), so
`cargo build/clippy/test --workspace` build and test it like any other member —
the host gates stay green, and `cargo test --workspace` runs the pipeline test in
`src/lib.rs` without needing a browser.

> Note: we deliberately do **not** depend on `ohmforge-tui` for its braille `Plot`.
> `ohmforge-tui` pulls in `crossterm`, which does not compile for
> `wasm32-unknown-unknown`. `ohmforge-web` carries its own small, dependency-free
> braille renderer (`src/braille.rs`), a port of the same technique.

## Build

### With `wasm-pack` (recommended)

```sh
# from the workspace root
wasm-pack build crates/ohmforge-web --target web --out-dir www/pkg
```

This writes the JS bindings + optimized `.wasm` into `crates/ohmforge-web/www/pkg/`
(git-ignored — it is a build artifact). `crates/ohmforge-web/www/index.html` imports
`./pkg/ohmforge_web.js`.

### Without `wasm-pack` (plain cargo)

The crate also builds straight to a `.wasm` with cargo:

```sh
cargo build -p ohmforge-web --target wasm32-unknown-unknown
```

To use that artifact in the browser you still need the JS bindings; install the
matching CLI (`cargo install wasm-bindgen-cli`) and run `wasm-bindgen` on the
`.wasm` under `target/wasm32-unknown-unknown/…`. `wasm-pack` just automates both
steps, so it is the recommended path.

## Serve

`--target web` modules load over `fetch`, so open the page through a web server
(not `file://`):

```sh
cd crates/ohmforge-web/www
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
The real logic is a pure, host-tested model — `ohmforge_web::CircuitModel`, a title
plus an ordered list of `ohmforge_web::Part` (`reference`, `node_a`, `node_b`,
`value`) — that round-trips to netlist text:

- `CircuitModel::from_netlist(&str)` lifts the textarea into the model,
  preserving the node names and the raw engineering-notation value strings
  (`1k`, `100n`) that `ohmforge_fmt`'s `Circuit` discards once it resolves nets to
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

The encode/decode pair is **pure host Rust** (`ohmforge_web::encode` / `ohmforge_web::decode`,
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
all survive. `ohmforge_web::encode_session` / `ohmforge_web::decode_session` are **pure host
Rust** (`Session` → token → `Session`), unit-tested in CI; `encode_session_share`
/ `decode_session_share` are the thin wasm wrappers. In the page, **Copy session
link** encodes `{netlist, duration, step, probes}`, and on load (and `hashchange`)
the page decodes it, restores every field, and re-runs — falling back to a legacy
netlist-only token if the fragment is not a session.

## Real-time co-edit (CP-WEB.5)

A session link is an async snapshot; live co-editing needs edits from two
browsers to *converge*. That merge layer is `ohmforge_web::Replica` + `EditOp`: every
edit is a small, clock-tagged op (add/remove/update part, set value, re-wire,
set title), replicas exchange ops over any transport in any order (with
duplicates), and each replica **materializes** the circuit by replaying its op
log in `(lamport, site)` order. Final state is a pure function of the *set* of
ops, so replicas converge to an identical netlist; conflicting concurrent edits
resolve last-writer-wins by clock, with the site id as the deterministic
tiebreak. It is plain host Rust (one `BTreeMap`, no new deps), unit-tested in CI:

```text
two replicas edit, swap ops in different orders → identical netlist
two concurrent writes to one value         → same winner on both, either order
a garbage op token                         → clean Err (never a panic)
```

The browser side is the thin `CoEdit` binding: a `Replica` wired to a same-origin
`BroadcastChannel` (a hand-rolled `wasm-bindgen` extern — no `web-sys`). Local
part edits broadcast their op; the page feeds inbound messages back through
`CoEdit::receive`. Each op is a URL/transport-safe base64url token
(`EditOp::encode`/`decode`), so the exact same tokens would travel over a
WebSocket unchanged.

**Hosted signaling relay (CP-WEB.6).** `BroadcastChannel` only reaches tabs on
the same origin on one machine; cross-machine sessions need a relay (a WebSocket
the op tokens flow through). That relay now ships. Its brain is a pure,
host-tested router, `Relay` — clients `JOIN <room>`, an `OP <token>` is
delivered to every *other* member of that room, join/leave are tracked, and a
malformed frame (or a garbage op token) is a clean `Err`, never a panic:

```text
op from one client → every OTHER member of its room (not the sender)
two clients in a room, relayed ops → identical netlist (fed to each Replica)
a malformed frame / bad op token   → clean Err (never a panic)
clients in different rooms          → isolated
```

The socket server behind it is a thin, **native-only** binary gated behind the
`relay` cargo feature (so the default workspace build and the `wasm32` bundle
stay clean and dep-free). The WebSocket upgrade — SHA-1 accept key + frame
parse/mask — is hand-rolled over `std::net` in the house style, pulling **no new
dependencies**. Run it with:

```sh
cargo run -p ohmforge-web --features relay --bin relay -- 127.0.0.1:9001
```

A browser `CoEdit` client points a WebSocket at `ws://HOST:PORT` in place of its
`BroadcastChannel` and sends the very same `EditOp` tokens unchanged.
