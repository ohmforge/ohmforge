# M8 plan — hardening & reach

M0–M7 are complete (91/91, v0.5.0). M8 is **not** new frontier — it is the
refinement pass after real hands-on use surfaced that the product "feels
glitchy." A four-track read-only QA audit (TUI, shell, AI, sim/render) found
**zero crashes** and confirmed the core numerics, renderers, and format
round-trips are correct — so M8 fixes a specific set of confirmed defects and
closes the big **reachability gaps** that make most of the engine and the whole
AI copilot unreachable from what a user actually types.

Standing rules hold: `ohmforge-core`/`ohmforge-sim` stay dependency-free + `wasm32`-clean +
deterministic; the golden/determinism suite stays **bit-for-bit**; new deps stay
behind opt-in features. No new `PartKind` cascades. Every fix ships with a
regression test that fails before and passes after.

---

## Checkpoints (new)

### sim-core — unblock the well-posed analyses
- **CP-SIM-M8.1** Linear **AC / noise / `.TF` / DC-sweep must solve circuits with a
  DC-floating node** (capacitive divider, AC-coupled subnode, open-ended line) —
  today they spuriously error `Singular` because they gate on a full DC
  operating point. Fix: skip the DC-bias gate for circuits with no nonlinear
  devices, and/or carry a residual SPICE-style `GMIN` into the bias solve so a
  reactance-only node is conditioned. Also guard the lossless-line transient
  against a timestep coarser than the line delay (warn/clamp, documented).
  *Gate:* a 1 nF–1 nF capacitive divider's AC gives `v(mid)=0.5`; a source-matched
  open-ended line solves in AC; **the golden/determinism suite stays bit-for-bit.**

### ui-shell — TUI hardening, shell reach, instrument rendering
- **CP-UX-M8.1** **TUI hardening.** Kill the flicker (stop the full-screen
  `Clear(All)` + repaint every 120 ms tick — diff frames / only redraw on change;
  don't tick-animate an idle screen). Separate playback from the live overlay (a
  scrubbing flag — DC op-point shows normally; re-solve/clear playback on mutate
  or probe-less run) so live-tune feedback works again. Size the scope plot to its
  pane (no fixed 72×16 truncation). Give edit-mode chrome precedence over the
  overlay. Add an input caret; let clicks dismiss/interact with the palette.
  *Gate:* tests prove no idle redraw churn, the overlay reflects a tune while a
  transient is loaded, and the scope renders within an 80×24 pane without clipping
  the axis.
- **CP-UX-M8.2** **Shell reach + correctness.** Make `run` reach the engine:
  `run ac`, `run dc`, `run noise`, `run mc`, `run tf` (+ keep `tran`/`op`). Allow
  **multi-terminal parts** (BJT `Q`/MOSFET `M` 3-terminal, controlled sources) and
  a **waveform verb** (`set <ref> sine …`/`pulse …`) plus tolerance/tempco/temp so
  AC/Bode/Monte-Carlo are actually drivable. Fix the **O(N²) undo** (bounded/diff
  snapshots; cap/stream `repeat` expansion). Correct the misleading
  `expected R/C/L/V` message (I and D work; Q/M now too), the dup-`probe` undo
  no-op, duration validation (reject ≤0, warn on absurd), degenerate `add`
  (same node), tab-completion (part refs + checkpoint names), and the `help`
  text. *Gate:* tests drive `run ac`/`run dc`/`run tf`, place and solve a BJT
  amplifier from eesh, a 100k-part script completes quickly, and the error/edge
  cases return clean typed errors.
- **CP-UX-M8.3** **Instrument rendering.** FFT channel must reject or resample a
  non-uniform (adaptive-transient) grid instead of smearing the axis; the Bode
  `−3 dB` corner must reference the passband peak (correct for high-/band-pass),
  not the lowest bin; the oscilloscope axis width math must count chars not bytes
  (`µs`/`Ω` labels align). *Gate:* tests on a non-uniform grid, a high-pass Bode,
  and a `µs` timebase.

### ai-fmt — copilot correctness & robustness
- **CP-AF-M8.1** **AI correctness.** The LED-without-series-resistor DRC must
  require a resistor on the LED's actual conduction path (exclude `GROUND` from
  the shared-net proxy; reject a merely-parallel resistor). `no_path_to_ground`
  must treat a capacitor as a DC open (not a conductive bridge). The debug-mode
  fault ranking should use a log-symmetric correction metric. On tool-loop cap,
  return the partial transcript/text instead of discarding it. The response
  parser must tolerate unknown content blocks (e.g. `thinking`) rather than
  hard-erroring. *Gate:* regression tests — an unprotected LED over a shared
  ground net is flagged; an AC-coupled-only node is reported floating; the parser
  survives a `thinking` block.
- **CP-AF-M8.2** **Live-transport robustness + honest docs.** Add a request +
  connect timeout to the `http` client so a stalled server can't hang the CLI.
  Fix the stale guide/crate docs that call the transport a non-sending stub (the
  native path is live). *Gate:* a timeout test (unresponsive local socket →
  bounded `AiError::Transport`, no hang); docs updated.

### platform — make it reachable, then ship
- **CP-PLAT.6** **Wire the copilot + analyses into the `of` binary** (done on
  `main` after the track fixes merge): `of ask`, `of review`/`of drc` (offline, no
  key), `ee design`/`ee debug`/`ee tune`, defaulting to offline mode when
  `ANTHROPIC_API_KEY` is absent; ensure the new `run` analyses are reachable
  end-to-end. Refresh the guides. *Gate:* `of ask …` (offline) runs with no key
  and returns a grounded answer; `of` help lists the AI subcommands.
- **CP-SHIP.4** Polish & tag **v0.6.0**: full CI-parity gate sweep (incl. `http`
  and `relay` feature jobs), refresh README to M0–M8, tag.

---

## Fan-out
One agent per track (sim-core, ui-shell, ai-fmt), concurrently in their
worktrees. The integrator wires `CP-PLAT.6` (the `of` binary CLI) on `main`
during reconcile, then gates and tags **v0.6.0** (`CP-SHIP.4`). Boards and web
had no audit findings — no agents there.

## Explicitly out of scope (deferred, not forgotten)
The FFT amplitude normalization/window-gain correction (peak frequency is
already correct), and vendor-SPICE `.model` parameter fidelity — tracked but not
part of this pass.
