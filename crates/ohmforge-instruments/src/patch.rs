//! Dataflow patcher — a LabVIEW-style patch graph over [`Signal`]s (`CP-UX-M3.1`).
//!
//! A *patch* wires [`Block`]s (gain, offset, arithmetic, a filter, clamps,
//! reductions, a log tap) into a directed graph. Edges carry [`Port`] values —
//! either a whole [`Signal`] or a reduced scalar — from a declared input to an
//! output. [`Patch::eval`] walks the graph in topological order, so the result
//! is deterministic and a cycle is reported as a clean [`PatchError`] rather than
//! looping forever.
//!
//! A saved patch is a [`VirtualInstrument`]: a reusable bench you re-run on fresh
//! inputs. A [`TestSequence`] runs pass/fail [`Check`]s against a run's outputs
//! and produces a named [`TestReport`].
//!
//! The processing blocks reuse the existing scope-math where it fits: binary
//! arithmetic goes through [`math_channel`] (so two signals on different grids
//! resample exactly as a scope math channel does), and the reductions reuse the
//! multimeter's [`measure`] pass.
//!
//! Same contract as the other instruments: `ohmforge-core`-only data, pure and
//! deterministic, no terminal — the patch and the report each render to a
//! `String`.

use crate::dmm::measure;
use crate::fmt::{fmt_eng, fmt_num};
use crate::math::{math_channel, MathOp};
use ohmforge_core::signal::{Signal, SignalData};
use std::fmt;
use std::sync::Arc;

/// A value flowing along a patch edge: a whole waveform or a reduced scalar.
#[derive(Debug, Clone, PartialEq)]
pub enum Port {
    /// A time- or frequency-domain waveform.
    Signal(Signal),
    /// A dimensionless scalar (a reduction such as an RMS, or a bare number).
    Scalar(f64),
}

impl Port {
    /// The signal, if this port carries one.
    pub fn as_signal(&self) -> Option<&Signal> {
        match self {
            Port::Signal(s) => Some(s),
            Port::Scalar(_) => None,
        }
    }

    /// The scalar, if this port carries one.
    pub fn as_scalar(&self) -> Option<f64> {
        match self {
            Port::Scalar(v) => Some(*v),
            Port::Signal(_) => None,
        }
    }

    /// A short human tag for error/log messages.
    fn kind(&self) -> &'static str {
        match self {
            Port::Signal(_) => "signal",
            Port::Scalar(_) => "scalar",
        }
    }
}

/// Which end of the band a single-pole [`Filter`] passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    /// Pass frequencies below the cutoff, attenuate above it.
    LowPass,
    /// Pass frequencies above the cutoff, attenuate below it.
    HighPass,
}

/// A single-pole (RC) filter over a real time-domain signal.
///
/// The pole is placed at `cutoff_hz`; the difference equation uses each step's
/// own `dt` from the signal's time axis, so a non-uniform grid still filters
/// correctly. A low-pass smooths; a high-pass removes the slow component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Filter {
    /// Pass band selection.
    pub kind: FilterKind,
    /// The −3 dB corner, in hertz.
    pub cutoff_hz: f64,
}

impl Filter {
    /// A low-pass with its corner at `cutoff_hz`.
    pub fn low_pass(cutoff_hz: f64) -> Self {
        Self {
            kind: FilterKind::LowPass,
            cutoff_hz,
        }
    }

    /// A high-pass with its corner at `cutoff_hz`.
    pub fn high_pass(cutoff_hz: f64) -> Self {
        Self {
            kind: FilterKind::HighPass,
            cutoff_hz,
        }
    }

    /// Apply the filter to a real time-domain signal, returning a new signal on
    /// the same grid. A per-step `dt` is read from `x`; a zero/negative step
    /// passes the sample through unchanged.
    fn apply(&self, sig: &Signal) -> Signal {
        let y = sig.real().unwrap_or(&[]);
        let n = y.len();
        let mut out = vec![0.0; n];
        if n == 0 {
            return Signal::time(sig.name.clone(), sig.unit, sig.x.clone(), out);
        }
        // Angular corner; `rc = 1 / (2π·fc)`.
        let rc = if self.cutoff_hz > 0.0 {
            1.0 / (std::f64::consts::TAU * self.cutoff_hz)
        } else {
            f64::INFINITY
        };
        out[0] = match self.kind {
            // A low-pass seeds with the first sample (steady state at t0);
            // a high-pass starts at zero (no DC passes).
            FilterKind::LowPass => y[0],
            FilterKind::HighPass => 0.0,
        };
        for i in 1..n {
            let dt = sig.x[i] - sig.x[i - 1];
            // A zero/negative/NaN step carries the previous output forward.
            if dt > 0.0 {
                match self.kind {
                    FilterKind::LowPass => {
                        let a = dt / (rc + dt);
                        out[i] = out[i - 1] + a * (y[i] - out[i - 1]);
                    }
                    FilterKind::HighPass => {
                        let a = rc / (rc + dt);
                        out[i] = a * (out[i - 1] + y[i] - y[i - 1]);
                    }
                }
            } else {
                out[i] = out[i - 1];
            }
        }
        Signal::time(sig.name.clone(), sig.unit, sig.x.clone(), out)
    }
}

