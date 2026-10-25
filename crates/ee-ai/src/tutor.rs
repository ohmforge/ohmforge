//! Tutor mode (`CP-AF-M3.3`) — guided lessons, graded challenges, and a glossary.
//!
//! This is the teaching surface of the copilot. Its grading is **deterministic
//! and offline**: a challenge is scored by running the learner's circuit through
//! the *real* simulator and measuring it, reusing the M2 verification-loop
//! machinery ([`grade`] over a [`Spec`]) — no model, no
//! network, no key. A tutor *may* layer the model on top for natural-language
//! coaching ([`coach`]), but a pass/fail verdict never depends on it.
//!
//! Three capabilities:
//!
//! * **Graded challenges** ([`Challenge`]) — a brief plus a [`Spec`]. The learner
//!   builds a circuit in a [`Shell`]; [`Challenge::grade`] solves it and returns a
//!   [`Verdict`] that is PASS against a correct circuit and FAIL **naming the
//!   miss** against a wrong one, citing the measured numbers from a real solve.
//! * **Guided lessons** ([`Lesson`]) — an ordered list of [`Step`]s, each a piece
//!   of prose and an optional [`StepGoal`] (read-only, a part must be present, or
//!   the circuit must meet a measured spec). A [`LessonRunner`] walks the steps
//!   and advances when the learner's circuit satisfies the current step's goal.
//! * **Glossary** ([`define`] / [`glossary`] / [`search_terms`]) — a small static
//!   term → explanation lookup, the contextual "what does this word mean?" path.
//!
//! Built-in content ([`Challenge::divider_3v3_from_5v`], [`Lesson::first_divider`])
//! makes the module useful out of the box and is what the checkpoint gate drives.

use ee_shell::Shell;

use crate::client::Client;
use crate::error::AiError;
use crate::message::SystemBlock;
use crate::transport::Transport;
use crate::verify::{grade, Criterion, Spec, Verdict};

// ── Glossary ───────────────────────────────────────────────────────────────

/// One glossary entry: a term and its one-line explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Term {
    /// The canonical term, e.g. `"voltage divider"`.
    pub term: &'static str,
    /// A short, plain-language explanation.
    pub definition: &'static str,
}

static GLOSSARY: &[Term] = &[
    Term {
        term: "Ohm's law",
        definition: "V = I·R: the voltage across a resistor equals its current times its resistance.",
    },
    Term {
        term: "voltage divider",
        definition: "Two resistors in series across a source; the midpoint sits at Vin·R_bottom/(R_top+R_bottom).",
    },
    Term {
        term: "node",
        definition: "An electrically common point where part terminals meet — every pin on a node is at the same voltage.",
    },
    Term {
        term: "ground",
        definition: "The 0 V reference node every other voltage is measured against (net 0 in ee-assist).",
    },
    Term {
        term: "operating point",
        definition: "The steady DC solution of a circuit: every node voltage and branch current with all sources held constant.",
    },
    Term {
        term: "series",
        definition: "Parts sharing a single path so the same current flows through each; series resistances add.",
    },
    Term {
        term: "parallel",
        definition: "Parts sharing both nodes so the same voltage is across each; parallel resistances combine as 1/Σ(1/R).",
    },
    Term {
        term: "pull-up resistor",
        definition: "A resistor to the supply that holds a line high when nothing else is driving it.",
    },
    Term {
        term: "decoupling capacitor",
        definition: "A small capacitor across a chip's supply pins that supplies transient current and steadies the rail.",
    },
    Term {
        term: "time constant",
        definition: "τ = R·C (or L/R): the time an RC/RL circuit takes to move ~63% of the way to its final value.",
    },
    Term {
        term: "diode",
        definition: "A one-way valve for current: it conducts when forward-biased past ~0.6 V and blocks the other way.",
    },
    Term {
        term: "LED",
        definition: "A diode that emits light when forward current flows; it needs a series resistor to limit that current.",
    },
    Term {
        term: "load",
        definition: "Whatever draws current from a circuit's output — modeled here as the resistance across the output node.",
    },
    Term {
        term: "tolerance",
        definition: "How far a real component's value may stray from nominal, e.g. a ±5% resistor.",
    },
];

/// Look a term up in the glossary, case-insensitively. Returns its explanation,
/// or `None` if the term is not catalogued.
pub fn define(term: &str) -> Option<&'static str> {
    let needle = term.trim();
    GLOSSARY
        .iter()
        .find(|t| t.term.eq_ignore_ascii_case(needle))
        .map(|t| t.definition)
}

