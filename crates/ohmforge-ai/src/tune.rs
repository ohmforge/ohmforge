//! AI auto-tune (`CP-AF-M6.2`) — a closed-loop component-value optimizer.
//!
//! Where the [`verify`](fn@crate::verify) loop *checks* one proposed change, this
//! module *searches* for the component value that makes a measured spec pass. It
//! is the frontier piece that closes a loop around the verify pass: it picks a
//! candidate value, drives it **through the existing verify path** (clone the
//! base shell, scratch-apply, re-simulate, measure), reads the real measured
//! number back, and adjusts — a deterministic, gradient-free **bisection** that
//! needs no model, no API key, and no network.
//!
//! ```text
//! pick a candidate value  → stage a patch that re-adds the part at that value
//!                         → verify() clones the base, applies, solves, measures
//!                         → read the measured spec quantity
//!                         → the measurement moved toward/past the target → bisect
//!                         → converge when the spec criterion passes
//! ```
//!
//! The live circuit is never touched: every candidate rides the same
//! clone-and-measure machinery as [`verify`](fn@crate::verify), which borrows the base shell
//! immutably. The search is a closed-form bisection over `f(value) = measured −
//! target`, so for a monotone measurement (a divider's output versus a divider
//! resistor, say) it converges, and — being pure arithmetic over a deterministic
//! simulator — it lands on the identical value every run.

use ohmforge_shell::Shell;

use crate::error::AiError;
use crate::verify::{verify, Criterion, Spec, Verdict};
use crate::ProposedPatch;

/// A closed-loop tuning request: which two-terminal part to adjust, the measured
/// goal to hit, and the value range to search.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoTune {
    /// The part whose value the search adjusts, e.g. `"R2"`. At each candidate it
    /// is removed and re-added, so it must be a two-terminal part whose unit the
    /// shell infers from its reference letter (`R`/`C`/`L`/`V`/`I`).
    pub reference: String,
    /// The two node names the part bridges, as named in the base shell (e.g.
    /// `["out", "gnd"]`) — used to re-add it at each candidate value.
    pub nodes: [String; 2],
    /// The measured goal. The search converges when this criterion passes
    /// (`|measured − target| <= tolerance`) on a real operating-point solve.
    pub criterion: Criterion,
    /// A one-line label for the goal, echoed into the final [`Verdict`].
    pub goal: String,
    /// The inclusive lower bound of the value search (same unit as the part).
    pub lo: f64,
    /// The inclusive upper bound of the value search.
    pub hi: f64,
    /// A hard cap on bisection steps so the search always terminates.
    pub max_iter: usize,
}

impl AutoTune {
    /// A tune that drives a node voltage to `target ± tolerance` volts by adjusting
    /// `reference` (bridging `nodes`) over `[lo, hi]`.
    pub fn node_volts(
        reference: impl Into<String>,
        nodes: [impl Into<String>; 2],
        node: impl Into<String>,
        target: f64,
        tolerance: f64,
        lo: f64,
        hi: f64,
    ) -> Self {
        let node = node.into();
        let [a, b] = nodes;
        AutoTune {
            reference: reference.into(),
            nodes: [a.into(), b.into()],
            criterion: Criterion::node_volts(node.clone(), target, tolerance),
            goal: format!("v({node}) = {target} V (±{tolerance}) by tuning"),
            lo,
            hi,
            max_iter: 100,
        }
    }
}

/// What a converged [`auto_tune`] yields: the value it landed on, the quantity
/// measured there, how many candidate solves it took, and the passing verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct TuneResult {
    /// The converged component value (the number to `set` the part to).
    pub value: f64,
    /// The spec quantity measured at `value`, on a real solve.
    pub measured: f64,
    /// How many candidate circuits were solved while searching.
    pub evaluations: usize,
    /// The passing [`Verdict`] from the final verify at `value`.
    pub verdict: Verdict,
}