/// An externally defined processing block — the extensibility seam (`CP-UX-M4.1`).
///
/// Implement this on your own type and register it into a [`Patch`] with
/// [`Block::plugin`]; it then evaluates alongside the built-in [`Block`] variants,
/// composing freely with them. A plugin is pure and deterministic like every
/// built-in: given the same input [`Port`]s it must return the same output, and
/// it reports a bad input as a [`PatchError`] rather than panicking.
///
/// ```
/// use ohmforge_instruments::{Block, Patch, Port, PatchError, ProcessingBlock};
/// use ohmforge_core::signal::{Signal, SignalData};
///
/// /// Squares every sample of a real signal (or a bare scalar).
/// #[derive(Debug)]
/// struct Square;
/// impl ProcessingBlock for Square {
///     fn name(&self) -> String { "Square".into() }
///     fn arity(&self) -> usize { 1 }
///     fn eval(&self, inputs: &[Port]) -> Result<Port, PatchError> {
///         match &inputs[0] {
///             Port::Scalar(v) => Ok(Port::Scalar(v * v)),
///             Port::Signal(s) => {
///                 let y = s.real().unwrap_or(&[]);
///                 let sq: Vec<f64> = y.iter().map(|v| v * v).collect();
///                 Ok(Port::Signal(Signal::time(s.name.clone(), s.unit, s.x.clone(), sq)))
///             }
///         }
///     }
/// }
///
/// let mut p = Patch::new();
/// let i = p.input("in");
/// let sq = p.add(Block::plugin(Square), &[i]);
/// p.output("out", sq);
/// ```
pub trait ProcessingBlock: fmt::Debug {
    /// A short label for the block, shown in [`Patch::render`].
    fn name(&self) -> String;

    /// How many input [`Port`]s the block consumes. The patcher checks this
    /// before calling [`eval`](ProcessingBlock::eval), so `eval` can index
    /// `inputs[0..arity()]` without a bounds check.
    fn arity(&self) -> usize;

    /// Evaluate the block over its resolved inputs (exactly [`arity`](ProcessingBlock::arity)
    /// of them). Report a bad input kind/shape as a [`PatchError`] — never panic.
    fn eval(&self, inputs: &[Port]) -> Result<Port, PatchError>;
}

/// A cheap, shareable handle to a registered [`ProcessingBlock`].
///
/// It wraps the boxed plugin in an [`Arc`] so a [`Block`] (and therefore a
/// [`Patch`]) stays `Clone`, and carries hand-written [`Debug`]/[`PartialEq`]
/// impls so [`Block`] keeps its derives. Two handles compare equal only when they
/// refer to the very same registered block.
#[derive(Clone)]
pub struct PluginHandle(Arc<dyn ProcessingBlock>);

impl PluginHandle {
    fn new(block: impl ProcessingBlock + 'static) -> Self {
        Self(Arc::new(block))
    }
}

impl fmt::Debug for PluginHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Plugin({})", self.0.name())
    }
}

impl PartialEq for PluginHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A processing block: the unit of computation at a patch node.
///
/// Arity is fixed per variant — [`Add`](Block::Add)/[`Sub`](Block::Sub)/
/// [`Mul`](Block::Mul) take two inputs, every other built-in takes one; a
/// [`Plugin`](Block::Plugin) carries its own arity. The scaling/offset/bounding
/// blocks map over a signal's samples *or* act on a bare scalar, so a
/// reduce-then-scale chain (`… → Rms → Gain(2)`) works too.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Multiply every sample (or the scalar) by `k`.
    Gain(f64),
    /// Add the constant `c` to every sample (or the scalar).
    Offset(f64),
    /// Elementwise sum of two inputs.
    Add,
    /// Elementwise difference of two inputs (`a − b`).
    Sub,
    /// Elementwise product of two inputs.
    Mul,
    /// Single-pole low/high-pass over a real time signal.
    Filter(Filter),
    /// Absolute value of every sample (or the scalar).
    Abs,
    /// Bound every sample (or the scalar) to `[min, max]`.
    Clamp {
        /// Lower bound.
        min: f64,
        /// Upper bound.
        max: f64,
    },
    /// Symmetric limiter: bound every sample (or the scalar) to `[−|l|, |l|]`.
    Limit(f64),
    /// Reduce a signal to its true-RMS scalar, `sqrt(mean(y²))`.
    Rms,
    /// Reduce a signal to its mean (DC) scalar.
    Mean,
    /// Reduce a signal to its peak scalar, `max|y|`.
    Peak,
    /// Identity tap that records its input into the run's log under a label.
    Log(String),
    /// An externally registered [`ProcessingBlock`]; build one with [`Block::plugin`].
    Plugin(PluginHandle),
}

impl Block {
    /// Register an external [`ProcessingBlock`] as a block, so it can be wired
    /// into a [`Patch`] with [`add`](Patch::add) alongside the built-ins.
    pub fn plugin(block: impl ProcessingBlock + 'static) -> Self {
        Block::Plugin(PluginHandle::new(block))
    }

    /// How many inputs this block consumes.
    pub fn arity(&self) -> usize {
        match self {
            Block::Add | Block::Sub | Block::Mul => 2,
            Block::Plugin(p) => p.0.arity(),
            _ => 1,
        }
    }

    /// A short label for the patch render.
    fn label(&self) -> String {
        match self {
            Block::Gain(k) => format!("Gain({})", fmt_num(*k)),
            Block::Offset(c) => format!("Offset({})", fmt_num(*c)),
            Block::Add => "Add".to_string(),
            Block::Sub => "Sub".to_string(),
            Block::Mul => "Mul".to_string(),
            Block::Filter(f) => {
                let k = match f.kind {
                    FilterKind::LowPass => "LP",
                    FilterKind::HighPass => "HP",
                };
                format!("Filter({k} {})", fmt_eng(f.cutoff_hz, "Hz"))
            }
            Block::Abs => "Abs".to_string(),
            Block::Clamp { min, max } => format!("Clamp({},{})", fmt_num(*min), fmt_num(*max)),
            Block::Limit(l) => format!("Limit({})", fmt_num(*l)),
            Block::Rms => "Rms".to_string(),
            Block::Mean => "Mean".to_string(),
            Block::Peak => "Peak".to_string(),
            Block::Log(name) => format!("Log({name})"),
            Block::Plugin(p) => p.0.name(),
        }
    }
}

/// A reference to an edge source: either a declared patch input or a node output.
///
/// Returned by [`Patch::input`] and [`Patch::add`]; pass it as another block's
/// input to wire the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// The declared input signal at this index.
    Input(usize),
    /// The output of the node at this index.
    Node(usize),
}

/// One node in the patch: a block and where each of its inputs comes from.
#[derive(Debug, Clone)]
struct Node {
    block: Block,
    inputs: Vec<Wire>,
}

