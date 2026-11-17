# ohmforge-instruments — virtual bench instruments

Virtual instruments for [ohmforge](https://github.com/ohmforge/ohmforge),
layered over the engines and presented as front panels in the TUI:
oscilloscope, multimeter (DMM), function generator, bench PSU, logic analyzer
(with I2C/SPI/UART decode), and Bode / FFT / spectrum views. A dataflow
**patcher** composes these into custom virtual benches.

Instruments *consume* `Signal`s from the simulator and *control* sources; they
hold no solver state of their own, so they sit purely above the compute core.

## Usage

```rust
use ohmforge_core::signal::Signal;
use ohmforge_core::units::Unit;
use ohmforge_instruments::{measure, DmmMode};

// A short transient waveform coming back from the simulator.
let t = vec![0.0, 1e-3, 2e-3, 3e-3];
let y = vec![0.0, 1.0, 0.0, -1.0];
let sig = Signal::time("v(out)", Unit::Volt, t, y);

// Read it on the multimeter.
let reading = measure(&sig);
let (volts, unit) = reading.value(DmmMode::Rms);
println!("RMS = {volts:.3} {unit}");
```

Richer panels build the same way — e.g. `Oscilloscope::new("CH1").channel(sig)`
for a rendered scope trace, or `Multimeter::new("DMM").mode(DmmMode::DcVolts)`.

---

Part of [**ohmforge**](https://github.com/ohmforge/ohmforge) — the AI-native
circuit & firmware workbench.

License: **MIT OR Apache-2.0**.