/// Search for the value of `tune.reference` that makes `tune.criterion` pass,
/// driving every candidate through the [`verify`](fn@crate::verify) clone-and-measure path.
///
/// `base` is borrowed immutably — the live circuit is never mutated; each
/// candidate is measured on a fresh clone. The search is a deterministic
/// bisection over `[tune.lo, tune.hi]`: it brackets the target between the two
/// bounds (requiring the measurement to sit on opposite sides of the target
/// there, or already pass at one bound) and halves the interval until the
/// criterion passes.
///
/// Returns [`AiError::Patch`] if the bounds are ill-formed, the target is not
/// bracketed by `[lo, hi]`, a candidate could not be simulated/measured, or the
/// search did not converge within `tune.max_iter` steps.
pub fn auto_tune(base: &Shell, tune: &AutoTune) -> Result<TuneResult, AiError> {
    if !(tune.lo.is_finite() && tune.hi.is_finite()) || tune.lo >= tune.hi {
        return Err(AiError::Patch(format!(
            "auto-tune bounds [{}, {}] are not a finite ascending range",
            tune.lo, tune.hi
        )));
    }
    if tune.max_iter == 0 {
        return Err(AiError::Patch(
            "auto-tune needs at least one iteration".to_string(),
        ));
    }

    let target = tune.criterion.target;
    let mut evaluations = 0usize;
    // Measure the spec quantity with the part set to `value`, through verify().
    let mut eval = |value: f64| -> Result<(f64, Verdict), AiError> {
        evaluations += 1;
        let patch = candidate_patch(tune, value);
        let spec = Spec::new(tune.goal.clone(), vec![tune.criterion.clone()]);
        let verdict = verify(base, &patch, &spec)?;
        // The spec has exactly one criterion, so there is always one outcome.
        let measured = verdict
            .outcomes
            .first()
            .map(|o| o.measured)
            .ok_or_else(|| AiError::Patch("auto-tune spec produced no outcome".to_string()))?;
        Ok((measured, verdict))
    };

    // Bracket the target with the two bounds. `f(v) = measured(v) − target`.
    let (mut lo, mut hi) = (tune.lo, tune.hi);
    let (m_lo, v_lo) = eval(lo)?;
    if v_lo.pass {
        return Ok(TuneResult {
            value: lo,
            measured: m_lo,
            evaluations,
            verdict: v_lo,
        });
    }
    let (m_hi, v_hi) = eval(hi)?;
    if v_hi.pass {
        return Ok(TuneResult {
            value: hi,
            measured: m_hi,
            evaluations,
            verdict: v_hi,
        });
    }
    let (mut f_lo, f_hi) = (m_lo - target, m_hi - target);
    if (f_lo > 0.0) == (f_hi > 0.0) {
        return Err(AiError::Patch(format!(
            "auto-tune cannot bracket the target: at {lo} the measurement is {m_lo} and at {hi} \
             it is {m_hi}, both on the same side of {target} — widen the bounds"
        )));
    }

    // Bisect: keep the half whose endpoints straddle the target.
    for _ in 0..tune.max_iter {
        let mid = 0.5 * (lo + hi);
        let (measured, verdict) = eval(mid)?;
        if verdict.pass {
            return Ok(TuneResult {
                value: mid,
                measured,
                evaluations,
                verdict,
            });
        }
        let f_mid = measured - target;
        if (f_mid > 0.0) == (f_lo > 0.0) {
            lo = mid;
            f_lo = f_mid;
        } else {
            hi = mid;
        }
    }

    Err(AiError::Patch(format!(
        "auto-tune did not converge within {} iterations (last interval [{lo}, {hi}])",
        tune.max_iter
    )))
}