/// A dataflow patch: declared inputs, a graph of [`Block`] nodes, named outputs.
///
/// Build it with [`input`](Patch::input), [`add`](Patch::add) and
/// [`output`](Patch::output), then [`eval`](Patch::eval) it on concrete signals.
/// Because [`add`](Patch::add) returns a [`Wire`] to the node it just created, a graph built
/// purely with the builder is acyclic by construction; [`set_inputs`](Patch::set_inputs)
/// can rewire a node to form (and thereby let [`eval`](Patch::eval) report) a cycle.
#[derive(Debug, Clone, Default)]
pub struct Patch {
    inputs: Vec<String>,
    nodes: Vec<Node>,
    outputs: Vec<(String, Wire)>,
}

impl Patch {
    /// An empty patch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare an input signal port and return a [`Wire`] that feeds it into
    /// blocks. The `name` is matched against the signals passed to
    /// [`eval`](Patch::eval).
    pub fn input(&mut self, name: impl Into<String>) -> Wire {
        self.inputs.push(name.into());
        Wire::Input(self.inputs.len() - 1)
    }

    /// Add a block fed by `inputs`, returning a [`Wire`] to its output.
    ///
    /// `inputs` should carry one wire per the block's [`arity`](Block::arity);
    /// a mismatch is not rejected here but surfaces as a clean
    /// [`PatchError::Arity`] at [`eval`](Patch::eval) time.
    pub fn add(&mut self, block: Block, inputs: &[Wire]) -> Wire {
        self.nodes.push(Node {
            block,
            inputs: inputs.to_vec(),
        });
        Wire::Node(self.nodes.len() - 1)
    }

    /// Re-point an existing node's inputs. Takes a [`Wire::Node`] naming the node
    /// to rewire; a [`Wire::Input`] is ignored. Used to rewire a patch — and, by
    /// pointing a node back at one that depends on it, to form a cycle.
    pub fn set_inputs(&mut self, node: Wire, inputs: &[Wire]) {
        if let Wire::Node(i) = node {
            if let Some(n) = self.nodes.get_mut(i) {
                n.inputs = inputs.to_vec();
            }
        }
    }

    /// Mark `wire` as a named output of the patch.
    pub fn output(&mut self, name: impl Into<String>, wire: Wire) {
        self.outputs.push((name.into(), wire));
    }

    /// Topological node order (indices), or the set of nodes on a cycle.
    ///
    /// Kahn's algorithm, always draining the lowest ready index first, so the
    /// order is deterministic.
    fn topo_order(&self) -> Result<Vec<usize>, PatchError> {
        let n = self.nodes.len();
        let mut indeg = vec![0usize; n];
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, node) in self.nodes.iter().enumerate() {
            for w in &node.inputs {
                if let Wire::Node(j) = *w {
                    dependents[j].push(i);
                    indeg[i] += 1;
                }
            }
        }
        // Ready = indegree-zero nodes, kept sorted ascending.
        let mut ready: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
        let mut order = Vec::with_capacity(n);
        while !ready.is_empty() {
            let i = ready.remove(0);
            order.push(i);
            for &k in &dependents[i] {
                indeg[k] -= 1;
                if indeg[k] == 0 {
                    let pos = ready.partition_point(|&v| v < k);
                    ready.insert(pos, k);
                }
            }
        }
        if order.len() == n {
            Ok(order)
        } else {
            // Whatever never reached indegree zero is on (or fed by) a cycle.
            let cycle: Vec<usize> = (0..n).filter(|&i| indeg[i] > 0).collect();
            Err(PatchError::Cycle(cycle))
        }
    }

    /// Evaluate the patch on the given named input signals.
    ///
    /// Inputs are matched to declared ports by name; a declared port with no
    /// matching signal is a [`PatchError::MissingInput`]. Returns a
    /// [`PatchRun`] holding the named outputs and anything a [`Block::Log`] tap
    /// recorded. A cycle, an arity mismatch, or a block fed the wrong port kind
    /// is reported as the corresponding [`PatchError`] — never a panic or a hang.
    pub fn eval(&self, inputs: &[(&str, Signal)]) -> Result<PatchRun, PatchError> {
        // Resolve declared inputs by name, in declaration order.
        let mut input_ports: Vec<Port> = Vec::with_capacity(self.inputs.len());
        for name in &self.inputs {
            let sig = inputs
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, s)| s.clone())
                .ok_or_else(|| PatchError::MissingInput(name.clone()))?;
            input_ports.push(Port::Signal(sig));
        }

        let order = self.topo_order()?;
        let mut node_out: Vec<Option<Port>> = vec![None; self.nodes.len()];
        let mut logs: Vec<(String, Port)> = Vec::new();

        for &i in &order {
            let node = &self.nodes[i];
            if node.inputs.len() != node.block.arity() {
                return Err(PatchError::Arity {
                    node: i,
                    expected: node.block.arity(),
                    got: node.inputs.len(),
                });
            }
            // Resolve each input wire to its already-computed port.
            let mut args: Vec<&Port> = Vec::with_capacity(node.inputs.len());
            for w in &node.inputs {
                let p = match *w {
                    Wire::Input(k) => input_ports.get(k),
                    Wire::Node(k) => node_out.get(k).and_then(|o| o.as_ref()),
                };
                match p {
                    Some(p) => args.push(p),
                    None => return Err(PatchError::DanglingWire { node: i, wire: *w }),
                }
            }
            let (out, log) = eval_block(&node.block, i, &args)?;
            if let Some(entry) = log {
                logs.push(entry);
            }
            node_out[i] = Some(out);
        }

        // Collect named outputs in declaration order.
        let mut outputs: Vec<(String, Port)> = Vec::with_capacity(self.outputs.len());
        for (name, wire) in &self.outputs {
            let port = match *wire {
                Wire::Input(k) => input_ports.get(k).cloned(),
                Wire::Node(k) => node_out.get(k).and_then(|o| o.clone()),
            };
            match port {
                Some(p) => outputs.push((name.clone(), p)),
                None => {
                    return Err(PatchError::DanglingWire {
                        node: usize::MAX,
                        wire: *wire,
                    })
                }
            }
        }

        Ok(PatchRun { outputs, logs })
    }

    /// Render the patch topology (inputs, nodes with their wiring, outputs) to a
    /// deterministic multi-line string.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("patch\n");
        for (i, name) in self.inputs.iter().enumerate() {
            out.push_str(&format!("  in[{i}] {name}\n"));
        }
        for (i, node) in self.nodes.iter().enumerate() {
            let srcs: Vec<String> = node.inputs.iter().map(wire_label).collect();
            out.push_str(&format!(
                "  n{i} {} <- [{}]\n",
                node.block.label(),
                srcs.join(", ")
            ));
        }
        for (name, wire) in &self.outputs {
            out.push_str(&format!("  out {name} = {}\n", wire_label(wire)));
        }
        out
    }
}

