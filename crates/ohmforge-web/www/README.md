# ohmforge — web landing page

This directory is the static "try it in your browser" home for
[ohmforge](https://github.com/ohmforge/ohmforge). It is plain HTML/CSS plus the
`ohmforge-sim` engine compiled to WebAssembly — no build step beyond
`wasm-pack`, no external CDNs.

```
www/
├── index.html         # hero + live demo, example gallery, schematic canvas,
│                      #   features, terminal snippet, closing CTA, footer
├── style.css          # design system: tokens, type scale, layout
│                      #   (light/dark, forge-amber accent, reduced-motion safe)
├── ohmforge-mark.svg  # square mark (favicon)
├── ohmforge-logo.svg  # wordmark
└── pkg/               # generated WASM bundle (see below) — not checked in
```

The page is a single self-contained document: a system-font stack, inline SVG
icons, and CSS custom properties for the whole theme — no external fonts, icon
fonts, or script CDNs, so it renders identically offline.

## 1. Build the WASM bundle

From the repository root, with [`wasm-pack`](https://rustwasm.github.io/wasm-pack/)
installed:

```sh
wasm-pack build crates/ohmforge-web --target web --out-dir www/pkg
```

This emits `pkg/ohmforge_web.js` + `pkg/ohmforge_web_bg.wasm` here, which
`index.html` imports as an ES module (`./pkg/ohmforge_web.js`).

> If you open the page before building, it still renders — the demo panel shows
> a "build the WASM bundle" hint with this exact command instead of a blank
> page.

## 2. Serve the page locally

ES-module imports and WASM require HTTP (not `file://`). Any static server works:

```sh
# Python (bundled on most systems)
python3 -m http.server 8080 --directory crates/ohmforge-web/www
# …then open http://localhost:8080
```

```sh
# or, if you have it, any equivalent:
npx serve crates/ohmforge-web/www
```

The demo auto-runs on load: a prefilled RC low-pass circuit simulates and draws
its braille scope + operating point within a second. Use **Run it** to re-run,
and the **Tweak the run** disclosure for the netlist demos, duration/step/probes,
the parts editor, session-link sharing, and tab co-editing.

## License

Apache-2.0. ohmforge is a trademark of its maintainers.