/// The staged patch that measures `tune.reference` at `value`: remove the part,
/// then re-add it at the candidate value. Verify applies this to a *clone* of the
/// base shell, so the removal/re-add never touches the live circuit.
fn candidate_patch(tune: &AutoTune, value: f64) -> ProposedPatch {
    let [a, b] = &tune.nodes;
    ProposedPatch {
        summary: format!("try {} = {value}", tune.reference),
        commands: vec![
            format!("remove {}", tune.reference),
            format!("add {} {a} {b} {value}", tune.reference),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 5 V rail into a resistive divider: `R1` fixed at 1 kΩ from `in` to `out`,
    /// and `R2` (the knob) from `out` to ground, starting at a wrong 100 Ω. At
    /// 5 V, `v(out) = 5·R2/(R1+R2)`, so 3.3 V wants `R2 ≈ 1941 Ω`.
    fn divider_shell() -> Shell {
        let mut sh = Shell::new();
        sh.execute("add V1 in gnd 5").unwrap();
        sh.execute("add R1 in out 1000").unwrap();
        sh.execute("add R2 out gnd 100").unwrap();
        sh
    }

    fn tune_r2() -> AutoTune {
        AutoTune::node_volts("R2", ["out", "gnd"], "out", 3.3, 1e-4, 1.0, 1e6)
    }

    /// The M6.2 gate: starting from a wrong value, the tuner lands within a tight
    /// tolerance of the 3.3 V target measured by a REAL sim, it is deterministic
    /// (same result every run), and it needs no key/network.
    #[test]
    fn auto_tune_converges_on_the_divider_target() {
        let base = divider_shell();
        let tune = tune_r2();

        let result = auto_tune(&base, &tune).expect("auto-tune converges");

        // Landed within the spec tolerance of 3.3 V, verified on a real solve.
        assert!(result.verdict.pass, "{}", result.verdict.summary);
        assert!(
            (result.measured - 3.3).abs() <= 1e-4,
            "measured v(out) = {}",
            result.measured
        );
        // The value is the analytic R2 = 3.3·1000 / (5 − 3.3) ≈ 1941.18 Ω.
        let expected = 3.3 * 1000.0 / (5.0 - 3.3);
        assert!(
            (result.value - expected).abs() < 1.0,
            "tuned R2 = {} (expected ≈ {expected})",
            result.value
        );
        assert!(result.evaluations > 2, "it actually searched");

        // The base shell was never mutated — R2 is still the original 100 Ω.
        let r2 = base.circuit().part_by_ref("R2").unwrap();
        assert_eq!(r2.value.unwrap().magnitude, 100.0, "base left untouched");

        // Deterministic: a second run yields the identical result.
        let again = auto_tune(&base, &tune).expect("auto-tune converges again");
        assert_eq!(result, again, "auto-tune must be deterministic");
    }

    /// A target the bounds cannot reach is a clean bracketing error, not a bogus
    /// value — 4.9 V is impossible on this divider for any R2 ≤ 1 MΩ with a 5 V
    /// rail only when the bound is too low; here we pin the upper bound so the
    /// output can never climb to the target, so the two bounds sit on the same
    /// side of it.
    #[test]
    fn auto_tune_reports_an_unreachable_target() {
        let base = divider_shell();
        // Ask for 3.3 V but cap R2 at 10 Ω: v(out) stays far below 3.3 V across
        // the whole [1, 10] range, so the target is never bracketed.
        let tune = AutoTune::node_volts("R2", ["out", "gnd"], "out", 3.3, 1e-4, 1.0, 10.0);
        let err = auto_tune(&base, &tune).unwrap_err();
        assert!(matches!(err, AiError::Patch(_)), "{err}");
        assert!(err.to_string().contains("bracket"), "{err}");
    }

    /// Ill-formed bounds are rejected up front, before any solve.
    #[test]
    fn auto_tune_rejects_bad_bounds() {
        let base = divider_shell();
        let mut tune = tune_r2();
        tune.lo = 1e6;
        tune.hi = 1.0;
        assert!(matches!(auto_tune(&base, &tune), Err(AiError::Patch(_))));
        let mut tune = tune_r2();
        tune.max_iter = 0;
        assert!(matches!(auto_tune(&base, &tune), Err(AiError::Patch(_))));
    }
}