/// Short label for a wire source, used in [`Patch::render`].
fn wire_label(w: &Wire) -> String {
    match w {
        Wire::Input(i) => format!("in{i}"),
        Wire::Node(i) => format!("n{i}"),
    }
}

/// Evaluate one block over its resolved input ports.
fn eval_block(
    block: &Block,
    node: usize,
    args: &[&Port],
) -> Result<(Port, Option<(String, Port)>), PatchError> {
    let out = match block {
        Block::Gain(k) => map_port(node, args[0], |v| v * k)?,
        Block::Offset(c) => map_port(node, args[0], |v| v + c)?,
        Block::Abs => map_port(node, args[0], f64::abs)?,
        Block::Clamp { min, max } => {
            let (lo, hi) = (min.min(*max), min.max(*max));
            map_port(node, args[0], |v| v.clamp(lo, hi))?
        }
        Block::Limit(l) => {
            let m = l.abs();
            map_port(node, args[0], |v| v.clamp(-m, m))?
        }
        Block::Add => binary(node, args[0], args[1], MathOp::Add)?,
        Block::Sub => binary(node, args[0], args[1], MathOp::Sub)?,
        Block::Mul => binary(node, args[0], args[1], MathOp::Mul)?,
        Block::Filter(f) => {
            let sig = expect_signal(node, args[0])?;
            Port::Signal(f.apply(sig))
        }
        Block::Rms => Port::Scalar(measure(expect_signal(node, args[0])?).rms),
        Block::Mean => Port::Scalar(measure(expect_signal(node, args[0])?).dc),
        Block::Peak => Port::Scalar(peak(expect_signal(node, args[0])?)),
        Block::Log(name) => {
            let p = args[0].clone();
            return Ok((p.clone(), Some((name.clone(), p))));
        }
        Block::Plugin(p) => {
            // The arity check in `eval` already guaranteed `args.len() == arity`.
            let owned: Vec<Port> = args.iter().map(|&a| a.clone()).collect();
            p.0.eval(&owned)?
        }
    };
    Ok((out, None))
}

/// Apply `f` to a real signal's samples, or to a bare scalar. A complex signal
/// is rejected — these blocks are for real time-domain data.
fn map_port(node: usize, port: &Port, f: impl Fn(f64) -> f64) -> Result<Port, PatchError> {
    match port {
        Port::Scalar(v) => Ok(Port::Scalar(f(*v))),
        Port::Signal(s) => match &s.y {
            SignalData::Real(y) => {
                let y: Vec<f64> = y.iter().map(|&v| f(v)).collect();
                Ok(Port::Signal(Signal {
                    name: s.name.clone(),
                    unit: s.unit,
                    domain: s.domain,
                    x: s.x.clone(),
                    y: SignalData::Real(y),
                }))
            }
            SignalData::Complex(_) => Err(PatchError::TypeMismatch {
                node,
                detail: "block needs a real signal, got a complex one".to_string(),
            }),
        },
    }
}

/// Binary arithmetic: two signals via [`math_channel`], two scalars directly, or
/// a signal broadcast against a scalar.
fn binary(node: usize, a: &Port, b: &Port, op: MathOp) -> Result<Port, PatchError> {
    let scalar_op = |x: f64, y: f64| match op {
        MathOp::Add => x + y,
        MathOp::Sub => x - y,
        MathOp::Mul => x * y,
    };
    match (a, b) {
        (Port::Signal(sa), Port::Signal(sb)) => {
            Ok(Port::Signal(math_channel(sa.name.clone(), sa, sb, op)))
        }
        (Port::Scalar(x), Port::Scalar(y)) => Ok(Port::Scalar(scalar_op(*x, *y))),
        (Port::Signal(s), Port::Scalar(k)) => broadcast(node, s, *k, |y| scalar_op(y, *k)),
        (Port::Scalar(k), Port::Signal(s)) => broadcast(node, s, *k, |y| scalar_op(*k, y)),
    }
}

/// Apply `f` to each sample of a real signal (one operand being a scalar).
fn broadcast(node: usize, s: &Signal, _k: f64, f: impl Fn(f64) -> f64) -> Result<Port, PatchError> {
    match &s.y {
        SignalData::Real(y) => {
            let y: Vec<f64> = y.iter().map(|&v| f(v)).collect();
            Ok(Port::Signal(Signal {
                name: s.name.clone(),
                unit: s.unit,
                domain: s.domain,
                x: s.x.clone(),
                y: SignalData::Real(y),
            }))
        }
        SignalData::Complex(_) => Err(PatchError::TypeMismatch {
            node,
            detail: "arithmetic needs a real signal, got a complex one".to_string(),
        }),
    }
}

/// The peak scalar: the largest absolute finite sample, or `0` for an empty /
/// non-real signal.
fn peak(sig: &Signal) -> f64 {
    sig.real()
        .unwrap_or(&[])
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0, |m, v| m.max(v.abs()))
}

/// Require a port to carry a signal.
fn expect_signal(node: usize, port: &Port) -> Result<&Signal, PatchError> {
    port.as_signal().ok_or_else(|| PatchError::TypeMismatch {
        node,
        detail: format!("block needs a signal, got a {}", port.kind()),
    })
}

