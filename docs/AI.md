# The AI copilot (`ohmforge-ai`)

The copilot's job is to make EE *approachable* without being a black box. Its guiding rule: **the AI works through the same API you do, and verifies its own claims by running the simulator.** No hand-waving — proposals are simulated before they're presented, and you always see and approve changes.

## Capabilities

| Mode | Prompt shape | Output |
|---|---|---|
| **Ask** | "why is Q1 saturated?" | Explanation grounded in the *current* sim results (real node voltages/currents, not guesses). |
| **Design** | "5V→3.3V, 500 mA, low ripple" | A netlist patch (parts + wiring + values) you can inspect, simulate, and apply/reject. |
| **Debug** | "the LED won't light" | Reads sim output, localizes the fault, proposes a fix, re-simulates to confirm. |
| **Firmware** | "fade the LED with PWM on the Uno" | An Arduino sketch, compiled into the emulator and co-simulated against your circuit. |
| **Explain / tutor** | "teach me how this filter works" | Step-through lesson over the live circuit, with suggested experiments. |

## How it works — tool-using and verified

The model is given **tools** that map onto the same `ohmforge-core`/`ohmforge-sim` API the shell uses. It doesn't emit prose that a human must transcribe; it *acts*, observes results, and iterates.

```
add_part(type, ref, value, nodes)      run_transient(duration)
wire(a, b)                             run_op() / run_ac(...) / sweep(...)
set_param(ref, param, value)           read_signal(sig)  →  numbers
remove(ref)                            measure(fn, sig)  →  value+units
list_parts() / show(ref|node)          board_add(model) / flash(fw)
propose_patch(...)  →  staged, needs   read_regs(board) / read_pins(board)
                       user approval
```

**The verification loop** (what keeps it honest):

```
1. model proposes a change via propose_patch(...)     # staged, not yet applied
2. ohmforge-ai applies it to a scratch copy of the project
3. ohmforge-sim runs the relevant analysis
4. model reads the real results (read_signal/measure)
5. if the result misses the spec → iterate (back to 1)
   if it meets the spec         → present the diff + the sim evidence
6. user runs `apply` or `reject`
```

So a "design a 3.3V regulator" answer arrives with a Bode/transient plot and measured output voltage/ripple from an *actual simulation of the proposed circuit* — not a plausible-looking schematic that may or may not work.

## Guardrails

- **Nothing is applied without approval.** Design/debug output is a **staged patch**; `apply`/`reject` is always the user's call. The TUI shows the diff.
- **Grounded, not guessed.** Explanations cite real measured values from the current run. If the model wants to claim "V(out)=3.28 V," it must have read it.
- **Scoped tools.** The AI can build/simulate/read; it can't touch the filesystem, network, or shell out to toolchains except through explicit, user-visible actions (e.g. `flash`, which the user sees).
- **Transparency.** Every tool call the AI makes is logged in the session transcript and is itself replayable/undoable.

## Provider & model

- Uses the **Claude API** (Messages API with tool use). Default to the latest capable models; pin a config-selectable model id. Keep the tool schema small and typed so calls are cheap and reliable.
- **Streaming** responses into the AI panel; tool calls render as they happen ("added R1… running transient… reading V(out)…").
- **Prompt caching** for the static system prompt + tool schema + parts-index context to cut cost/latency on multi-turn sessions.
- API key via env/config; **never** hard-coded. No telemetry.

## RAG / knowledge

- A local **parts & datasheet index** (device parameters, typical values, pinouts, common circuits) is retrieved and injected as context so the model reasons over *real* part behavior and this project's actual state, not just its training prior.
- The index is versioned and shippable, which also powers **offline mode**.

## Offline mode

When no API key is present, a reduced copilot still helps via **deterministic heuristics + the local index**: value calculators (Ohm/divider/RC corner/current-limit resistor), rule-based debug checks (floating nodes, no ground reference, source shorted, LED with no series resistor), and canned explanations for indexed parts/circuits. It degrades gracefully rather than disappearing.

## Why this design

Tinkerers get burned by confident-but-wrong AI. Binding the copilot to a **deterministic simulator it must consult and re-run** turns "sounds right" into "measured right," and keeps the human in the loop for every change. That's the whole point: friendly *and* trustworthy.
