# Common-emitter BJT amplifier (DC operating point)

```
        Vcc 12 V
         │
    ┌────┴────┐
 [ R1 47k ] [ RC 2k2 ]
    │          │
   base ──────┤ C          vc (collector output)
    │         Q1 (NPN)
 [ R2 10k ]   │ E
    │        emit
   gnd      [ RE 1k ]
               │
              gnd
```

> **Runnable from the shell.** A bipolar transistor is a 3-terminal device
> ([`ohmforge_core::graph::PartKind::Bjt`], Ebers–Moll), placed with
> `add Q1 <collector> <base> <emitter> [npn|pnp]`. So this circuit runs directly as
> a script:
>
> ```sh
> cat examples/analog/common_emitter_bjt.eesh | cargo run -p ohmforge --bin eesh
> ```
>
> The same bias is also the built-in `cargo run -p ohmforge amp` demo, and is
> exercised by the `bjt_common_emitter_bias` and `bjt_ce_small_signal_gain`
> regression tests in `ohmforge-sim`.

## The circuit

A textbook voltage-divider-biased common-emitter stage: `R1`/`R2` set the base
bias, `RC` is the collector load, and `RE` is the emitter degeneration resistor.
`Q1` is an NPN with the default Ebers–Moll model (`β_F = 100`, `I_s = 1e-14 A`).

## The physics

The divider holds the base near `Vcc·R2/(R1+R2) ≈ 2.1 V`. Subtracting a ~0.66 V
base-emitter drop puts the emitter at `Ve ≈ 1.3 V`, so the emitter (≈ collector)
current is

```
Ic ≈ Ie = Ve / RE ≈ 1.33 V / 1k ≈ 1.33 mA
Vc  = Vcc − Ic·RC ≈ 12 − 1.33m·2k2 ≈ 9.1 V
```

landing the collector mid-rail — the signature of a transistor biased in the
**forward-active** region, where it can amplify. The engine solves this by
Newton–Raphson over the full 3×3 Ebers–Moll Jacobian (two junction diodes plus
the transport source), with the gmin / source-stepping convergence aids available
if the cold-start guess is too far off.

## Expected result (verified — `cargo run -p ohmforge amp`)

```
  Vb = 2.00 V   Vc = 9.09 V   Ve = 1.33 V   →   Vbe = 0.662 V, Ic ≈ 1.33 mA (active region)
```

`Vbe = 0.662 V` sits squarely in the silicon window, and `Vc` (9.09 V) is between
the emitter and the rail — forward-active, not saturated (`Vc > Vb`) nor cut off.
A small signal injected at the base is amplified by roughly `−RC/RE ≈ −2.2` at the
collector (with the emitter resistor un-bypassed); bypass `RE` for the full
`−g_m·RC`, as the `bjt_ce_small_signal_gain` AC test demonstrates.