/// An error building or evaluating a patch. Every failure mode is one of these —
/// the patcher never panics or loops on bad wiring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    /// The graph has a cycle; the field lists the node indices on it.
    Cycle(Vec<usize>),
    /// A declared input had no matching signal passed to [`Patch::eval`].
    MissingInput(String),
    /// A node had the wrong number of input wires for its block.
    Arity {
        /// The offending node index.
        node: usize,
        /// Inputs the block needs.
        expected: usize,
        /// Inputs the node was given.
        got: usize,
    },
    /// A wire referenced a node/input that produced no value.
    DanglingWire {
        /// The node being evaluated (`usize::MAX` for an output wire).
        node: usize,
        /// The dangling wire.
        wire: Wire,
    },
    /// A block was fed the wrong kind of port (e.g. a scalar where it needs a
    /// signal, or a complex signal where it needs a real one).
    TypeMismatch {
        /// The offending node index.
        node: usize,
        /// What went wrong.
        detail: String,
    },
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatchError::Cycle(nodes) => write!(f, "patch has a cycle through nodes {nodes:?}"),
            PatchError::MissingInput(name) => write!(f, "no signal provided for input {name:?}"),
            PatchError::Arity {
                node,
                expected,
                got,
            } => write!(f, "node {node} needs {expected} input(s), got {got}"),
            PatchError::DanglingWire { node, wire } => {
                write!(f, "node {node} has a dangling wire {wire:?}")
            }
            PatchError::TypeMismatch { node, detail } => write!(f, "node {node}: {detail}"),
        }
    }
}

impl std::error::Error for PatchError {}

/// The result of evaluating a [`Patch`]: named output ports and logged taps.
#[derive(Debug, Clone)]
pub struct PatchRun {
    outputs: Vec<(String, Port)>,
    logs: Vec<(String, Port)>,
}

impl PatchRun {
    /// The output port named `name`, if present.
    pub fn get(&self, name: &str) -> Option<&Port> {
        self.outputs.iter().find(|(n, _)| n == name).map(|(_, p)| p)
    }

    /// The signal output named `name`, if it is a signal.
    pub fn signal(&self, name: &str) -> Option<&Signal> {
        self.get(name).and_then(Port::as_signal)
    }

    /// The scalar output named `name`, if it is a scalar.
    pub fn scalar(&self, name: &str) -> Option<f64> {
        self.get(name).and_then(Port::as_scalar)
    }

    /// Every named output, in declaration order.
    pub fn outputs(&self) -> &[(String, Port)] {
        &self.outputs
    }

    /// Everything a [`Block::Log`] tap recorded, in evaluation order.
    pub fn logs(&self) -> &[(String, Port)] {
        &self.logs
    }
}

/// A saved patch you re-run on new inputs — the "virtual instrument".
///
/// Holds a name and a [`Patch`]; [`run`](VirtualInstrument::run) is a thin
/// wrapper over [`Patch::eval`] so the same bench can be driven with many
/// different input signals.
#[derive(Debug, Clone)]
pub struct VirtualInstrument {
    name: String,
    patch: Patch,
}

impl VirtualInstrument {
    /// Save `patch` under `name`.
    pub fn new(name: impl Into<String>, patch: Patch) -> Self {
        Self {
            name: name.into(),
            patch,
        }
    }

    /// The instrument's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The underlying patch.
    pub fn patch(&self) -> &Patch {
        &self.patch
    }

    /// Re-run the saved patch on fresh inputs.
    pub fn run(&self, inputs: &[(&str, Signal)]) -> Result<PatchRun, PatchError> {
        self.patch.eval(inputs)
    }

    /// Render the instrument's name and patch topology to a string.
    pub fn render(&self) -> String {
        format!(
            "{} ({} blocks)\n{}",
            self.name,
            self.patch.nodes.len(),
            self.patch.render()
        )
    }
}

/// The bound a [`Check`] asserts on a scalar output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bound {
    /// The value must lie in `[lo, hi]` inclusive.
    Within(f64, f64),
    /// The value must be `< max`.
    LessThan(f64),
    /// The value must be `> min`.
    GreaterThan(f64),
}

impl Bound {
    /// Whether `v` satisfies the bound.
    fn holds(&self, v: f64) -> bool {
        match *self {
            Bound::Within(lo, hi) => v >= lo && v <= hi,
            Bound::LessThan(max) => v < max,
            Bound::GreaterThan(min) => v > min,
        }
    }

    fn describe(&self) -> String {
        match *self {
            Bound::Within(lo, hi) => format!("in [{}, {}]", fmt_num(lo), fmt_num(hi)),
            Bound::LessThan(max) => format!("< {}", fmt_num(max)),
            Bound::GreaterThan(min) => format!("> {}", fmt_num(min)),
        }
    }
}

/// One pass/fail assertion against a patch output.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    /// Human name, reported in the pass/fail report.
    pub name: String,
    /// The scalar output to inspect.
    pub output: String,
    /// The bound it must satisfy.
    pub bound: Bound,
}

impl Check {
    /// A named check that `output` satisfies `bound`.
    pub fn new(name: impl Into<String>, output: impl Into<String>, bound: Bound) -> Self {
        Self {
            name: name.into(),
            output: output.into(),
            bound,
        }
    }

    /// Evaluate against a run, producing a [`CheckResult`].
    fn eval(&self, run: &PatchRun) -> CheckResult {
        match run.scalar(&self.output) {
            Some(v) => {
                let passed = self.bound.holds(v);
                let detail = format!("{} = {} {}", self.output, fmt_num(v), self.bound.describe());
                CheckResult {
                    name: self.name.clone(),
                    passed,
                    detail,
                }
            }
            None => CheckResult {
                name: self.name.clone(),
                passed: false,
                detail: format!("output {:?} is not a scalar", self.output),
            },
        }
    }
}

/// The outcome of one [`Check`].
#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    /// The check's name.
    pub name: String,
    /// Whether it passed.
    pub passed: bool,
    /// A human explanation (the value and the bound it was tested against).
    pub detail: String,
}