/// The whole glossary, in a stable order — for a caller that wants to render it.
pub fn glossary() -> &'static [Term] {
    GLOSSARY
}

/// Contextual lookup: every glossary entry whose term or definition contains
/// `query` (case-insensitively), best-match (term hit) first, then alphabetical.
/// A blank query returns nothing.
pub fn search_terms(query: &str) -> Vec<&'static Term> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<&'static Term> = GLOSSARY
        .iter()
        .filter(|t| {
            t.term.to_ascii_lowercase().contains(&needle)
                || t.definition.to_ascii_lowercase().contains(&needle)
        })
        .collect();
    // Term matches rank before definition-only matches; ties break on the term.
    hits.sort_by(|a, b| {
        let a_in_term = a.term.to_ascii_lowercase().contains(&needle);
        let b_in_term = b.term.to_ascii_lowercase().contains(&needle);
        b_in_term.cmp(&a_in_term).then_with(|| a.term.cmp(b.term))
    });
    hits
}

// ── Graded challenges ────────────────────────────────────────────────────────

/// A graded challenge: a brief for the learner and the measurable [`Spec`] the
/// circuit they build must satisfy. Grading is a real simulation, not a rubric —
/// see [`Challenge::grade`].
#[derive(Debug, Clone, PartialEq)]
pub struct Challenge {
    /// A short title, e.g. `"3.3 V divider"`.
    pub title: String,
    /// What the learner must build, in plain language.
    pub brief: String,
    /// The measured criteria the built circuit must meet to pass.
    pub spec: Spec,
}

impl Challenge {
    /// A challenge with the given title, brief, and spec.
    pub fn new(title: impl Into<String>, brief: impl Into<String>, spec: Spec) -> Self {
        Challenge {
            title: title.into(),
            brief: brief.into(),
            spec,
        }
    }

    /// Grade a learner's `built` circuit by running it through the **real**
    /// simulator and measuring every spec criterion. Returns a [`Verdict`] that
    /// passes only when all criteria are met, and whose summary names each miss
    /// with the measured-vs-target gap. Deterministic and offline: no model is
    /// consulted. The learner's shell is cloned, never mutated.
    ///
    /// Errors (as [`AiError::Patch`]) only if the spec names a node/part the
    /// built circuit lacks, or the solve itself fails.
    pub fn grade(&self, built: &Shell) -> Result<Verdict, AiError> {
        grade(built, &self.spec)
    }

    /// The built-in gate challenge: turn the 5 V rail on node `in` into 3.3 V on
    /// node `out`, within ±50 mV. A correct unloaded divider (e.g. 1.7 kΩ over
    /// 3.3 kΩ) passes; any divider that lands elsewhere fails, named.
    pub fn divider_3v3_from_5v() -> Self {
        Challenge::new(
            "3.3 V divider",
            "Starting from the 5 V source V1 on node `in`, build a resistor divider \
             so node `out` sits at 3.3 V (±50 mV).",
            Spec::new(
                "5 V -> 3.3 V divider at `out`",
                vec![Criterion::node_volts("out", 3.3, 0.05)],
            ),
        )
    }
}

/// Optional model-backed coaching. The **grade is always deterministic** (sim +
/// rules via [`Challenge::grade`]); this only turns a verdict into a
/// natural-language nudge and so needs a live model through `client`.
///
/// It advertises no tools — the model is asked for prose alone — so a stubbed
/// [`MockTransport`](crate::transport::MockTransport) returning one text turn
/// drives it in tests with no network.
pub fn coach<T: Transport>(
    client: &Client<T>,
    challenge: &Challenge,
    verdict: &Verdict,
) -> Result<String, AiError> {
    let system = [SystemBlock::new(
        "You are an encouraging electronics tutor. The learner's attempt has already been \
         graded by a real simulator; you are given the measured verdict. In one or two \
         sentences, nudge them toward the fix WITHOUT handing over the full answer.",
    )];
    let prompt = format!(
        "Challenge: {}\n{}\n\nTheir latest attempt measured:\n{}\n\nGive a short hint.",
        challenge.title, challenge.brief, verdict.summary
    );
    let mut no_tools = |_name: &str, _input: &str| Ok(String::new());
    let result = client.run(&system, &[], prompt, &mut no_tools)?;
    Ok(result.final_text)
}

// ── Guided lessons ───────────────────────────────────────────────────────────

