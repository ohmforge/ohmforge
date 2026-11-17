# ohmforge-fmt — file formats & ecosystem interop

The I/O layer for [ohmforge](https://github.com/ohmforge/ohmforge). It reads and
writes circuits and waveforms in the formats the wider ecosystem uses, and
defines ohmforge's own native project format — human-readable and diff-friendly,
so circuits live happily in git.

Supported: native project load/save, SPICE netlist import/export, KiCad netlist
import, Fritzing sketch export, bill-of-materials (BOM) export to CSV, and
waveform export to CSV and VCD. Everything here maps to and from the
`ohmforge_core::Circuit` and `Signal` types, so formats are an edge around the
compute core rather than a dependency of it.

## Usage

```rust
use ohmforge_fmt::{import_spice, save_project, to_bom_csv};

// Import a SPICE netlist into a Circuit.
let spice = "\
* RC divider
V1 in 0 5
R1 in out 10k
R2 out 0 10k
.end
";
let circuit = import_spice(spice).expect("valid netlist");

// Round-trip it through the native project format (diff-friendly text).
let project_text = save_project(&circuit);
assert!(!project_text.is_empty());

// Or export a bill of materials.
let bom_csv = to_bom_csv(&circuit);
println!("{bom_csv}");
```

Signals coming back from the simulator export the same way with
`to_csv(&signals)` and `to_vcd(&signals)`.

---

Part of [**ohmforge**](https://github.com/ohmforge/ohmforge) — the AI-native
circuit & firmware workbench.

License: **MIT OR Apache-2.0**.
