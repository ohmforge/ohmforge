# ohmforge-web — wasm bindings + browser app

The WebAssembly front end for [ohmforge](https://github.com/ohmforge/ohmforge):
it parses a netlist, runs the (already wasm-clean) `ohmforge-sim` engine in the
browser, and hands the result back to JavaScript as JSON plus a ready-to-display
braille scope string. This is how ohmforge runs in a browser tab with no server
doing the math.

The whole "netlist → run → result" pipeline is plain Rust (`run`, `Report`) that
compiles and is unit-tested on the host, so it is proven in CI without a
browser. Only the thin `#[wasm_bindgen]` wrapper is browser-specific, gated
behind `cfg(target_arch = "wasm32")`. On the host this crate depends on nothing
but the dependency-free compute crates (`ohmforge-core`, `ohmforge-sim`) plus
`ohmforge-fmt`, keeping `wasm-bindgen` out of the host dependency graph.

An opt-in, native-only `relay` feature adds a small signaling server (and a
`relay` binary) for browser-to-browser co-editing; it pulls no new dependencies
and never compiles into the browser bundle.

## Usage

Build the wasm bundle with `wasm-pack`:

```sh
wasm-pack build crates/ohmforge-web --target web
```

Then call the exported functions from JavaScript:

```js
import init, { simulate, scope } from "./pkg/ohmforge_web.js";

await init();

const netlist = `
V1 in 0 5
R1 in out 1k
C1 out 0 1u
.end
`;

// Full result as JSON (op point + transient traces + scope).
const json = simulate(netlist, 5e-3, 0);        // step 0 => adaptive
console.log(JSON.parse(json));

// Or just the braille scope string for a <pre>.
document.querySelector("pre").textContent = scope(netlist, 5e-3, 0);
```

To run the co-edit relay server natively:

```sh
cargo run -p ohmforge-web --features relay --bin relay
```

---

Part of [**ohmforge**](https://github.com/ohmforge/ohmforge) — the AI-native
circuit & firmware workbench.

License: **MIT OR Apache-2.0**.