/// What a lesson step asks the learner to achieve before moving on.
#[derive(Debug, Clone, PartialEq)]
pub enum StepGoal {
    /// Prose only — no circuit state to verify. Checking it always advances.
    Read,
    /// The circuit must contain a part with this reference designator.
    HasPart(String),
    /// The built circuit must meet this measured [`Spec`] (checked on a real solve).
    Meets(Spec),
}

/// One step of a [`Lesson`]: a piece of prose and the goal that gates advancing.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// The instruction/explanation shown to the learner.
    pub prose: String,
    /// What must be true of the learner's circuit to pass this step.
    pub goal: StepGoal,
}

impl Step {
    /// A prose-only step (no circuit check).
    pub fn read(prose: impl Into<String>) -> Self {
        Step {
            prose: prose.into(),
            goal: StepGoal::Read,
        }
    }

    /// A step that is satisfied once a part with `reference` exists.
    pub fn has_part(prose: impl Into<String>, reference: impl Into<String>) -> Self {
        Step {
            prose: prose.into(),
            goal: StepGoal::HasPart(reference.into()),
        }
    }

    /// A step that is satisfied once the circuit meets `spec`.
    pub fn meets(prose: impl Into<String>, spec: Spec) -> Self {
        Step {
            prose: prose.into(),
            goal: StepGoal::Meets(spec),
        }
    }
}

/// A guided lesson: a title and an ordered list of [`Step`]s, walked with a
/// [`LessonRunner`].
#[derive(Debug, Clone, PartialEq)]
pub struct Lesson {
    /// The lesson title.
    pub title: String,
    /// The steps, in teaching order.
    pub steps: Vec<Step>,
}

impl Lesson {
    /// A lesson with the given title and steps.
    pub fn new(title: impl Into<String>, steps: Vec<Step>) -> Self {
        Lesson {
            title: title.into(),
            steps,
        }
    }

    /// The built-in introductory lesson: build a 5 V → 3.3 V divider one part at a
    /// time, ending with a measured check that the output lands at 3.3 V.
    pub fn first_divider() -> Self {
        Lesson::new(
            "Your first voltage divider",
            vec![
                Step::read(
                    "A voltage divider drops a voltage with two resistors in series. We'll turn \
                     a 5 V rail into 3.3 V. Ready when you are.",
                ),
                Step::has_part(
                    "Add the source: `add V1 in gnd 5` — a 5 V supply from node `in` to ground.",
                    "V1",
                ),
                Step::has_part(
                    "Add the top resistor: `add R1 in out 1.7k` — from `in` down to the output.",
                    "R1",
                ),
                Step::has_part(
                    "Add the bottom resistor: `add R2 out gnd 3.3k` — from the output to ground.",
                    "R2",
                ),
                Step::meets(
                    "Solve it (`run op`). The output should sit at 3.3 V — the divider ratio \
                     3.3k/(1.7k+3.3k) of the 5 V rail.",
                    Spec::new(
                        "divider output at 3.3 V",
                        vec![Criterion::node_volts("out", 3.3, 0.05)],
                    ),
                ),
            ],
        )
    }
}

/// How a [`LessonRunner::check`] of the current step turned out.
#[derive(Debug, Clone, PartialEq)]
pub struct StepReport {
    /// `true` if the current step's goal was met and the runner advanced.
    pub advanced: bool,
    /// `true` if the lesson is now finished (every step passed).
    pub complete: bool,
    /// A human message: a confirmation, the measured verdict summary, or why the
    /// step is not yet satisfied.
    pub detail: String,
    /// For a [`StepGoal::Meets`] step, the measured verdict; otherwise `None`.
    pub verdict: Option<Verdict>,
}

/// Walks a [`Lesson`]'s steps, advancing as the learner's circuit meets each
/// step's goal. Borrows the lesson; holds only the current position, so it is
/// cheap to construct and never mutates anything but its own cursor.
#[derive(Debug, Clone)]
pub struct LessonRunner<'a> {
    lesson: &'a Lesson,
    at: usize,
}

impl<'a> LessonRunner<'a> {
    /// Start a runner at the first step of `lesson`.
    pub fn new(lesson: &'a Lesson) -> Self {
        LessonRunner { lesson, at: 0 }
    }

    /// The lesson being walked.
    pub fn lesson(&self) -> &Lesson {
        self.lesson
    }

    /// The zero-based index of the current step.
    pub fn position(&self) -> usize {
        self.at
    }

    /// The current step, or `None` when the lesson is complete.
    pub fn current(&self) -> Option<&Step> {
        self.lesson.steps.get(self.at)
    }

    /// `true` once every step has been passed.
    pub fn is_complete(&self) -> bool {
        self.at >= self.lesson.steps.len()
    }