/// An ordered list of [`Check`]s to run against a patch's outputs.
#[derive(Debug, Clone, Default)]
pub struct TestSequence {
    name: String,
    checks: Vec<Check>,
}

impl TestSequence {
    /// A new, empty sequence named `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            checks: Vec::new(),
        }
    }

    /// Append a check (builder style).
    pub fn check(mut self, check: Check) -> Self {
        self.checks.push(check);
        self
    }

    /// The sequence's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Run every check against `run`, in order.
    pub fn run(&self, run: &PatchRun) -> TestReport {
        TestReport {
            name: self.name.clone(),
            results: self.checks.iter().map(|c| c.eval(run)).collect(),
        }
    }
}

/// The pass/fail report from running a [`TestSequence`].
#[derive(Debug, Clone, PartialEq)]
pub struct TestReport {
    /// The sequence's name.
    pub name: String,
    /// One result per check, in order.
    pub results: Vec<CheckResult>,
}

impl TestReport {
    /// Whether every check passed.
    pub fn passed(&self) -> bool {
        self.results.iter().all(|r| r.passed)
    }

    /// The failing checks, in order.
    pub fn failures(&self) -> impl Iterator<Item = &CheckResult> {
        self.results.iter().filter(|r| !r.passed)
    }

    /// Render the report: a header line, then `PASS`/`FAIL` per named check.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let verdict = if self.passed() { "PASS" } else { "FAIL" };
        out.push_str(&format!("{} [{verdict}]\n", self.name));
        for r in &self.results {
            let tag = if r.passed { "PASS" } else { "FAIL" };
            out.push_str(&format!("  [{tag}] {}: {}\n", r.name, r.detail));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ohmforge_core::units::Unit;
    use std::f64::consts::{SQRT_2, TAU};

    /// `n` samples of a sine at `f` Hz, step `dt`, amplitude `a`, offset `off`.
    fn sine(f: f64, dt: f64, n: usize, a: f64, off: f64) -> Signal {
        let w = TAU * f;
        let x: Vec<f64> = (0..n).map(|j| j as f64 * dt).collect();
        let y: Vec<f64> = (0..n)
            .map(|j| off + a * (w * j as f64 * dt).sin())
            .collect();
        Signal::time("v(sig)", Unit::Volt, x, y)
    }

    fn ramp(y: Vec<f64>) -> Signal {
        let x: Vec<f64> = (0..y.len()).map(|i| i as f64).collect();
        Signal::time("v(a)", Unit::Volt, x, y)
    }

    #[test]
    fn gain_then_rms_is_twice_input_rms() {
        // src → Gain(2) → Rms should be 2× the input RMS.
        let src = sine(1_000.0, 1e-6, 10_000, 2.0, 0.0);
        let input_rms = measure(&src).rms;
        assert!(
            (input_rms - 2.0 / SQRT_2).abs() < 1e-3,
            "input rms {input_rms}"
        );

        let mut p = Patch::new();
        let s = p.input("in");
        let g = p.add(Block::Gain(2.0), &[s]);
        let r = p.add(Block::Rms, &[g]);
        p.output("rms", r);

        let run = p.eval(&[("in", src)]).unwrap();
        let out = run.scalar("rms").unwrap();
        // Hand value: 2 × (2/√2) = 2√2 ≈ 2.828427.
        assert!(
            (out - 2.0 * SQRT_2).abs() < 1e-3,
            "out {out}, want {}",
            2.0 * SQRT_2
        );
        assert!(
            (out - 2.0 * input_rms).abs() < 1e-9,
            "not exactly 2× input rms"
        );
    }

    #[test]
    fn two_input_mul_patch() {
        // a = [1,2,3], b = [4,5,6]; a·b = [4,10,18]; mean = 32/3.
        let a = ramp(vec![1.0, 2.0, 3.0]);
        let b = ramp(vec![4.0, 5.0, 6.0]);

        let mut p = Patch::new();
        let wa = p.input("a");
        let wb = p.input("b");
        let m = p.add(Block::Mul, &[wa, wb]);
        let mean = p.add(Block::Mean, &[m]);
        p.output("prod", m);
        p.output("mean", mean);

        let run = p.eval(&[("a", a), ("b", b)]).unwrap();
        assert_eq!(
            run.signal("prod").unwrap().real().unwrap(),
            &[4.0, 10.0, 18.0]
        );
        assert!((run.scalar("mean").unwrap() - 32.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn clamp_and_limit_bound_correctly() {
        let src = ramp(vec![-5.0, -1.0, 0.0, 2.0, 9.0]);

        let mut p = Patch::new();
        let s = p.input("in");
        let c = p.add(
            Block::Clamp {
                min: -2.0,
                max: 3.0,
            },
            &[s],
        );
        let l = p.add(Block::Limit(4.0), &[s]);
        p.output("clamp", c);
        p.output("limit", l);

        let run = p.eval(&[("in", src)]).unwrap();
        assert_eq!(
            run.signal("clamp").unwrap().real().unwrap(),
            &[-2.0, -1.0, 0.0, 2.0, 3.0]
        );
        assert_eq!(
            run.signal("limit").unwrap().real().unwrap(),
            &[-4.0, -1.0, 0.0, 2.0, 4.0]
        );
    }

    #[test]
    fn low_pass_attenuates_a_high_frequency_component() {
        // A fast 50 kHz tone sampled at 1 MS/s, low-passed well below it: the
        // output RMS collapses. A slow 100 Hz tone passes nearly untouched.
        let fast = sine(50_000.0, 1e-6, 20_000, 1.0, 0.0);
        let slow = sine(100.0, 1e-6, 20_000, 1.0, 0.0);

        let vi_run = |src: Signal| {
            let mut p = Patch::new();
            let s = p.input("in");
            let f = p.add(Block::Filter(Filter::low_pass(1_000.0)), &[s]);
            let r = p.add(Block::Rms, &[f]);
            p.output("rms", r);
            p.eval(&[("in", src)]).unwrap().scalar("rms").unwrap()
        };

        let fast_in = measure(&fast).rms;
        let fast_out = vi_run(fast);
        let slow_out = vi_run(slow);

        assert!(
            fast_out < 0.2 * fast_in,
            "fast tone not attenuated: {fast_out} vs {fast_in}"
        );
        assert!(slow_out > 0.6, "slow tone over-attenuated: {slow_out}");
    }

    #[test]
    fn high_pass_removes_the_dc_component() {
        // DC-offset slow sine → high-pass removes the offset, mean ≈ 0.
        let src = sine(100.0, 1e-6, 20_000, 1.0, 5.0);
        assert!((measure(&src).dc - 5.0).abs() < 0.1);

        let mut p = Patch::new();
        let s = p.input("in");
        let f = p.add(Block::Filter(Filter::high_pass(10.0)), &[s]);
        p.output("out", f);
        let run = p.eval(&[("in", src)]).unwrap();
        let mean = measure(run.signal("out").unwrap()).dc;
        assert!(mean.abs() < 0.2, "DC not removed: mean {mean}");
    }

    #[test]
    fn topo_order_correct_on_a_branching_patch() {
        // in → Gain(2) ─┬→ Abs ─┐
        //               └────────┴→ Add → out
        // Add depends on both Abs and Gain, which depend on in.
        let src = ramp(vec![-1.0, 2.0, -3.0]);

        let mut p = Patch::new();
        let s = p.input("in");
        let g = p.add(Block::Gain(2.0), &[s]); // [-2, 4, -6]
        let a = p.add(Block::Abs, &[g]); // [2, 4, 6]
        let sum = p.add(Block::Add, &[g, a]); // [0, 8, 0]
        p.output("sum", sum);

        let run = p.eval(&[("in", src)]).unwrap();
        assert_eq!(run.signal("sum").unwrap().real().unwrap(), &[0.0, 8.0, 0.0]);
    }

    #[test]
    fn a_cycle_is_reported_without_hanging() {
        // Build two gains, then rewire the first to read the second → 0↔1 cycle.
        let mut p = Patch::new();
        let s = p.input("in");
        let g0 = p.add(Block::Gain(1.0), &[s]);
        let g1 = p.add(Block::Gain(1.0), &[g0]);
        p.set_inputs(g0, &[g1]); // g0 ← g1 ← g0
        p.output("out", g1);

        let err = p.eval(&[("in", ramp(vec![1.0]))]).unwrap_err();
        match err {
            PatchError::Cycle(nodes) => {
                assert!(
                    nodes.contains(&0) && nodes.contains(&1),
                    "cycle nodes {nodes:?}"
                );
            }
            other => panic!("expected a cycle error, got {other:?}"),
        }
    }

    #[test]
    fn missing_input_is_a_clean_error() {
        let mut p = Patch::new();
        let s = p.input("in");
        let g = p.add(Block::Gain(2.0), &[s]);
        p.output("out", g);
        let err = p.eval(&[("wrong", ramp(vec![1.0]))]).unwrap_err();
        assert_eq!(err, PatchError::MissingInput("in".to_string()));
    }

    #[test]
    fn arity_mismatch_is_reported() {
        let mut p = Patch::new();
        let s = p.input("in");
        let bad = p.add(Block::Add, &[s]); // Add needs 2 inputs
        p.output("out", bad);
        let err = p.eval(&[("in", ramp(vec![1.0]))]).unwrap_err();
        assert_eq!(
            err,
            PatchError::Arity {
                node: 0,
                expected: 2,
                got: 1
            }
        );
    }

    #[test]
    fn log_tap_records_its_input_and_passes_through() {
        let src = ramp(vec![1.0, 2.0, 3.0]);
        let mut p = Patch::new();
        let s = p.input("in");
        let g = p.add(Block::Gain(2.0), &[s]);
        let tap = p.add(Block::Log("after_gain".to_string()), &[g]);
        p.output("out", tap);

        let run = p.eval(&[("in", src)]).unwrap();
        // Passthrough: the output equals the gained signal.
        assert_eq!(run.signal("out").unwrap().real().unwrap(), &[2.0, 4.0, 6.0]);
        // And the tap recorded it under its label.
        assert_eq!(run.logs().len(), 1);
        assert_eq!(run.logs()[0].0, "after_gain");
        assert_eq!(
            run.logs()[0].1.as_signal().unwrap().real().unwrap(),
            &[2.0, 4.0, 6.0]
        );
    }

    #[test]
    fn virtual_instrument_reruns_on_new_inputs() {
        // A saved "RMS meter": in → Rms → out, run on two different signals.
        let mut p = Patch::new();
        let s = p.input("in");
        let r = p.add(Block::Rms, &[s]);
        p.output("rms", r);
        let vi = VirtualInstrument::new("rms-meter", p);

        let one = vi.run(&[("in", ramp(vec![3.0, 3.0, 3.0]))]).unwrap();
        assert!((one.scalar("rms").unwrap() - 3.0).abs() < 1e-12);

        let two = vi.run(&[("in", ramp(vec![0.0, 0.0, 0.0, 0.0]))]).unwrap();
        assert!(two.scalar("rms").unwrap().abs() < 1e-12);

        assert_eq!(vi.name(), "rms-meter");
        assert!(vi.render().contains("Rms"));
    }

    #[test]
    fn test_sequence_passes_and_names_the_failing_check() {
        // Patch: in → Gain(2) → {Rms, Peak}. Input is a ±1 sine → gained ±2.
        let src = sine(1_000.0, 1e-6, 10_000, 1.0, 0.0);
        let mut p = Patch::new();
        let s = p.input("in");
        let g = p.add(Block::Gain(2.0), &[s]);
        let rms = p.add(Block::Rms, &[g]);
        let pk = p.add(Block::Peak, &[g]);
        p.output("rms", rms);
        p.output("peak", pk);
        let run = p.eval(&[("in", src)]).unwrap();

        // Gained RMS ≈ 2/√2 ≈ 1.414; peak ≈ 2.
        let pass = TestSequence::new("seq")
            .check(Check::new("rms band", "rms", Bound::Within(1.3, 1.5)))
            .check(Check::new("peak limit", "peak", Bound::LessThan(2.1)))
            .run(&run);
        assert!(pass.passed(), "report:\n{}", pass.render());
        assert_eq!(pass.failures().count(), 0);

        // Now a sequence that must fail, naming the failing check.
        let fail = TestSequence::new("seq")
            .check(Check::new("rms band", "rms", Bound::Within(1.3, 1.5)))
            .check(Check::new("peak too tight", "peak", Bound::LessThan(1.0)))
            .run(&run);
        assert!(!fail.passed());
        let failing: Vec<&str> = fail.failures().map(|r| r.name.as_str()).collect();
        assert_eq!(failing, vec!["peak too tight"]);
        assert!(fail.render().contains("[FAIL] peak too tight"));
    }

    // ── external ProcessingBlock plugins (CP-UX-M4.1) ──────────────────────

    /// A one-input plugin that squares every real sample, and rejects a scalar
    /// (to prove a plugin can report a clean type error).
    #[derive(Debug)]
    struct Square;
    impl ProcessingBlock for Square {
        fn name(&self) -> String {
            "Square".to_string()
        }
        fn arity(&self) -> usize {
            1
        }
        fn eval(&self, inputs: &[Port]) -> Result<Port, PatchError> {
            match &inputs[0] {
                Port::Signal(s) => {
                    let y = s.real().unwrap_or(&[]);
                    let sq: Vec<f64> = y.iter().map(|v| v * v).collect();
                    Ok(Port::Signal(Signal::time(
                        s.name.clone(),
                        s.unit,
                        s.x.clone(),
                        sq,
                    )))
                }
                Port::Scalar(_) => Err(PatchError::TypeMismatch {
                    node: usize::MAX,
                    detail: "Square needs a signal, got a scalar".to_string(),
                }),
            }
        }
    }

    /// A two-input plugin: the elementwise maximum of two real signals, broadcast
    /// via the built-in grid handling by delegating to the public API.
    #[derive(Debug)]
    struct WindowAvg {
        width: usize,
    }
    impl ProcessingBlock for WindowAvg {
        fn name(&self) -> String {
            format!("WindowAvg({})", self.width)
        }
        fn arity(&self) -> usize {
            1
        }
        fn eval(&self, inputs: &[Port]) -> Result<Port, PatchError> {
            let s = inputs[0]
                .as_signal()
                .ok_or_else(|| PatchError::TypeMismatch {
                    node: usize::MAX,
                    detail: "WindowAvg needs a signal".to_string(),
                })?;
            let y = s.real().unwrap_or(&[]);
            let w = self.width.max(1);
            // Trailing moving average over up to `w` samples.
            let avg: Vec<f64> = (0..y.len())
                .map(|i| {
                    let lo = i.saturating_sub(w - 1);
                    let win = &y[lo..=i];
                    win.iter().sum::<f64>() / win.len() as f64
                })
                .collect();
            Ok(Port::Signal(Signal::time(
                s.name.clone(),
                s.unit,
                s.x.clone(),
                avg,
            )))
        }
    }

    #[test]
    fn plugin_block_evaluates_and_composes_with_builtins() {
        // in → Square → Gain(2): [1,2,3] → [1,4,9] → [2,8,18], and Mean = 28/3.
        let src = ramp(vec![1.0, 2.0, 3.0]);
        let mut p = Patch::new();
        let i = p.input("in");
        let sq = p.add(Block::plugin(Square), &[i]);
        let g = p.add(Block::Gain(2.0), &[sq]);
        let mean = p.add(Block::Mean, &[g]);
        p.output("scaled", g);
        p.output("mean", mean);

        let run = p.eval(&[("in", src)]).unwrap();
        assert_eq!(
            run.signal("scaled").unwrap().real().unwrap(),
            &[2.0, 8.0, 18.0]
        );
        assert!((run.scalar("mean").unwrap() - 28.0 / 3.0).abs() < 1e-12);
        // The plugin's name rides the render.
        assert!(p.render().contains("Square"), "{}", p.render());
    }

    #[test]
    fn plugin_block_type_error_is_reported() {
        // Rms yields a scalar; feeding it to Square (which needs a signal) must
        // surface the plugin's own TypeMismatch, not a panic.
        let src = ramp(vec![1.0, 2.0, 3.0]);
        let mut p = Patch::new();
        let i = p.input("in");
        let r = p.add(Block::Rms, &[i]);
        let bad = p.add(Block::plugin(Square), &[r]);
        p.output("out", bad);

        let err = p.eval(&[("in", src)]).unwrap_err();
        assert!(
            matches!(err, PatchError::TypeMismatch { .. }),
            "want a type mismatch, got {err:?}"
        );
    }

    #[test]
    fn plugin_block_arity_is_honored() {
        // A plugin declaring arity 1 given two wires is an Arity error, checked by
        // the patcher before the plugin ever runs.
        let a = ramp(vec![1.0, 2.0]);
        let b = ramp(vec![3.0, 4.0]);
        let mut p = Patch::new();
        let wa = p.input("a");
        let wb = p.input("b");
        let node = p.add(Block::plugin(Square), &[wa, wb]);
        p.output("out", node);

        let err = p.eval(&[("a", a), ("b", b)]).unwrap_err();
        assert_eq!(
            err,
            PatchError::Arity {
                node: 0,
                expected: 1,
                got: 2
            }
        );
    }

    #[test]
    fn windowed_average_plugin_smooths() {
        // A width-3 trailing average of [0,3,6,9]: [0, 1.5, 3, 6].
        let src = ramp(vec![0.0, 3.0, 6.0, 9.0]);
        let mut p = Patch::new();
        let i = p.input("in");
        let avg = p.add(Block::plugin(WindowAvg { width: 3 }), &[i]);
        p.output("avg", avg);

        let run = p.eval(&[("in", src)]).unwrap();
        assert_eq!(
            run.signal("avg").unwrap().real().unwrap(),
            &[0.0, 1.5, 3.0, 6.0]
        );
        assert!(p.render().contains("WindowAvg(3)"));
    }
}
