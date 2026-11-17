# ohmforge-tui — terminal UI

The terminal UI for [ohmforge](https://github.com/ohmforge/ohmforge), built on
`ratatui` + `crossterm`: schematic rendering with a live voltage/current
overlay, high-resolution **braille** waveform plots, dockable panels (schematic,
scope, shell, AI chat, parts palette, instruments), a command palette, and
mouse-driven part placement. It works over SSH.

Rendering is decoupled from simulation via snapshots, so a slow redraw never
stalls the solver. This crate sits above the compute core and consumes its
`Signal`s and `Circuit`s; it owns presentation, not solving.

## Usage

```rust
use ohmforge_tui::Plot;

// Render a waveform as a braille plot you can drop into any TUI pane.
let x: Vec<f64> = (0..200).map(|i| i as f64 * 1e-4).collect();
let y: Vec<f64> = x.iter().map(|t| (2.0 * std::f64::consts::PI * 100.0 * t).sin()).collect();

let frame = Plot::new("v(out)")
    .units("V", "s")
    .size(60, 12)
    .render(&x, &y);

println!("{frame}");
```

The full interactive event loop is driven by `ohmforge_tui::run` from the `of`
binary.

---

Part of [**ohmforge**](https://github.com/ohmforge/ohmforge) — the AI-native
circuit & firmware workbench.

License: **MIT OR Apache-2.0**.