    /// Check the current step's goal against the learner's `shell`. On success the
    /// runner advances to the next step; on a miss it stays put and the report
    /// says why. A [`StepGoal::Read`] step always passes.
    ///
    /// Errors (as [`AiError::Patch`]) only propagate from a [`StepGoal::Meets`]
    /// spec that names something the circuit lacks or a failed solve — the same
    /// contract as [`Challenge::grade`].
    pub fn check(&mut self, shell: &Shell) -> Result<StepReport, AiError> {
        let Some(step) = self.lesson.steps.get(self.at) else {
            return Ok(StepReport {
                advanced: false,
                complete: true,
                detail: "The lesson is already complete.".to_string(),
                verdict: None,
            });
        };

        let (passed, detail, verdict) = match &step.goal {
            StepGoal::Read => (true, "Step read.".to_string(), None),
            StepGoal::HasPart(reference) => {
                if shell.circuit().part_by_ref(reference).is_some() {
                    (true, format!("Found {reference}."), None)
                } else {
                    (
                        false,
                        format!("Add {reference} to continue — it is not in the circuit yet."),
                        None,
                    )
                }
            }
            StepGoal::Meets(spec) => {
                let v = grade(shell, spec)?;
                (v.pass, v.summary.clone(), Some(v))
            }
        };

        if passed {
            self.at += 1;
        }
        Ok(StepReport {
            advanced: passed,
            complete: self.is_complete(),
            detail,
            verdict,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Client;
    use crate::message::{ContentBlock, ModelResponse, StopReason, Usage};
    use crate::transport::MockTransport;

    /// A correct 5 V → 3.3 V divider: 1.7 kΩ over 3.3 kΩ gives 5·3.3/5 = 3.3 V.
    fn correct_divider() -> Shell {
        let mut sh = Shell::new();
        for cmd in [
            "add V1 in gnd 5",
            "add R1 in out 1.7k",
            "add R2 out gnd 3.3k",
        ] {
            sh.execute(cmd).unwrap();
        }
        sh
    }

    /// A wrong divider: two equal 1 kΩ legs land `out` at 2.5 V, not 3.3 V.
    fn wrong_divider() -> Shell {
        let mut sh = Shell::new();
        for cmd in ["add V1 in gnd 5", "add R1 in out 1k", "add R2 out gnd 1k"] {
            sh.execute(cmd).unwrap();
        }
        sh
    }

    // ── Challenge gate ─────────────────────────────────────────────────────────

    /// THE GATE: the built-in divider challenge auto-scores PASS against a correct
    /// circuit via a real sim read.
    #[test]
    fn challenge_passes_a_correct_divider_via_real_sim() {
        let challenge = Challenge::divider_3v3_from_5v();
        let verdict = challenge.grade(&correct_divider()).expect("grade runs");
        assert!(verdict.pass, "correct divider passes: {}", verdict.summary);
        assert!(verdict.summary.contains("PASS"), "{}", verdict.summary);
        // The number cited is the real measured value, near 3.3 V.
        let out = &verdict.outcomes[0];
        assert_eq!(out.label, "v(out)");
        assert!(
            (out.measured - 3.3).abs() < 0.05,
            "measured {}",
            out.measured
        );
    }

    /// THE GATE: the same challenge auto-scores FAIL against a wrong circuit and
    /// NAMES the miss, measured — again via a real solve, no model.
    #[test]
    fn challenge_fails_a_wrong_divider_and_names_the_miss() {
        let challenge = Challenge::divider_3v3_from_5v();
        let verdict = challenge.grade(&wrong_divider()).expect("grade runs");
        assert!(!verdict.pass, "wrong divider fails: {}", verdict.summary);
        assert!(verdict.summary.contains("FAIL"), "{}", verdict.summary);
        assert!(
            verdict.summary.contains("misses target"),
            "names the miss: {}",
            verdict.summary
        );
        let out = &verdict.outcomes[0];
        assert!(!out.pass);
        // The real measured value is ~2.5 V, well off the 3.3 V target.
        assert!(
            (out.measured - 2.5).abs() < 0.05,
            "measured {}",
            out.measured
        );
        assert!(out.gap() > 0.5, "the gap is the real miss: {}", out.gap());
    }

    /// A spec that reads a node the learner never built is a Patch error, not a
    /// bogus pass/fail.
    #[test]
    fn grading_a_missing_node_is_an_error() {
        let challenge = Challenge::divider_3v3_from_5v();
        let mut empty = Shell::new();
        empty.execute("add V1 in gnd 5").unwrap(); // no `out` node
        let err = challenge.grade(&empty).unwrap_err();
        assert!(matches!(err, AiError::Patch(_)), "{err}");
    }

    // ── Lessons ─────────────────────────────────────────────────────────────────

    /// A lesson steps: each step's goal gates advancing, and building the circuit
    /// part by part walks the runner to completion, ending on a real measured check.
    #[test]
    fn lesson_runner_advances_as_the_circuit_is_built() {
        let lesson = Lesson::first_divider();
        let mut runner = LessonRunner::new(&lesson);
        let mut sh = Shell::new();

        // Step 0 is read-only: checking it advances with no circuit change.
        assert_eq!(runner.position(), 0);
        let r0 = runner.check(&sh).unwrap();
        assert!(r0.advanced && !r0.complete);
        assert_eq!(runner.position(), 1);

        // Step 1 wants V1 — a bare circuit does NOT advance.
        let miss = runner.check(&sh).unwrap();
        assert!(!miss.advanced, "no V1 yet");
        assert_eq!(runner.position(), 1, "stayed on the V1 step");
        assert!(miss.detail.contains("V1"), "{}", miss.detail);

        // Add each part in turn; each `has_part` step then advances.
        sh.execute("add V1 in gnd 5").unwrap();
        assert!(runner.check(&sh).unwrap().advanced);
        sh.execute("add R1 in out 1.7k").unwrap();
        assert!(runner.check(&sh).unwrap().advanced);
        sh.execute("add R2 out gnd 3.3k").unwrap();
        assert!(runner.check(&sh).unwrap().advanced);

        // Final step is a measured spec; the finished divider meets it and the
        // lesson completes.
        assert!(!runner.is_complete());
        let last = runner.check(&sh).unwrap();
        assert!(last.advanced && last.complete, "{}", last.detail);
        assert!(last.verdict.as_ref().unwrap().pass);
        assert!(runner.is_complete());
        assert!(runner.current().is_none());

        // Checking past the end is a harmless "already complete".
        let after = runner.check(&sh).unwrap();
        assert!(!after.advanced && after.complete);
    }

    /// A measured step does not advance while the circuit misses its spec.
    #[test]
    fn measured_step_holds_until_the_spec_is_met() {
        let lesson = Lesson::new(
            "mini",
            vec![Step::meets(
                "get out to 3.3 V",
                Spec::new("3v3", vec![Criterion::node_volts("out", 3.3, 0.05)]),
            )],
        );
        let mut runner = LessonRunner::new(&lesson);
        let report = runner.check(&wrong_divider()).unwrap();
        assert!(!report.advanced, "2.5 V misses 3.3 V");
        assert!(!runner.is_complete());
        assert!(report.verdict.is_some());
        // The correct circuit then advances it to done.
        assert!(runner.check(&correct_divider()).unwrap().complete);
    }

    // ── Glossary ──────────────────────────────────────────────────────────────

    #[test]
    fn glossary_defines_and_searches() {
        // A case-insensitive exact lookup.
        assert!(define("voltage divider").unwrap().contains("series"));
        assert!(define("Voltage Divider").is_some(), "case-insensitive");
        assert!(define("not a term").is_none());

        // A contextual search finds entries by term or definition, term hits first.
        let hits = search_terms("resistor");
        assert!(!hits.is_empty());
        assert_eq!(
            hits[0].term, "pull-up resistor",
            "term match ranks first: {hits:?}"
        );

        // Deterministic and non-empty catalog.
        assert_eq!(search_terms("resistor"), hits);
        assert!(!glossary().is_empty());
        assert!(search_terms("   ").is_empty());
    }

    // ── Optional model coach (deterministic grade, optional prose) ───────────────

    /// The optional model path: grading stays deterministic, but `coach` returns
    /// the model's natural-language hint. A stubbed model proves it needs no
    /// network and advertises no tools.
    #[test]
    fn coach_returns_a_model_hint_without_tools() {
        let challenge = Challenge::divider_3v3_from_5v();
        let verdict = challenge.grade(&wrong_divider()).unwrap();
        assert!(!verdict.pass);

        let answer = ModelResponse {
            content: vec![ContentBlock::text(
                "Close! Your output is a bit low — try raising the bottom resistor relative to the top.",
            )],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        };
        let client = Client::new(MockTransport::new([answer])).with_prompt_cache(false);
        let hint = coach(&client, &challenge, &verdict).unwrap();
        assert!(hint.contains("bottom resistor"), "{hint}");
    }
}
