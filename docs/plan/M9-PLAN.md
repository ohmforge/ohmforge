# M9 plan — cool features: power, sensors, RF, visual capture

M0–M8 complete (99/99, v0.7.0, published as **ohmforge**). M9 is the "cool
features" milestone — four crowd-pleasing frontiers, one per audience:

- **Power electronics** — switch-mode supplies (buck/boost) with PWM switching.
- **Sensors & actuators** — thermistor / photoresistor / pot / DC motor / servo / stepper.
- **RF & signal integrity** — 2-port S-parameters + a Smith chart and eye diagram.
- **Visual schematic capture** — draw circuits with the mouse, in the TUI and the browser.

Standing rules hold: `ohmforge-core`/`ohmforge-sim` stay dependency-free +
`wasm32`-clean + deterministic; the golden/determinism suite stays bit-for-bit;
**no new data-carrying `PartKind`/`Waveform` variant cascades** — new
multi-terminal/active elements are added as **circuit-level relations** (the M6
`Circuit::couple` / M7 `Circuit::add_line` pattern). Crate ownership is disjoint
so the tracks don't collide: sim-core owns `-core`+`-sim`, boards owns
`-devices`+`-mcu`, ui-shell owns `-tui`+`-shell`+`-instruments`, web owns `-web`.

---

## Checkpoints (new)

### sim-core — power electronics + RF (owns `ohmforge-core`, `ohmforge-sim`)
- **CP-SIM-M9.1** **Power electronics**: a controlled switching element (voltage/clock-controlled, added as a **circuit-level relation** — no new `PartKind`) driven by a PWM stimulus (reuse/extend the existing `Pulse` waveform for duty/period), so a **buck** and a **boost** converter simulate in transient. *Gate:* a buck converter steps an input down to the duty-set output (e.g. 12 V → ~5 V) and a boost steps up, with sane switching-node + inductor-current waveforms; converters with no switch are unaffected and the golden suite stays bit-for-bit.
- **CP-SIM-M9.2** **2-port S-parameters**: `Engine::s_parameters(circuit, port1, port2, z0)` returning S11/S21/S12/S22 (and the Z/Y 2-port matrix) from the AC solve, referenced to `Z0`. *Gate:* a matched line / resistive attenuator / L-match returns S-parameters matching closed-form theory (a `Z0`-matched 2-port → S11 ≈ 0, |S21| as predicted). Purely additive API.

### boards — sensors & actuators (owns `ohmforge-devices`, `ohmforge-mcu`)
- **CP-BRD-M9.1** **Sensor & actuator models**, built as `ohmforge-devices` subcircuits/models over existing primitives (the M5 op-amp/switch/relay pattern — **do not edit `ohmforge-core`/`-sim`**): NTC **thermistor** (temperature→resistance, β/Steinhart), **photoresistor** (illuminance→resistance), **potentiometer** (wiper 0..1), **DC motor** (back-EMF + mechanical time constant), **servo** (pulse-width→angle), **stepper** (step→position). Wire at least one through the MCU co-sim bridge (e.g. a servo driven by an MCU PWM pin). *Gate:* a thermistor divider's node voltage shifts with set temperature as the β-model predicts; a 1.5 ms servo pulse maps to ~90°; a DC motor spins up to steady-state speed; a pot wiper divides linearly.

### ui-shell — visual capture + RF instruments (owns `ohmforge-tui`, `ohmforge-shell`, `ohmforge-instruments`)
- **CP-UX-M9.1** **Visual schematic capture in the TUI**: mouse-driven place / move / wire parts on a grid canvas that drives the **real `Circuit`** through the shared mutation path (the CP-UX-M5.1/M8 `EditAction` grammar). Pure, testable state machine — no tty. *Gate:* mouse events place parts and draw a wire, asserted on the resulting `Circuit`; a drag moves a part; all on data, no tty.
- **CP-UX-M9.2** **RF instrument panels** in `ohmforge-instruments`: a **Smith chart** (plot a reflection coefficient Γ / S11 on the Smith grid) and an **eye diagram** (fold a bitstream/periodic signal). Render from provided S-parameter/Γ and signal data so the panels don't block on the sim-core API (wiring them to `Engine::s_parameters` is a thin follow-up). *Gate:* a known Γ lands at the correct Smith coordinate (e.g. Γ=0 at center, open/short at the rim extremes); an eye diagram folds a known bit pattern into the expected eye.

### web — visual capture in the browser (owns `ohmforge-web`) — *starts after the home example-gallery lands*
- **CP-WEB.8** **Drag-and-drop schematic canvas** in the browser: place and wire parts visually (not just the parts form), round-tripping to the netlist and re-simulating. *Gate:* host-testable canvas-edit → netlist logic round-trips and the page builds + simulates a circuit drawn on the canvas.

### platform — ship
- **CP-SHIP.5** Polish & tag **v0.8.0**: full CI-parity gate sweep (incl. `http`/`relay` jobs), README refreshed to M0–M9, tag.

---

## Fan-out
sim-core, boards, and ui-shell launch concurrently in their worktrees (reset to
the current `main`). The **web** track launches once the home example-gallery
work is committed (both touch `ohmforge-web`). Merge + gate each as it lands;
`CP-SHIP.5` tags **v0.8.0** when M9 closes.

## Deferred / out of scope
Closed-loop SMPS control-law design (we ship the switching plant + PWM, not an
auto-compensator), full electro-thermal coupling, and a hosted schematic-capture
collaboration layer.
