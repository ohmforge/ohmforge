# ee-web — ee-assist in the browser (CP-WEB.1)

`wasm-bindgen` glue that runs the (already wasm-clean) `ee-sim` engine in a web
browser. You paste a netlist (or pick a demo), and the page parses it with
`ee_fmt::netlist::parse`, runs an **operating point** and a **transient** with
`ee_sim::Engine`, and shows the result: a braille scope string in a `<pre>` plus
the operating-point node voltages.

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

`duration` is the transient length in seconds; `step` is the fixed timestep in
seconds, or `0` (≤ 0) to let the engine pick an adaptive step. Non-finite samples
serialize as JSON `null`.
