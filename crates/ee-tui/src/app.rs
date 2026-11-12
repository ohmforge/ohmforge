//! The live, dockable TUI application state machine (`CP-UX-M1.4`).
//!
//! This is the first *interactive* layer of `ee-tui`. Everything below the
//! terminal I/O is a pure state machine: focus switching, panel resize/dock, the
//! command palette (fuzzy filter + selection), and the shell input line are all
//! methods that take an abstract [`AppEvent`] and mutate [`App`], and
//! [`App::render`] is a pure function from state to a single `String` of a given
//! width and height. No `crossterm`, no raw mode, no `tty` — so the whole thing
//! is unit-testable the way [`Screen`](crate::Screen) and [`Plot`]
//! already are.
//!
//! The crossterm raw-mode glue (reading real key/mouse events, drawing the frame)
//! lives in the sibling [`run`](mod@crate::run) module as a thin `pub fn run` wrapper
//! the binary calls; it only translates OS events into [`AppEvent`]s and prints
//! what [`App::render`] returns. That split is deliberate: the gate below this
//! comment is all pure logic.
//!
//! ```text
//! ┌▸ schematic ───────┐┌─ scope ───────────┐
//! │ V1  V-source  5V  ││  tran            ⡠⠊│
//! │ R1  Resistor 4.7k ││     ┤        ⡠⠔⠊⠁  │
//! └───────────────────┘└───────────────────┘
//! ┌─ shell ─────────────────────────────────┐
//! │ > run tran 1m                            │
//! │ scope updated                            │
//! └─────────────────────────────────────────┘
//! ```
//!
//! The application does not itself know how to simulate — command execution is
//! injected through the [`Backend`] trait, which `ee-shell`'s `Shell` implements.
//! That keeps `ee-tui` free of any dependency on `ee-shell`/`ee-sim` (the
//! dependency arrow runs the other way) and lets the tests drive the app with a
//! trivial mock.

use ee_core::graph::Circuit;
use ee_core::units::Unit;
use ee_core::value::Value;

use crate::current::{branch_currents, BranchCurrents};
use crate::overlay::{NodeVoltages, Overlay};
use crate::playback::Playback;
use crate::plot::Plot;
use crate::schematic::{fmt_value, Schematic};
use crate::screen::Pane;

/// How far one resize step moves a split boundary, as a fraction of the axis.
const RESIZE_STEP: f64 = 0.07;

/// Horizontal split bounds (left-column width as a fraction of the total width).
const H_MIN: f64 = 0.2;
const H_MAX: f64 = 0.8;

/// Vertical split bounds (top-row height as a fraction of the total height).
const V_MIN: f64 = 0.25;
const V_MAX: f64 = 0.85;

/// How many log lines the shell pane retains.
const LOG_CAP: usize = 500;

/// Minimum width/height the layout is computed against (panes must fit a frame).
const MIN_W: usize = 16;
const MIN_H: usize = 6;

/// A source of command execution and the circuit under construction.
///
/// The live [`App`] owns one of these and calls it when the user runs a shell
/// line. `ee-shell`'s `Shell` implements it (mapping its `ShellError` to a
/// `String`), which is the whole integration seam between the TUI and the shell —
/// `ee-tui` never names `ee-shell` or `ee-sim`.
pub trait Backend {
    /// Execute one command line, returning its text output or a friendly error.
    fn execute(&mut self, line: &str) -> Result<String, String>;
    /// Borrow the circuit being built, for the schematic/overlay pane.
    fn circuit(&self) -> &Circuit;
    /// Solve the DC operating point of the current circuit, for the live overlay
    /// (`CP-UX-M6.1`). The default returns an empty solve (every net unknown); a
    /// backend wired to the solver overrides it so the overlay reflects edits and
    /// live tuning immediately. Decoupled from `ee-sim` by the [`NodeVoltages`]
    /// data type, exactly as [`NodeVoltages::from_signals`] is — `ee-tui` names no
    /// solver crate.
    fn op_point(&self) -> NodeVoltages {
        NodeVoltages::new()
    }

    /// The cached transient result — the probed-node voltage time series from the
    /// last transient `run` — for the scrubbable playback (`CP-UX-M7.1`). The
    /// default is empty (no transient yet); a solver-backed backend overrides it
    /// so the playhead has frames to scrub. Decoupled from `ee-sim` by the
    /// [`Signal`](ee_core::signal::Signal) type, exactly as [`op_point`](Backend::op_point)
    /// is by [`NodeVoltages`].
    fn transient(&self) -> Vec<ee_core::signal::Signal> {
        Vec::new()
    }
}

/// The three docked panels, in focus-cycle order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelId {
    /// Top-left: the schematic list (or op-point overlay when toggled).
    Schematic,
    /// Top-right: the waveform scope (last `run` render).
    Scope,
    /// Full-width bottom: the shell input line and command log.
    Shell,
}

/// The panels in their fixed cycle order.
const PANELS: [PanelId; 3] = [PanelId::Schematic, PanelId::Scope, PanelId::Shell];

/// A rectangular region of the screen, in character cells (origin top-left).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    /// Whether the cell `(col, row)` falls inside this rectangle.
    pub fn contains(&self, col: usize, row: usize) -> bool {
        col >= self.x && col < self.x + self.w && row >= self.y && row < self.y + self.h
    }
}

/// The two split fractions that place the three panels.
///
/// `h_split` is the left column's width as a fraction of the total; `v_split` is
/// the top row's height as a fraction of the total. Both are kept inside sane
/// bounds so every pane keeps a usable minimum size.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub h_split: f64,
    pub v_split: f64,
}

impl Default for Layout {
    fn default() -> Self {
        // Top-heavy, evenly split left/right — matches the static `Screen` layout.
        Self {
            h_split: 0.5,
            v_split: 0.66,
        }
    }
}

/// A key code, modifier-free (modifiers travel in [`KeyEvent`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
}

/// A key press: a [`KeyCode`] plus whether Ctrl was held.
///
/// This is the abstract key the state machine reacts to; [`run`](fn@crate::run) maps
/// real crossterm key events onto it. Only the Ctrl modifier matters to the app
/// (Ctrl-P palette, Ctrl-C quit, Ctrl-arrows resize), so that is all we carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub ctrl: bool,
}

impl KeyEvent {
    /// A plain (no-modifier) key.
    pub fn new(code: KeyCode) -> Self {
        Self { code, ctrl: false }
    }

    /// A plain printable character key.
    pub fn char(c: char) -> Self {
        Self::new(KeyCode::Char(c))
    }

    /// This key with the Ctrl modifier set.
    pub fn with_ctrl(mut self) -> Self {
        self.ctrl = true;
        self
    }
}

/// What a mouse event is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    /// A button went down at the cell.
    Down,
    /// The scroll wheel moved up.
    ScrollUp,
    /// The scroll wheel moved down.
    ScrollDown,
}

/// A mouse event at a terminal cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub col: usize,
    pub row: usize,
}

/// An abstract input event the state machine understands.
///
/// [`run`](fn@crate::run) produces these from crossterm; the tests build them
/// directly. Nothing here depends on a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(usize, usize),
}

/// A single interactive circuit edit.
///
/// This is the whole edit surface of the app: every interactive edit (a key
/// chord, a committed prompt, a palette entry) is reduced to one of these, and
/// [`App::apply_edit`] turns it into the equivalent shell command line fed to the
/// [`Backend`]. So the shell's `add`/`wire`/`remove` grammar is the *single*
/// mutation path — the live editor and the typed shell line share it, and the
/// circuit the schematic/overlay draws is the one the sim runs against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditAction {
    /// Place a part: `add <reference> <node_a> <node_b> <value>`.
    Place {
        reference: String,
        node_a: String,
        node_b: String,
        value: String,
    },
    /// Merge two named nodes into one net: `wire <node_a> <node_b>`.
    Wire { node_a: String, node_b: String },
    /// Delete a placed part by reference: `remove <reference>`.
    Remove { reference: String },
    /// Retarget a component's value: `set <reference>.value <value>`. This is the
    /// live-tuning mutation (`CP-UX-M6.1`); it lowers to the shell's `set` verb so
    /// a `+`/`-` nudge and a typed `set` share the one mutation path, and the
    /// re-solved op-point/overlay reflects it on the next render.
    Tune { reference: String, value: String },
}

impl EditAction {
    /// The shell command line this edit is equivalent to.
    fn to_command(&self) -> String {
        match self {
            EditAction::Place {
                reference,
                node_a,
                node_b,
                value,
            } => format!("add {reference} {node_a} {node_b} {value}"),
            EditAction::Wire { node_a, node_b } => format!("wire {node_a} {node_b}"),
            EditAction::Remove { reference } => format!("remove {reference}"),
            EditAction::Tune { reference, value } => format!("set {reference}.value {value}"),
        }
    }
}

/// The multiplicative step one `+`/`-` tune nudge applies to a component value.
const TUNE_STEP: f64 = 1.1;

/// What the edit-mode mini-prompt is currently collecting, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum EditPrompt {
    /// No prompt: arrow keys move the selection; `a`/`w`/`d` start an edit.
    #[default]
    None,
    /// Collecting `<ref> <a> <b> <value>` for a part placement.
    Place,
    /// Collecting `<a> <b>` for a wire.
    Wire,
}

/// The interactive edit-mode sub-state.
#[derive(Debug, Default)]
struct Edit {
    /// Whether edit mode is active (keystrokes edit the circuit, not the shell).
    active: bool,
    /// Selection cursor as an index into the circuit's part list.
    selected: usize,
    /// Which argument prompt is open, if any.
    prompt: EditPrompt,
    /// The text typed into the current prompt (whitespace-separated fields).
    buffer: String,
}

/// A command-palette action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Focus(PanelId),
    GrowFocus,
    ShrinkFocus,
    ToggleOverlay,
    ToggleEdit,
    ClearLog,
    Quit,
}

/// One entry in the command palette.
struct Command {
    name: &'static str,
    action: Action,
}

/// The fixed palette command list (fuzzy-filtered by the query).
const COMMANDS: &[Command] = &[
    Command {
        name: "focus schematic",
        action: Action::Focus(PanelId::Schematic),
    },
    Command {
        name: "focus scope",
        action: Action::Focus(PanelId::Scope),
    },
    Command {
        name: "focus shell",
        action: Action::Focus(PanelId::Shell),
    },
    Command {
        name: "grow panel",
        action: Action::GrowFocus,
    },
    Command {
        name: "shrink panel",
        action: Action::ShrinkFocus,
    },
    Command {
        name: "toggle overlay",
        action: Action::ToggleOverlay,
    },
    Command {
        name: "edit mode",
        action: Action::ToggleEdit,
    },
    Command {
        name: "clear log",
        action: Action::ClearLog,
    },
    Command {
        name: "quit",
        action: Action::Quit,
    },
];

/// The command-palette sub-state.
#[derive(Debug, Default)]
struct Palette {
    open: bool,
    query: String,
    selected: usize,
}

/// The live application state.
///
/// Generic over its [`Backend`] so the tests can drive it with a mock and the
/// binary drives it with `ee-shell`'s `Shell`. Build with [`App::new`], feed it
/// [`AppEvent`]s via [`handle_event`](App::handle_event), and draw it with
/// [`render`](App::render).
pub struct App<B: Backend> {
    backend: B,
    layout: Layout,
    focus: PanelId,
    palette: Palette,
    /// The command log shown in the shell pane (oldest first).
    log: Vec<String>,
    /// The last waveform render (the scope pane's body).
    scope: String,
    /// The shell input line being typed.
    input: String,
    /// Whether the left pane shows the op-point overlay rather than the list.
    overlay: bool,
    /// Interactive schematic-editing sub-state (`CP-UX-M5.1`).
    edit: Edit,
    /// The last solved operating point, refreshed after every mutation so the
    /// overlay reflects edits and live tuning (`CP-UX-M6.1`).
    voltages: NodeVoltages,
    /// Per-branch currents derived from `voltages`, for the flow animation.
    currents: BranchCurrents,
    /// The cached transient result and its playhead — a scrubbable "play" over the
    /// last transient `run` (`CP-UX-M7.1`). Empty until a transient is run.
    playback: Playback,
    /// Whether the user is actively scrubbing the scope transport. Only while this
    /// is set does the overlay follow the playhead; otherwise it shows the live DC
    /// op-point / tuned solution, so live-tune feedback keeps working after a
    /// transient has been loaded (`CP-UX-M8.1`). Cleared on any command/edit.
    scrubbing: bool,
    /// Animation frame counter — the run loop bumps it so the flow arrows march.
    tick: u64,
    /// Last known terminal size, for mouse hit-testing between renders.
    size: (usize, usize),
    should_quit: bool,
}

impl<B: Backend> App<B> {
    /// A fresh app over `backend`, focused on the shell with an empty scope.
    pub fn new(backend: B) -> Self {
        let voltages = backend.op_point();
        let currents = branch_currents(backend.circuit(), &voltages);
        Self {
            backend,
            layout: Layout::default(),
            focus: PanelId::Shell,
            palette: Palette::default(),
            log: vec!["type a command, or press Ctrl-P for the palette".to_string()],
            scope: String::new(),
            input: String::new(),
            overlay: false,
            edit: Edit::default(),
            voltages,
            currents,
            playback: Playback::new(),
            scrubbing: false,
            tick: 0,
            size: (100, 30),
            should_quit: false,
        }
    }

    /// Whether the run loop should exit.
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// The currently focused panel.
    pub fn focus(&self) -> PanelId {
        self.focus
    }

    /// Record the terminal size (the run loop calls this before each draw).
    pub fn set_size(&mut self, width: usize, height: usize) {
        self.size = (width.max(MIN_W), height.max(MIN_H));
    }

    // ── event handling ──────────────────────────────────────────────────────

    /// React to one input event, mutating state. Pure — no I/O.
    pub fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.on_key(key),
            AppEvent::Mouse(m) => self.on_mouse(m),
            AppEvent::Resize(w, h) => self.set_size(w, h),
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Ctrl-C always quits, palette open or not.
        if key.ctrl && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if self.palette.open {
            self.on_palette_key(key);
            return;
        }
        // Ctrl-P opens the palette from anywhere.
        if key.ctrl && key.code == KeyCode::Char('p') {
            self.open_palette();
            return;
        }
        // Ctrl-arrows resize the split boundaries.
        if key.ctrl {
            match key.code {
                KeyCode::Left => return self.nudge_split(-RESIZE_STEP, 0.0),
                KeyCode::Right => return self.nudge_split(RESIZE_STEP, 0.0),
                KeyCode::Up => return self.nudge_split(0.0, -RESIZE_STEP),
                KeyCode::Down => return self.nudge_split(0.0, RESIZE_STEP),
                _ => {}
            }
        }
        // Edit mode takes over the keyboard until Esc leaves it.
        if self.edit.active {
            self.on_edit_key(key);
            return;
        }
        match key.code {
            KeyCode::Tab => self.focus_next(),
            KeyCode::BackTab => self.focus_prev(),
            // `e` on the schematic pane enters interactive edit mode.
            KeyCode::Char('e') if self.focus == PanelId::Schematic => self.enter_edit_mode(),
            // The scope pane is the transient-playback transport.
            _ if self.focus == PanelId::Scope => self.on_scope_key(key),
            _ if self.focus == PanelId::Shell => self.on_shell_key(key),
            _ => {}
        }
    }

    /// Transient-playback transport keys on the scope pane (`CP-UX-M7.1`): space
    /// toggles play/pause, Left/Right step the playhead one sample, and `,`/`.`
    /// jump to the first/last frame. All clamp at the ends; an empty playback is a
    /// no-op.
    fn on_scope_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(' ') => self.playback.toggle(),
            KeyCode::Left => self.playback.step(-1),
            KeyCode::Right => self.playback.step(1),
            KeyCode::Char(',') => self.playback.seek(0),
            KeyCode::Char('.') => self.playback.seek(usize::MAX),
            _ => return,
        }
        // Any transport action means the user is scrubbing: hand the overlay to the
        // playhead until the next command/edit (`CP-UX-M8.1`).
        if !self.playback.is_empty() {
            self.scrubbing = true;
        }
    }

    fn on_shell_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(c) => self.input.push(c),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Enter => self.run_input(),
            _ => {}
        }
    }

    fn on_palette_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.palette.open = false,
            KeyCode::Up => self.palette.selected = self.palette.selected.saturating_sub(1),
            KeyCode::Down => {
                let last = self.palette_matches().len().saturating_sub(1);
                self.palette.selected = (self.palette.selected + 1).min(last);
            }
            KeyCode::Enter => self.run_selected_command(),
            KeyCode::Backspace => {
                self.palette.query.pop();
                self.palette.selected = 0;
            }
            KeyCode::Char(c) => {
                self.palette.query.push(c);
                self.palette.selected = 0;
            }
            _ => {}
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            // While the palette is open a click interacts with it instead of
            // falling through to the pane behind: on a command row it selects and
            // runs it; anywhere outside the box it dismisses the palette
            // (`CP-UX-M8.1`).
            MouseKind::Down if self.palette.open => {
                let (w, h) = self.size;
                let rect = self.palette_rect(w, h);
                if !rect.contains(m.col, m.row) {
                    self.palette.open = false;
                    return;
                }
                // Inside the box: rows are border(y), query(y+1), then matches.
                let first_match_row = rect.y + 2;
                if m.row >= first_match_row {
                    let idx = m.row - first_match_row;
                    if idx < self.palette_matches().len() {
                        self.palette.selected = idx;
                        self.run_selected_command();
                    }
                }
            }
            // A click focuses whichever panel it lands in.
            MouseKind::Down => {
                let (w, h) = self.size;
                for (id, rect) in self.geometry(w, h) {
                    if rect.contains(m.col, m.row) {
                        self.focus = id;
                        break;
                    }
                }
            }
            // Scrolling moves the palette selection when it is open.
            MouseKind::ScrollUp if self.palette.open => {
                self.palette.selected = self.palette.selected.saturating_sub(1);
            }
            MouseKind::ScrollDown if self.palette.open => {
                let last = self.palette_matches().len().saturating_sub(1);
                self.palette.selected = (self.palette.selected + 1).min(last);
            }
            _ => {}
        }
    }

    // ── focus / resize ────────────────────────────────────────────────────

    /// Move focus to the next panel in cycle order.
    pub fn focus_next(&mut self) {
        let i = PANELS.iter().position(|&p| p == self.focus).unwrap_or(0);
        self.focus = PANELS[(i + 1) % PANELS.len()];
    }

    /// Move focus to the previous panel in cycle order.
    pub fn focus_prev(&mut self) {
        let i = PANELS.iter().position(|&p| p == self.focus).unwrap_or(0);
        self.focus = PANELS[(i + PANELS.len() - 1) % PANELS.len()];
    }

    /// Nudge the split boundaries by `(dh, dv)`, clamped to their bounds.
    fn nudge_split(&mut self, dh: f64, dv: f64) {
        self.layout.h_split = (self.layout.h_split + dh).clamp(H_MIN, H_MAX);
        self.layout.v_split = (self.layout.v_split + dv).clamp(V_MIN, V_MAX);
    }

    /// Grow the focused panel at its neighbours' expense.
    pub fn grow_focused(&mut self) {
        match self.focus {
            PanelId::Schematic => self.nudge_split(RESIZE_STEP, 0.0),
            PanelId::Scope => self.nudge_split(-RESIZE_STEP, 0.0),
            PanelId::Shell => self.nudge_split(0.0, -RESIZE_STEP),
        }
    }

    /// Shrink the focused panel, giving the space to its neighbours.
    pub fn shrink_focused(&mut self) {
        match self.focus {
            PanelId::Schematic => self.nudge_split(-RESIZE_STEP, 0.0),
            PanelId::Scope => self.nudge_split(RESIZE_STEP, 0.0),
            PanelId::Shell => self.nudge_split(0.0, RESIZE_STEP),
        }
    }

    // ── palette ─────────────────────────────────────────────────────────────

    fn open_palette(&mut self) {
        self.palette.open = true;
        self.palette.query.clear();
        self.palette.selected = 0;
    }

    /// Whether the command palette is open.
    pub fn palette_open(&self) -> bool {
        self.palette.open
    }

    /// Indices into [`COMMANDS`] matching the current query, best match first.
    ///
    /// The filter is a case-insensitive fuzzy subsequence match (see
    /// [`fuzzy_score`]); ties break toward the earliest command in the list so
    /// the ordering is deterministic.
    fn palette_matches(&self) -> Vec<usize> {
        let mut scored: Vec<(usize, i32)> = COMMANDS
            .iter()
            .enumerate()
            .filter_map(|(i, c)| fuzzy_score(&self.palette.query, c.name).map(|s| (i, s)))
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        scored.into_iter().map(|(i, _)| i).collect()
    }

    /// The matching command names, best first (for tests and the palette view).
    pub fn palette_filtered(&self) -> Vec<&'static str> {
        self.palette_matches()
            .into_iter()
            .map(|i| COMMANDS[i].name)
            .collect()
    }

    fn run_selected_command(&mut self) {
        let matches = self.palette_matches();
        let Some(&idx) = matches.get(self.palette.selected) else {
            self.palette.open = false;
            return;
        };
        self.palette.open = false;
        match COMMANDS[idx].action {
            Action::Focus(p) => self.focus = p,
            Action::GrowFocus => self.grow_focused(),
            Action::ShrinkFocus => self.shrink_focused(),
            Action::ToggleOverlay => self.overlay = !self.overlay,
            Action::ToggleEdit => self.toggle_edit_mode(),
            Action::ClearLog => self.log.clear(),
            Action::Quit => self.should_quit = true,
        }
    }

    // ── shell integration ─────────────────────────────────────────────────

    /// Run the current input line through the backend and record the result.
    fn run_input(&mut self) {
        let line = self.input.trim().to_string();
        self.input.clear();
        if line.is_empty() {
            return;
        }
        self.run_line(&line);
    }

    /// Execute one command line through the backend and record its result: a
    /// waveform frame updates the scope, other output is logged line by line, and
    /// an error is logged with an `error:` prefix (never a panic). Shared by the
    /// typed shell line and the interactive editor.
    fn run_line(&mut self, line: &str) {
        self.push_log(format!("> {line}"));
        // A new command returns the overlay to the live op-point until the user
        // deliberately scrubs again (`CP-UX-M8.1`).
        self.scrubbing = false;
        match self.backend.execute(line) {
            Ok(out) if is_scope(&out) => {
                self.scope = out;
                self.push_log("scope updated".to_string());
            }
            Ok(out) => {
                for l in out.lines() {
                    self.push_log(l.to_string());
                }
            }
            Err(e) => self.push_log(format!("error: {e}")),
        }
        // Reload the playback from the backend's transient cache (now empty after a
        // mutating command, since the stored waveform no longer matches the circuit)
        // and re-solve the op-point so the overlay tracks the latest circuit.
        self.refresh_playback();
        self.refresh_solution();
    }

    /// Re-solve the operating point and rederive the branch currents from the
    /// current circuit. Called after every command/edit so the overlay — voltages
    /// and the current-flow animation alike — tracks the latest circuit. Pure (the
    /// solve is the backend's; this only caches its data).
    fn refresh_solution(&mut self) {
        self.voltages = self.backend.op_point();
        self.currents = branch_currents(self.backend.circuit(), &self.voltages);
    }

    /// Reload the scrubbable playback from the backend's cached transient result.
    /// Called after a transient `run` updates the scope, so the playhead has the
    /// just-computed frames to scrub (reset to the start, paused). Pure — the solve
    /// is the backend's; this only caches its signals (`CP-UX-M7.1`).
    fn refresh_playback(&mut self) {
        self.playback = Playback::from_signals(&self.backend.transient());
    }

    /// Advance the animation one frame: bump the current-flow phase and, when the
    /// transient playback is playing, march its playhead one sample. The run loop
    /// calls this on each redraw tick, so a single seam drives both the flow arrows
    /// and the "play" scrub (`CP-UX-M6.1`/`CP-UX-M7.1`). Pure state bump — no I/O.
    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        self.playback.advance();
    }

    /// Whether anything on screen is actually animating, so the run loop knows
    /// whether to drive [`tick`](App::tick) at all (`CP-UX-M8.1`).
    ///
    /// Two things animate: a *playing* transient playback (the playhead marches),
    /// and the current-flow arrows — but the arrows only move when the overlay is
    /// shown **and** some branch carries current. An idle screen (overlay off, or
    /// an empty/zero-current circuit, playback paused) animates nothing, so the
    /// loop can block for input instead of repainting on a timer. Pure, no I/O.
    pub fn is_animating(&self) -> bool {
        self.playback.playing() || (self.overlay && self.currents.max_magnitude() > 0.0)
    }

    // ── transient playback (CP-UX-M7.1) ────────────────────────────────────

    /// The cached transient playback (for tests and the scope/overlay panes).
    pub fn playback(&self) -> &Playback {
        &self.playback
    }

    /// Start the transient playback advancing on each tick.
    pub fn play(&mut self) {
        self.playback.play();
        self.mark_scrubbing();
    }

    /// Pause the transient playback, leaving the playhead where it is.
    pub fn pause(&mut self) {
        self.playback.pause();
    }

    /// Toggle the transient playback between playing and paused.
    pub fn toggle_play(&mut self) {
        self.playback.toggle();
        self.mark_scrubbing();
    }

    /// Step the playhead by `delta` samples (negative rewinds), clamped at the ends.
    pub fn step_playhead(&mut self, delta: isize) {
        self.playback.step(delta);
        self.mark_scrubbing();
    }

    /// Seek the playhead to an absolute sample index, clamped to the last frame.
    pub fn seek_playhead(&mut self, index: usize) {
        self.playback.seek(index);
        self.mark_scrubbing();
    }

    /// Mark the overlay as following the playhead (a non-empty playback is scrubbed).
    fn mark_scrubbing(&mut self) {
        if !self.playback.is_empty() {
            self.scrubbing = true;
        }
    }

    /// The node voltages the overlay should paint: the transient sample at the
    /// playhead *only while the user is actively scrubbing* the scope transport,
    /// else the live DC op-point / tuned solution. Gating on [`scrubbing`] (rather
    /// than merely "a transient is loaded") is what lets live-tune feedback keep
    /// working after a `run tran` — the overlay reflects each tune's re-solved
    /// op-point until the user deliberately scrubs the playhead (`CP-UX-M8.1`).
    fn overlay_voltages(&self) -> NodeVoltages {
        if self.scrubbing && !self.playback.is_empty() {
            self.playback.voltages()
        } else {
            self.voltages.clone()
        }
    }

    /// Whether the overlay is currently following the scrubbed playhead.
    pub fn is_scrubbing(&self) -> bool {
        self.scrubbing && !self.playback.is_empty()
    }

    /// The last solved operating-point node voltages (for tests and the overlay).
    pub fn op_voltages(&self) -> &NodeVoltages {
        &self.voltages
    }

    /// The branch currents derived from the last solve (for tests and the overlay).
    pub fn branch_currents(&self) -> &BranchCurrents {
        &self.currents
    }

    // ── interactive editing (CP-UX-M5.1) ──────────────────────────────────

    /// Whether interactive edit mode is active.
    pub fn edit_mode(&self) -> bool {
        self.edit.active
    }

    /// The reference of the part under the selection cursor, if any.
    pub fn selected_part(&self) -> Option<String> {
        self.backend
            .circuit()
            .parts()
            .get(self.edit.selected)
            .map(|p| p.reference.clone())
    }

    /// Apply one [`EditAction`] to the circuit by running the equivalent shell
    /// command through the backend. The result (or a friendly error) is logged and
    /// the schematic/overlay pane reflects it on the next render. Pure — no I/O.
    pub fn apply_edit(&mut self, action: EditAction) {
        let line = action.to_command();
        self.run_line(&line);
        self.clamp_selection();
    }

    /// Enter edit mode, focusing the schematic and clearing any stale prompt.
    fn enter_edit_mode(&mut self) {
        self.edit.active = true;
        self.edit.prompt = EditPrompt::None;
        self.edit.buffer.clear();
        self.focus = PanelId::Schematic;
        self.clamp_selection();
    }

    /// Leave edit mode, discarding any in-progress prompt.
    fn exit_edit_mode(&mut self) {
        self.edit.active = false;
        self.edit.prompt = EditPrompt::None;
        self.edit.buffer.clear();
    }

    fn toggle_edit_mode(&mut self) {
        if self.edit.active {
            self.exit_edit_mode();
        } else {
            self.enter_edit_mode();
        }
    }

    /// Keep the selection cursor within the current part list (0 when empty).
    fn clamp_selection(&mut self) {
        let n = self.backend.circuit().parts().len();
        self.edit.selected = if n == 0 {
            0
        } else {
            self.edit.selected.min(n - 1)
        };
    }

    /// Route a keystroke to the selection handler or the open prompt.
    fn on_edit_key(&mut self, key: KeyEvent) {
        match self.edit.prompt {
            EditPrompt::None => self.on_edit_select_key(key),
            _ => self.on_edit_prompt_key(key),
        }
    }

    /// Edit mode with no prompt open: move the cursor or begin an edit.
    fn on_edit_select_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.exit_edit_mode(),
            KeyCode::Up => self.edit.selected = self.edit.selected.saturating_sub(1),
            KeyCode::Down => {
                let last = self.backend.circuit().parts().len().saturating_sub(1);
                self.edit.selected = (self.edit.selected + 1).min(last);
            }
            KeyCode::Char('a') => self.open_prompt(EditPrompt::Place),
            KeyCode::Char('w') => self.open_prompt(EditPrompt::Wire),
            KeyCode::Char('d') | KeyCode::Backspace => self.remove_selected(),
            // `+`/`-` nudge the selected component's value and re-solve live.
            KeyCode::Char('+') | KeyCode::Char('=') => self.tune_selected(true),
            KeyCode::Char('-') | KeyCode::Char('_') => self.tune_selected(false),
            _ => {}
        }
    }

    /// Nudge the selected component's value by one fixed step (up when `up`,
    /// else down) and re-solve. Reads the part's current value, scales it, and
    /// routes the change through the shared [`EditAction::Tune`] → `set` path, so
    /// the overlay's op-point and current flow reflect it immediately. A part with
    /// no numeric value (or no selection) is a clean logged no-op.
    pub fn tune_selected(&mut self, up: bool) {
        let Some(reference) = self.selected_part() else {
            self.push_log("error: no part selected to tune".to_string());
            return;
        };
        let Some(value) = self
            .backend
            .circuit()
            .part_by_ref(&reference)
            .and_then(|p| p.value)
        else {
            self.push_log(format!("error: {reference} has no tunable value"));
            return;
        };
        let factor = if up { TUNE_STEP } else { 1.0 / TUNE_STEP };
        let tuned = Value::new(value.magnitude * factor, value.unit);
        self.apply_edit(EditAction::Tune {
            reference,
            value: fmt_value(&tuned),
        });
    }

    fn open_prompt(&mut self, prompt: EditPrompt) {
        self.edit.prompt = prompt;
        self.edit.buffer.clear();
    }

    /// Delete the selected part, or log a clean error when nothing is selected.
    fn remove_selected(&mut self) {
        match self.selected_part() {
            Some(reference) => self.apply_edit(EditAction::Remove { reference }),
            None => self.push_log("error: no part selected to remove".to_string()),
        }
    }

    /// Edit mode with an argument prompt open: type, erase, cancel, or commit.
    fn on_edit_prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.edit.prompt = EditPrompt::None;
                self.edit.buffer.clear();
            }
            KeyCode::Backspace => {
                self.edit.buffer.pop();
            }
            KeyCode::Char(c) => self.edit.buffer.push(c),
            KeyCode::Enter => self.commit_prompt(),
            _ => {}
        }
    }

    /// Commit the open prompt: parse its fields into an [`EditAction`] and apply
    /// it. A wrong field count is a clean logged error, not a panic.
    fn commit_prompt(&mut self) {
        let prompt = self.edit.prompt;
        let buffer = std::mem::take(&mut self.edit.buffer);
        self.edit.prompt = EditPrompt::None;
        let fields: Vec<&str> = buffer.split_whitespace().collect();
        match (prompt, fields.as_slice()) {
            (EditPrompt::Place, [reference, node_a, node_b, value]) => {
                self.apply_edit(EditAction::Place {
                    reference: (*reference).to_string(),
                    node_a: (*node_a).to_string(),
                    node_b: (*node_b).to_string(),
                    value: (*value).to_string(),
                })
            }
            (EditPrompt::Place, _) => {
                self.push_log("error: add needs <ref> <nodeA> <nodeB> <value>".to_string())
            }
            (EditPrompt::Wire, [node_a, node_b]) => self.apply_edit(EditAction::Wire {
                node_a: (*node_a).to_string(),
                node_b: (*node_b).to_string(),
            }),
            (EditPrompt::Wire, _) => self.push_log("error: wire needs <nodeA> <nodeB>".to_string()),
            (EditPrompt::None, _) => {}
        }
    }

    fn push_log(&mut self, line: String) {
        self.log.push(line);
        if self.log.len() > LOG_CAP {
            let drop = self.log.len() - LOG_CAP;
            self.log.drain(0..drop);
        }
    }

    // ── layout + rendering ────────────────────────────────────────────────

    /// The three panels and their cell rectangles at the given size.
    ///
    /// The rectangles tile the whole area exactly (no gaps, no overlap), so the
    /// render can blit each panel into a shared grid.
    pub fn geometry(&self, width: usize, height: usize) -> [(PanelId, Rect); 3] {
        let w = width.max(MIN_W);
        let h = height.max(MIN_H);

        let left_w = ((self.layout.h_split * w as f64).round() as usize).clamp(8, w - 8);
        let right_w = w - left_w;
        let top_h = ((self.layout.v_split * h as f64).round() as usize).clamp(3, h - 3);
        let bottom_h = h - top_h;

        [
            (
                PanelId::Schematic,
                Rect {
                    x: 0,
                    y: 0,
                    w: left_w,
                    h: top_h,
                },
            ),
            (
                PanelId::Scope,
                Rect {
                    x: left_w,
                    y: 0,
                    w: right_w,
                    h: top_h,
                },
            ),
            (
                PanelId::Shell,
                Rect {
                    x: 0,
                    y: top_h,
                    w,
                    h: bottom_h,
                },
            ),
        ]
    }

    /// Render the whole UI to exactly `height` lines of exactly `width` cells.
    ///
    /// Pure and deterministic: panels are drawn without colour (one cell per
    /// char) and blitted into a grid, then the command palette (if open) is drawn
    /// centred on top.
    pub fn render(&self, width: usize, height: usize) -> String {
        let w = width.max(MIN_W);
        let h = height.max(MIN_H);
        let mut grid = vec![vec![' '; w]; h];

        for (id, rect) in self.geometry(w, h) {
            let lines = self.render_panel(id, rect);
            blit(&mut grid, &lines, rect.x, rect.y);
        }

        if self.palette.open {
            self.render_palette(&mut grid, w, h);
        }

        grid.into_iter()
            .map(|row| row.into_iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Render one panel to `rect.h` lines of `rect.w` cells each.
    fn render_panel(&self, id: PanelId, rect: Rect) -> Vec<String> {
        let focused = id == self.focus;
        match id {
            // Edit mode takes precedence over the overlay: the selection marker,
            // prompt, and hint must stay visible while editing even if the overlay
            // is toggled on (`CP-UX-M8.1`).
            PanelId::Schematic if self.edit.active => {
                let rows = Schematic::new("schematic")
                    .width(rect.w)
                    .rows(self.backend.circuit());
                let base = self.edit_schematic_body(&rows);
                let title = focus_title("schematic [edit]", focused);
                Pane::new(&title, &base).render(rect.w, rect.h)
            }
            PanelId::Schematic if self.overlay => {
                let title = focus_title("op-point", focused);
                // The overlay reflects the sampled instant: the transient sample at
                // the playhead when scrubbing, else the DC op-point. Currents are
                // rederived from whichever voltages are shown so the flow matches.
                let volts = self.overlay_voltages();
                let currents = branch_currents(self.backend.circuit(), &volts);
                Overlay::new(&title)
                    .width(rect.w)
                    .color(false)
                    .flow(&currents, self.tick)
                    .pane_lines(self.backend.circuit(), &volts, rect.h)
            }
            PanelId::Schematic => {
                let rows = Schematic::new("schematic")
                    .width(rect.w)
                    .rows(self.backend.circuit());
                let (base, name) = if rows.is_empty() {
                    ("(no parts)".to_string(), "schematic")
                } else {
                    (rows.join("\n"), "schematic")
                };
                let title = focus_title(name, focused);
                Pane::new(&title, &base).render(rect.w, rect.h)
            }
            PanelId::Scope => {
                let title = focus_title("scope", focused);
                let body = self.scope_body(rect);
                Pane::new(&title, &body).render(rect.w, rect.h)
            }
            PanelId::Shell => {
                let title = focus_title("shell", focused);
                let body = self.shell_body(rect.h);
                Pane::new(&title, &body).render(rect.w, rect.h)
            }
        }
    }

    /// The schematic pane body while editing: the part rows with a selection
    /// marker on the cursor, then a hint or the open argument prompt.
    fn edit_schematic_body(&self, rows: &[String]) -> String {
        let mut lines: Vec<String> = if rows.is_empty() {
            vec!["  (no parts)".to_string()]
        } else {
            rows.iter()
                .enumerate()
                .map(|(i, r)| {
                    let marker = if i == self.edit.selected {
                        "▸ "
                    } else {
                        "  "
                    };
                    format!("{marker}{r}")
                })
                .collect()
        };
        let hint = match self.edit.prompt {
            EditPrompt::None => "[a]dd [w]ire [d]el [+/-]tune [esc]".to_string(),
            EditPrompt::Place => format!("add> {}", self.edit.buffer),
            EditPrompt::Wire => format!("wire> {}", self.edit.buffer),
        };
        lines.push(hint);
        lines.join("\n")
    }

    /// The scope pane body, sized to its pane. When a transient is loaded it is
    /// **re-plotted from the cached series at the live pane rect** so the waveform
    /// (and its x-axis label) never clip, regardless of the fixed size the shell
    /// first rendered at; a transport header rides on top (`CP-UX-M7.1`/`M8.1`).
    /// Non-transient scope frames (AC/DC/noise) fall back to the shell's string.
    fn scope_body(&self, rect: Rect) -> String {
        // Pane interior: two border columns/rows are consumed by the frame.
        let inner_w = rect.w.saturating_sub(4);
        let inner_h = rect.h.saturating_sub(2);
        if !self.playback.is_empty() {
            // One interior row goes to the transport header; the plots fill the rest.
            let plot_h = inner_h.saturating_sub(1);
            let body = self.replot_transient(inner_w, plot_h);
            return format!("{}\n{body}", self.transport_line());
        }
        if self.scope.is_empty() {
            "(run a transient to plot)".to_string()
        } else {
            self.scope.clone()
        }
    }

    /// Re-plot every cached transient series into `inner_w × inner_h` interior
    /// cells, splitting the height across the probed signals. Each [`Plot`] needs a
    /// title row, an x-axis rule, and a labels row on top of its braille rows, and
    /// a y-axis gutter on the left — budgeted here so nothing overruns the pane.
    fn replot_transient(&self, inner_w: usize, inner_h: usize) -> String {
        let sigs = self.playback.signals();
        if sigs.is_empty() {
            return String::new();
        }
        // Chrome per plot: title + x rule + x labels = 3 rows; gutter ≈ 7 cols.
        const PLOT_CHROME_ROWS: usize = 3;
        const GUTTER_BUDGET: usize = 7;
        let per = (inner_h / sigs.len()).max(PLOT_CHROME_ROWS + 1);
        let plot_h = per.saturating_sub(PLOT_CHROME_ROWS).max(1);
        let plot_w = inner_w.saturating_sub(GUTTER_BUDGET).max(1);
        let mut out = String::new();
        for (i, s) in sigs.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let y = s.real().unwrap_or(&[]);
            out.push_str(
                &Plot::new(&s.name)
                    .units("V", "s")
                    .size(plot_w, plot_h)
                    .render(&s.x, y),
            );
        }
        out
    }

    /// The playback transport status line: play/pause state, the playhead time,
    /// its frame index, and each probed signal's value at the playhead.
    fn transport_line(&self) -> String {
        let state = if self.playback.playing() {
            "▶"
        } else {
            "⏸"
        };
        let time = fmt_value(&Value::new(self.playback.time(), Unit::Second));
        let samples: Vec<String> = self
            .playback
            .samples()
            .into_iter()
            .map(|(name, v)| format!("{name}={}", fmt_value(&Value::new(v, Unit::Volt))))
            .collect();
        format!(
            "{state} t={time} [{}/{}] {}",
            self.playback.cursor() + 1,
            self.playback.len(),
            samples.join(" ")
        )
    }

    /// The shell pane body: the most recent log lines, then the input prompt.
    ///
    /// The run loop hides the terminal's own cursor, so the prompt draws its own
    /// caret (`▏`) at the input position while the shell holds focus, so the user
    /// can see where typing lands (`CP-UX-M8.1`).
    fn shell_body(&self, pane_h: usize) -> String {
        let rows = pane_h.saturating_sub(2); // interior rows inside the frame
        let caret = if self.focus == PanelId::Shell {
            "\u{258f}"
        } else {
            ""
        };
        let prompt = format!("> {}{caret}", self.input);
        if rows == 0 {
            return prompt;
        }
        let log_rows = rows - 1;
        let start = self.log.len().saturating_sub(log_rows);
        let mut body: Vec<&str> = self.log[start..].iter().map(String::as_str).collect();
        body.push(&prompt);
        body.join("\n")
    }

    /// The centred, framed command-palette box at the given screen size — shared by
    /// the renderer and the mouse hit-test so a click lands where the box is drawn.
    fn palette_rect(&self, w: usize, h: usize) -> Rect {
        let matches = self.palette_matches();
        let box_w = 40.min(w.saturating_sub(4)).max(12);
        // One row for the query, up to eight match rows, plus two border rows.
        let shown = matches.len().min(8);
        let box_h = (shown + 3).min(h.saturating_sub(2)).max(3);
        Rect {
            x: (w - box_w) / 2,
            y: (h - box_h) / 2,
            w: box_w,
            h: box_h,
        }
    }

    /// Draw the command palette as a centred, framed overlay.
    fn render_palette(&self, grid: &mut [Vec<char>], w: usize, h: usize) {
        let matches = self.palette_matches();
        let rect = self.palette_rect(w, h);
        let (box_w, box_h) = (rect.w, rect.h);

        let mut body = String::new();
        body.push_str(&format!("/ {}", self.palette.query));
        for (i, &idx) in matches.iter().take(box_h.saturating_sub(3)).enumerate() {
            let marker = if i == self.palette.selected {
                "▸ "
            } else {
                "  "
            };
            body.push('\n');
            body.push_str(marker);
            body.push_str(COMMANDS[idx].name);
        }
        if matches.is_empty() {
            body.push_str("\n  (no matching command)");
        }

        let lines = Pane::new("palette", &body).render(box_w, box_h);
        blit(grid, &lines, rect.x, rect.y);
    }

    /// Borrow the backend (e.g. to read the circuit after the loop exits).
    pub fn backend(&self) -> &B {
        &self.backend
    }
}

/// A fuzzy subsequence score of `query` against `candidate`, or `None` if the
/// query is not a subsequence.
///
/// Case-insensitive; whitespace in the query is ignored so `"foc sch"` matches
/// `"focus schematic"`. Higher is better: a match at the very start and runs of
/// consecutive matched characters are rewarded, so the most relevant command
/// sorts first.
fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    let q: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if q.is_empty() {
        return Some(0);
    }
    let cand: Vec<char> = candidate.chars().flat_map(char::to_lowercase).collect();

    let mut qi = 0;
    let mut score = 0i32;
    let mut last_match: Option<usize> = None;
    for (ci, &ch) in cand.iter().enumerate() {
        if qi < q.len() && ch == q[qi] {
            score += 1;
            if ci == 0 {
                score += 10; // matches the very first character
            }
            if let Some(prev) = last_match {
                if ci == prev + 1 {
                    score += 5; // consecutive run
                }
            }
            last_match = Some(ci);
            qi += 1;
        }
    }
    (qi == q.len()).then_some(score)
}

/// A panel title with a focus marker when it holds focus.
fn focus_title(base: &str, focused: bool) -> String {
    if focused {
        format!("▸ {base}")
    } else {
        base.to_string()
    }
}

/// Whether a command's output is a waveform scope frame (carries the `┤` axis).
fn is_scope(out: &str) -> bool {
    out.contains('\u{2524}')
}

/// Copy `lines` into `grid` with its top-left at `(x, y)`, clipping at the edges.
fn blit(grid: &mut [Vec<char>], lines: &[String], x: usize, y: usize) {
    for (r, line) in lines.iter().enumerate() {
        let gy = y + r;
        if gy >= grid.len() {
            break;
        }
        for (c, ch) in line.chars().enumerate() {
            let gx = x + c;
            if gx >= grid[gy].len() {
                break;
            }
            grid[gy][gx] = ch;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee_core::graph::{NetId, PartKind};
    use ee_core::signal::Signal;
    use ee_core::units::Unit;
    use ee_core::value::Value;

    /// A mock backend: records the lines it ran, returns a canned scope frame for
    /// `run`, a plain string otherwise, and an error for the line `"boom"`.
    struct MockBackend {
        circuit: Circuit,
        ran: Vec<String>,
    }

    impl MockBackend {
        fn new() -> Self {
            let mut circuit = Circuit::new("mock");
            let n = circuit.new_net();
            circuit.add_part(
                "R1",
                PartKind::Resistor,
                Some(Value::new(1000.0, Unit::Ohm)),
                vec![n, NetId::GROUND],
            );
            Self {
                circuit,
                ran: Vec::new(),
            }
        }
    }

    impl Backend for MockBackend {
        fn execute(&mut self, line: &str) -> Result<String, String> {
            self.ran.push(line.to_string());
            if line == "boom" {
                return Err("it went boom".to_string());
            }
            if line.starts_with("run") {
                // A minimal frame carrying the scope axis glyph.
                return Ok("  tran\n  \u{2524}\u{2801}\n".to_string());
            }
            Ok(format!("ok: {line}"))
        }
        fn circuit(&self) -> &Circuit {
            &self.circuit
        }
    }

    fn app() -> App<MockBackend> {
        App::new(MockBackend::new())
    }

    fn key<B: Backend>(app: &mut App<B>, code: KeyCode) {
        app.handle_event(AppEvent::Key(KeyEvent::new(code)));
    }

    fn typ<B: Backend>(app: &mut App<B>, s: &str) {
        for c in s.chars() {
            app.handle_event(AppEvent::Key(KeyEvent::char(c)));
        }
    }

    #[test]
    fn tab_cycles_focus_forward_and_backtab_cycles_back() {
        let mut a = app();
        a.focus = PanelId::Schematic;
        key(&mut a, KeyCode::Tab);
        assert_eq!(a.focus(), PanelId::Scope);
        key(&mut a, KeyCode::Tab);
        assert_eq!(a.focus(), PanelId::Shell);
        key(&mut a, KeyCode::Tab);
        assert_eq!(a.focus(), PanelId::Schematic, "wraps around");
        key(&mut a, KeyCode::BackTab);
        assert_eq!(a.focus(), PanelId::Shell, "back-tab wraps the other way");
    }

    #[test]
    fn growing_the_focused_panel_changes_pane_geometry() {
        let mut a = app();
        let (w, h) = (80, 24);
        let before = a.geometry(w, h);
        // Focus the shell (bottom pane) and grow it: its height must increase and
        // the top row must shrink by the same amount.
        a.focus = PanelId::Shell;
        a.grow_focused();
        let after = a.geometry(w, h);

        let shell_before = before.iter().find(|(p, _)| *p == PanelId::Shell).unwrap().1;
        let shell_after = after.iter().find(|(p, _)| *p == PanelId::Shell).unwrap().1;
        assert!(
            shell_after.h > shell_before.h,
            "shell grew: {} -> {}",
            shell_before.h,
            shell_after.h
        );
        let sch_before = before
            .iter()
            .find(|(p, _)| *p == PanelId::Schematic)
            .unwrap()
            .1;
        let sch_after = after
            .iter()
            .find(|(p, _)| *p == PanelId::Schematic)
            .unwrap()
            .1;
        assert!(sch_after.h < sch_before.h, "top row shrank to compensate");
    }

    #[test]
    fn ctrl_arrow_resizes_the_split_boundary() {
        let mut a = app();
        let (w, h) = (80, 24);
        let left_before = a.geometry(w, h)[0].1.w;
        // Ctrl-Right widens the left column.
        a.handle_event(AppEvent::Key(KeyEvent::new(KeyCode::Right).with_ctrl()));
        let left_after = a.geometry(w, h)[0].1.w;
        assert!(
            left_after > left_before,
            "left column widened: {left_before} -> {left_after}"
        );
    }

    #[test]
    fn palette_opens_filters_and_enter_runs_the_action() {
        let mut a = app();
        a.focus = PanelId::Shell;
        // Ctrl-P opens the palette.
        a.handle_event(AppEvent::Key(KeyEvent::char('p').with_ctrl()));
        assert!(a.palette_open());

        // Typing "foc sch" fuzzy-filters down to exactly "focus schematic" first.
        typ(&mut a, "foc sch");
        let filtered = a.palette_filtered();
        assert_eq!(
            filtered.first().copied(),
            Some("focus schematic"),
            "fuzzy filter ranks the intended command first: {filtered:?}"
        );
        assert!(
            !filtered.contains(&"focus scope"),
            "'focsch' is not a subsequence of 'focus scope': {filtered:?}"
        );

        // Enter runs the top match and closes the palette.
        key(&mut a, KeyCode::Enter);
        assert!(!a.palette_open(), "palette closed after a command");
        assert_eq!(a.focus(), PanelId::Schematic, "it focused the schematic");
    }

    #[test]
    fn palette_arrow_selection_picks_a_lower_match() {
        let mut a = app();
        a.open_palette();
        typ(&mut a, "focus");
        // All three "focus …" commands match; Down selects the second one.
        let filtered = a.palette_filtered();
        assert!(
            filtered.len() >= 2,
            "several focus commands match: {filtered:?}"
        );
        let second = filtered[1];
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Enter);
        let expected = match second {
            "focus scope" => PanelId::Scope,
            "focus shell" => PanelId::Shell,
            "focus schematic" => PanelId::Schematic,
            other => panic!("unexpected second match {other:?}"),
        };
        assert_eq!(a.focus(), expected);
    }

    #[test]
    fn palette_query_with_no_match_is_empty() {
        let mut a = app();
        a.open_palette();
        typ(&mut a, "zzzz");
        assert!(a.palette_filtered().is_empty(), "no command matches 'zzzz'");
        // Enter on an empty match list is a safe no-op that closes the palette.
        key(&mut a, KeyCode::Enter);
        assert!(!a.palette_open());
    }

    #[test]
    fn empty_query_lists_every_command() {
        let mut a = app();
        a.open_palette();
        assert_eq!(a.palette_filtered().len(), COMMANDS.len());
    }

    #[test]
    fn mouse_click_focuses_the_panel_under_the_cursor() {
        let mut a = app();
        a.set_size(80, 24);
        a.focus = PanelId::Shell;
        // The scope pane is top-right; click in its interior.
        let scope = a.geometry(80, 24)[1].1;
        let col = scope.x + scope.w / 2;
        let row = scope.y + 1;
        a.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseKind::Down,
            col,
            row,
        }));
        assert_eq!(a.focus(), PanelId::Scope);
    }

    #[test]
    fn typing_in_the_shell_and_pressing_enter_runs_the_backend() {
        let mut a = app();
        a.focus = PanelId::Shell;
        typ(&mut a, "add R1 a b 1k");
        key(&mut a, KeyCode::Enter);
        assert_eq!(
            a.backend().ran.last().map(String::as_str),
            Some("add R1 a b 1k"),
            "the line reached the backend"
        );
        // The command and its output are in the log; the input line is cleared.
        let frame = a.render(80, 24);
        assert!(
            frame.contains("> add R1 a b 1k"),
            "echoed command:\n{frame}"
        );
        assert!(
            frame.contains("ok: add R1 a b 1k"),
            "backend output:\n{frame}"
        );
    }

    #[test]
    fn a_run_command_updates_the_scope_pane() {
        let mut a = app();
        a.focus = PanelId::Shell;
        typ(&mut a, "run tran 1m");
        key(&mut a, KeyCode::Enter);
        let frame = a.render(80, 24);
        // The canned frame's braille dot shows up in the scope pane.
        assert!(
            frame
                .chars()
                .any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)),
            "scope pane shows the waveform:\n{frame}"
        );
    }

    #[test]
    fn backend_error_is_reported_in_the_log() {
        let mut a = app();
        a.focus = PanelId::Shell;
        typ(&mut a, "boom");
        key(&mut a, KeyCode::Enter);
        let frame = a.render(80, 24);
        assert!(frame.contains("error: it went boom"), "{frame}");
    }

    #[test]
    fn ctrl_c_quits_from_any_state() {
        let mut a = app();
        assert!(!a.should_quit());
        a.handle_event(AppEvent::Key(KeyEvent::char('c').with_ctrl()));
        assert!(a.should_quit());
    }

    #[test]
    fn render_is_exact_dimensions_and_shows_every_panel_title() {
        let mut a = app();
        a.focus = PanelId::Schematic;
        let (w, h) = (80, 24);
        let frame = a.render(w, h);
        assert_eq!(frame.lines().count(), h, "line count == height");
        for line in frame.lines() {
            assert_eq!(line.chars().count(), w, "exactly width cells: {line:?}");
        }
        assert!(frame.contains("schematic"), "schematic title:\n{frame}");
        assert!(frame.contains("scope"), "scope title:\n{frame}");
        assert!(frame.contains("shell"), "shell title:\n{frame}");
        assert!(
            frame.contains("▸ schematic"),
            "focus marker on schematic:\n{frame}"
        );
        // The mock circuit's part lists in the schematic pane.
        assert!(frame.contains("R1"), "schematic lists the part:\n{frame}");
    }

    #[test]
    fn toggle_overlay_switches_the_left_pane() {
        let mut a = app();
        assert!(a.render(80, 24).contains("schematic"));
        a.overlay = true;
        let frame = a.render(80, 24);
        assert!(frame.contains("op-point"), "overlay view title:\n{frame}");
    }

    #[test]
    fn render_is_deterministic() {
        let a = app();
        assert_eq!(a.render(80, 24), a.render(80, 24));
    }

    #[test]
    fn fuzzy_score_rewards_prefix_and_consecutive_matches() {
        // An exact prefix outscores a scattered subsequence.
        let prefix = fuzzy_score("quit", "quit").unwrap();
        let scattered = fuzzy_score("qt", "quit").unwrap();
        assert!(prefix > scattered, "{prefix} vs {scattered}");
        assert!(fuzzy_score("zzz", "quit").is_none(), "non-subsequence");
    }

    // ── interactive editing (CP-UX-M5.1) ──────────────────────────────────

    /// A backend that actually builds an `ee_core::Circuit` from the `add`/`wire`/
    /// `remove` grammar — a faithful stand-in for `ee-shell`'s `Shell` so the edit
    /// state machine can be asserted on a *real* circuit without depending on the
    /// shell crate. Minimal on purpose: only what the editor emits is parsed.
    struct EditBackend {
        circuit: Circuit,
        nets: std::collections::HashMap<String, NetId>,
    }

    impl EditBackend {
        fn new() -> Self {
            Self {
                circuit: Circuit::new("edit"),
                nets: std::collections::HashMap::new(),
            }
        }

        fn is_ground(name: &str) -> bool {
            matches!(name, "gnd" | "ground" | "0")
        }

        fn net_for(&mut self, name: &str) -> NetId {
            if Self::is_ground(name) {
                return NetId::GROUND;
            }
            if let Some(&id) = self.nets.get(name) {
                return id;
            }
            let id = self.circuit.new_net();
            self.nets.insert(name.to_string(), id);
            id
        }

        fn net_known(&self, name: &str) -> Result<NetId, String> {
            if Self::is_ground(name) {
                return Ok(NetId::GROUND);
            }
            self.nets
                .get(name)
                .copied()
                .ok_or_else(|| format!("unknown node {name:?}"))
        }
    }

    impl Backend for EditBackend {
        fn execute(&mut self, line: &str) -> Result<String, String> {
            let w: Vec<&str> = line.split_whitespace().collect();
            match w.as_slice() {
                ["add", r, a, b, v] => {
                    if self.circuit.part_by_ref(r).is_some() {
                        return Err(format!("duplicate part {r}"));
                    }
                    let (kind, unit) = match r.chars().next().map(|c| c.to_ascii_uppercase()) {
                        Some('R') => (PartKind::Resistor, Unit::Ohm),
                        Some('C') => (PartKind::Capacitor, Unit::Farad),
                        Some('V') => (PartKind::VoltageSource, Unit::Volt),
                        _ => return Err(format!("unknown part type for {r}")),
                    };
                    let value = Value::parse(v, unit).map_err(|_| format!("bad value {v:?}"))?;
                    let (na, nb) = (self.net_for(a), self.net_for(b));
                    self.circuit.add_part(*r, kind, Some(value), vec![na, nb]);
                    Ok(format!("added {r}"))
                }
                ["wire", a, b] => {
                    let (na, nb) = (self.net_known(a)?, self.net_known(b)?);
                    self.circuit.wire(na, nb);
                    Ok(format!("wired {a} {b}"))
                }
                ["set", target, v] => {
                    // `set <ref>.value <v>` — retarget a component value in place.
                    let reference = target.split('.').next().unwrap_or(*target).to_string();
                    let unit = match self.circuit.part_by_ref(&reference) {
                        Some(p) => p.value.map(|val| val.unit).unwrap_or(Unit::Ohm),
                        None => return Err(format!("no part named {reference}")),
                    };
                    let value = Value::parse(v, unit).map_err(|_| format!("bad value {v:?}"))?;
                    self.circuit.set_value(&reference, value);
                    Ok(format!("set {reference}"))
                }
                ["remove", r] => {
                    if self.circuit.part_by_ref(r).is_none() {
                        return Err(format!("no part named {r}"));
                    }
                    // ee-core has no part removal; rebuild without the target,
                    // preserving net ids and union-find topology via `resolve`.
                    let old = &self.circuit;
                    let mut max_id = 0u32;
                    for p in old.parts() {
                        for t in &p.terminals {
                            max_id = max_id.max(t.0);
                        }
                    }
                    let mut c = Circuit::new("edit");
                    for _ in 1..=max_id {
                        c.new_net();
                    }
                    for id in 1..=max_id {
                        let root = old.resolve(NetId(id));
                        if root.0 != id {
                            c.wire(NetId(id), root);
                        }
                    }
                    for p in old.parts() {
                        if &p.reference == r {
                            continue;
                        }
                        c.add_part(
                            p.reference.clone(),
                            p.kind.clone(),
                            p.value,
                            p.terminals.clone(),
                        );
                    }
                    self.circuit = c;
                    Ok(format!("removed {r}"))
                }
                _ => Err(format!("cannot parse {line:?}")),
            }
        }

        fn circuit(&self) -> &Circuit {
            &self.circuit
        }
    }

    fn edit_app() -> App<EditBackend> {
        App::new(EditBackend::new())
    }

    /// Placing parts adds them to the app's circuit; wiring two nodes merges their
    /// nets (checked via `resolve`); removing a part drops it.
    #[test]
    fn edit_actions_place_wire_and_remove_on_the_real_circuit() {
        let mut a = edit_app();
        a.apply_edit(EditAction::Place {
            reference: "V1".into(),
            node_a: "in".into(),
            node_b: "gnd".into(),
            value: "5".into(),
        });
        a.apply_edit(EditAction::Place {
            reference: "R1".into(),
            node_a: "in".into(),
            node_b: "out".into(),
            value: "1k".into(),
        });
        assert_eq!(a.backend().circuit().parts().len(), 2, "two parts placed");

        // Before wiring, R1's two terminals are distinct nets.
        let r1 = a.backend().circuit().part_by_ref("R1").unwrap();
        let (ta, tb) = (r1.terminals[0], r1.terminals[1]);
        assert_ne!(
            a.backend().circuit().resolve(ta),
            a.backend().circuit().resolve(tb)
        );

        a.apply_edit(EditAction::Wire {
            node_a: "in".into(),
            node_b: "out".into(),
        });
        assert_eq!(
            a.backend().circuit().resolve(ta),
            a.backend().circuit().resolve(tb),
            "wiring merged the two nets"
        );

        a.apply_edit(EditAction::Remove {
            reference: "V1".into(),
        });
        assert!(
            a.backend().circuit().part_by_ref("V1").is_none(),
            "V1 removed"
        );
        assert!(a.backend().circuit().part_by_ref("R1").is_some(), "R1 kept");
    }

    /// The keystroke path: enter edit mode, add through the prompt, select, delete.
    #[test]
    fn edit_keystrokes_drive_selection_and_deletion() {
        let mut a = edit_app();
        // Focus the schematic, then `e` enters edit mode.
        a.focus = PanelId::Schematic;
        key(&mut a, KeyCode::Char('e'));
        assert!(a.edit_mode(), "entered edit mode");

        key(&mut a, KeyCode::Char('a'));
        typ(&mut a, "R1 a b 1k");
        key(&mut a, KeyCode::Enter);
        key(&mut a, KeyCode::Char('a'));
        typ(&mut a, "R2 b c 2k");
        key(&mut a, KeyCode::Enter);
        assert_eq!(a.backend().circuit().parts().len(), 2);

        // Cursor up to the first part and delete it.
        key(&mut a, KeyCode::Up);
        key(&mut a, KeyCode::Up);
        assert_eq!(a.selected_part().as_deref(), Some("R1"));
        key(&mut a, KeyCode::Char('d'));
        assert!(
            a.backend().circuit().part_by_ref("R1").is_none(),
            "R1 deleted"
        );
        assert_eq!(a.backend().circuit().parts().len(), 1);

        key(&mut a, KeyCode::Esc);
        assert!(!a.edit_mode(), "Esc exits edit mode");
    }

    /// The schematic pane reflects edits and shows the edit-mode chrome.
    #[test]
    fn schematic_render_reflects_edits_and_edit_mode() {
        let mut a = edit_app();
        a.apply_edit(EditAction::Place {
            reference: "R1".into(),
            node_a: "a".into(),
            node_b: "b".into(),
            value: "4k7".into(),
        });
        // Not yet in edit mode: the part lists in the normal schematic pane.
        let frame = a.render(80, 24);
        assert!(frame.contains("R1"), "placed part renders:\n{frame}");

        a.focus = PanelId::Schematic;
        key(&mut a, KeyCode::Char('e'));
        let frame = a.render(80, 24);
        assert!(frame.contains("[edit]"), "edit-mode title:\n{frame}");
        assert!(frame.contains("▸ R1"), "selection marker on R1:\n{frame}");
        assert!(frame.contains("[a]dd"), "edit hint line:\n{frame}");

        // The overlay view also reflects the edited circuit.
        a.overlay = true;
        let frame = a.render(80, 24);
        assert!(frame.contains("R1"), "overlay reflects the part:\n{frame}");
    }

    /// A malformed add (too few fields) and a backend-rejected edit (unknown node
    /// on a wire) are clean logged errors — no panic, no circuit change.
    #[test]
    fn invalid_edits_surface_errors_without_panic() {
        let mut a = edit_app();
        a.focus = PanelId::Schematic;
        key(&mut a, KeyCode::Char('e'));

        // Malformed add: too few fields.
        key(&mut a, KeyCode::Char('a'));
        typ(&mut a, "oops");
        key(&mut a, KeyCode::Enter);
        assert_eq!(a.backend().circuit().parts().len(), 0, "nothing placed");

        // A wire naming an unknown node is rejected by the backend, logged clean.
        a.apply_edit(EditAction::Wire {
            node_a: "ghost".into(),
            node_b: "gnd".into(),
        });
        let frame = a.render(80, 24);
        assert!(
            frame.contains("error"),
            "errors surface in the log:\n{frame}"
        );

        // Deleting with nothing selected is a friendly no-op, not a panic.
        a.remove_selected();
        assert_eq!(a.backend().circuit().parts().len(), 0);
    }

    // ── live tuning (CP-UX-M6.1) ───────────────────────────────────────────

    /// A `+`/`-` nudge on the selected part retargets its value through the shared
    /// `set` mutation path, and the edited circuit carries the new value.
    #[test]
    fn tune_keys_change_the_selected_components_value() {
        let mut a = edit_app();
        a.focus = PanelId::Schematic;
        key(&mut a, KeyCode::Char('e'));
        key(&mut a, KeyCode::Char('a'));
        typ(&mut a, "R1 a b 1k");
        key(&mut a, KeyCode::Enter);
        assert_eq!(a.selected_part().as_deref(), Some("R1"), "R1 selected");

        let mag = |a: &App<EditBackend>| {
            a.backend()
                .circuit()
                .part_by_ref("R1")
                .unwrap()
                .value
                .unwrap()
                .magnitude
        };
        let before = mag(&a);
        key(&mut a, KeyCode::Char('+'));
        let up = mag(&a);
        assert!(up > before, "+ raised R1's value: {before} -> {up}");
        assert!((up / before - TUNE_STEP).abs() < 1e-6, "one 10% step: {up}");

        // The nudge lowered to a `set` command on the shared path (logged, echoed).
        let frame = a.render(80, 24);
        assert!(
            frame.contains("set R1.value"),
            "set command echoed:\n{frame}"
        );

        // `-` steps back down below the raised value.
        key(&mut a, KeyCode::Char('-'));
        let down = mag(&a);
        assert!(down < up, "- lowered R1's value: {up} -> {down}");
    }

    /// Tuning with nothing selected, or a part with no numeric value, is a clean
    /// logged no-op rather than a panic.
    #[test]
    fn tune_without_a_value_is_a_clean_no_op() {
        let mut a = edit_app();
        // Empty circuit: nothing selected to tune.
        a.tune_selected(true);
        let frame = a.render(80, 24);
        assert!(
            frame.contains("error"),
            "friendly error, no panic:\n{frame}"
        );
        assert_eq!(a.backend().circuit().parts().len(), 0, "nothing changed");
    }

    /// The tune `EditAction` lowers to the shell's `set <ref>.value <v>` verb.
    #[test]
    fn tune_action_lowers_to_the_set_command() {
        let line = EditAction::Tune {
            reference: "R1".into(),
            value: "2.2k".into(),
        }
        .to_command();
        assert_eq!(line, "set R1.value 2.2k");
    }

    /// Opening the palette and running "edit mode" toggles edit mode on.
    #[test]
    fn palette_edit_mode_command_toggles_editing() {
        let mut a = edit_app();
        a.open_palette();
        typ(&mut a, "edit mode");
        assert_eq!(a.palette_filtered().first().copied(), Some("edit mode"));
        key(&mut a, KeyCode::Enter);
        assert!(a.edit_mode(), "palette command entered edit mode");
    }

    // ── transient playback (CP-UX-M7.1) ────────────────────────────────────

    /// A backend with a divider circuit and a canned transient result, so the
    /// playback state machine runs against real `Signal`s + a real `Circuit`
    /// without the shell or solver. A `run` line returns a scope frame (so the app
    /// loads the playback through the normal seam), and `transient()` hands back
    /// the cached ramp. `v(n1)` rises 0→3 V and `v(n2)` 0→1.5 V over four samples
    /// at 0, 1, 2, 3 ms.
    struct PlayBackend {
        circuit: Circuit,
        transient: Vec<Signal>,
    }

    impl PlayBackend {
        fn new() -> Self {
            let mut circuit = Circuit::new("play");
            let n1 = circuit.new_net();
            let n2 = circuit.new_net();
            circuit.add_part(
                "V1",
                PartKind::VoltageSource,
                Some(Value::new(3.0, Unit::Volt)),
                vec![n1, NetId::GROUND],
            );
            circuit.add_part(
                "R1",
                PartKind::Resistor,
                Some(Value::new(1000.0, Unit::Ohm)),
                vec![n1, n2],
            );
            circuit.add_part(
                "R2",
                PartKind::Resistor,
                Some(Value::new(1000.0, Unit::Ohm)),
                vec![n2, NetId::GROUND],
            );
            let t = vec![0.0, 1e-3, 2e-3, 3e-3];
            let transient = vec![
                Signal::time(
                    format!("v(n{})", circuit.resolve(n1).0),
                    Unit::Volt,
                    t.clone(),
                    vec![0.0, 1.0, 2.0, 3.0],
                ),
                Signal::time(
                    format!("v(n{})", circuit.resolve(n2).0),
                    Unit::Volt,
                    t,
                    vec![0.0, 0.5, 1.0, 1.5],
                ),
            ];
            Self { circuit, transient }
        }
    }

    impl Backend for PlayBackend {
        fn execute(&mut self, line: &str) -> Result<String, String> {
            if line.starts_with("run") {
                // A minimal frame carrying the scope axis glyph (treated as a run).
                return Ok("  tran\n  \u{2524}\u{2801}\n".to_string());
            }
            Ok(format!("ok: {line}"))
        }
        fn circuit(&self) -> &Circuit {
            &self.circuit
        }
        fn transient(&self) -> Vec<Signal> {
            self.transient.clone()
        }
    }

    /// Running a transient loads the cached result into playback, and advancing the
    /// playhead one sample at a time reads the exact (nearest-sample) node voltage
    /// at each sampled time.
    #[test]
    fn playhead_samples_the_transient_at_each_step() {
        let mut a = App::new(PlayBackend::new());
        a.focus = PanelId::Shell;
        typ(&mut a, "run tran 3m");
        key(&mut a, KeyCode::Enter);

        assert_eq!(a.playback().len(), 4, "four cached frames loaded");
        assert_eq!(a.playback().cursor(), 0, "playhead starts at sample 0");

        // Advancing N steps samples the expected voltage at each step — nearest
        // sample, i.e. the cursor indexes straight into the cached time series.
        let expect = [(0.0, 0.0), (1.0, 0.5), (2.0, 1.0), (3.0, 1.5)];
        for (i, (e1, e2)) in expect.iter().enumerate() {
            if i > 0 {
                a.step_playhead(1);
            }
            assert_eq!(a.playback().cursor(), i, "cursor at step {i}");
            assert_eq!(a.playback().sample("v(n1)"), Some(*e1), "v(n1) at step {i}");
            assert_eq!(a.playback().sample("v(n2)"), Some(*e2), "v(n2) at step {i}");
            // The playhead's time tracks the cursor (1 ms spacing).
            assert_eq!(a.playback().time(), i as f64 * 1e-3, "time at step {i}");
        }
    }

    /// Play/pause/seek/step and the frame-timeout tick move the cursor as expected
    /// and clamp at both ends; the scope-pane transport keys drive the same path.
    #[test]
    fn play_pause_seek_move_the_cursor_and_clamp() {
        let mut a = App::new(PlayBackend::new());
        a.focus = PanelId::Shell;
        typ(&mut a, "run");
        key(&mut a, KeyCode::Enter);
        assert_eq!(a.playback().len(), 4);

        // Play / pause / toggle transitions.
        assert!(
            !a.playback().playing(),
            "a freshly loaded playback is paused"
        );
        a.play();
        assert!(a.playback().playing());
        a.pause();
        assert!(!a.playback().playing());
        a.toggle_play();
        assert!(a.playback().playing(), "toggle flips play back on");

        // The run loop's tick marches the playhead one sample while playing — the
        // same tick that advances the current-flow animation.
        a.tick();
        assert_eq!(a.playback().cursor(), 1, "a tick advanced the playhead");

        // Seek clamps past the end; step clamps at both ends.
        a.seek_playhead(99);
        assert_eq!(a.playback().cursor(), 3, "seek clamps to the last frame");
        a.step_playhead(5);
        assert_eq!(a.playback().cursor(), 3, "step past the end clamps");
        a.step_playhead(-10);
        assert_eq!(a.playback().cursor(), 0, "step before the start clamps");

        // Playing to the end clamps on the last frame and pauses.
        a.play();
        for _ in 0..10 {
            a.tick();
        }
        assert_eq!(a.playback().cursor(), 3, "ticks clamp at the end");
        assert!(!a.playback().playing(), "reaching the end pauses");

        // The scope-pane transport keys drive the same transitions.
        a.focus = PanelId::Scope;
        a.handle_event(AppEvent::Key(KeyEvent::char(','))); // seek start
        assert_eq!(a.playback().cursor(), 0);
        a.handle_event(AppEvent::Key(KeyEvent::new(KeyCode::Right))); // step forward
        assert_eq!(a.playback().cursor(), 1);
        a.handle_event(AppEvent::Key(KeyEvent::new(KeyCode::Left))); // step back
        assert_eq!(a.playback().cursor(), 0);
        a.handle_event(AppEvent::Key(KeyEvent::char('.'))); // seek end
        assert_eq!(a.playback().cursor(), 3);
        a.handle_event(AppEvent::Key(KeyEvent::char(' '))); // toggle play
        assert!(
            a.playback().playing(),
            "space toggled play on the scope pane"
        );
    }

    /// The live overlay at the playhead is annotated with the *instantaneous*
    /// transient voltages (reusing the op-point `v(n<net>)` path), and scrubbing to
    /// another sample changes them — it is not a frozen DC op-point.
    #[test]
    fn overlay_at_the_playhead_shows_the_instantaneous_voltages() {
        let mut a = App::new(PlayBackend::new());
        a.focus = PanelId::Shell;
        typ(&mut a, "run");
        key(&mut a, KeyCode::Enter);
        a.overlay = true;

        // At sample 2 the ramp reads v(n1) = 2 V and v(n2) = 1 V.
        a.seek_playhead(2);
        let frame = a.render(80, 24);
        assert!(
            frame.contains("n1=2V"),
            "playhead overlay shows v(n1)=2 V:\n{frame}"
        );
        assert!(
            frame.contains("n2=1V"),
            "playhead overlay shows v(n2)=1 V:\n{frame}"
        );

        // Scrubbing to sample 3 moves the annotation to v(n1) = 3 V.
        a.seek_playhead(3);
        let frame = a.render(80, 24);
        assert!(frame.contains("n1=3V"), "scrubbed to v(n1)=3 V:\n{frame}");
        assert!(
            !frame.contains("n1=2V"),
            "the previous sample's annotation is gone:\n{frame}"
        );

        // The scope pane reflects the sampled instant via its transport header.
        assert!(
            frame.contains("t="),
            "scope transport shows the playhead time:\n{frame}"
        );
    }

    // ── CP-UX-M8.1: TUI hardening ──────────────────────────────────────────

    /// A tune-aware backend: `op_point` reports `v(n1)` equal to a stored level,
    /// and `set …` updates that level — so the overlay can be checked for tracking
    /// the *live op-point* versus the loaded transient's `t = 0` sample.
    struct TuneBackend {
        circuit: Circuit,
        level: f64,
    }

    impl TuneBackend {
        fn new() -> Self {
            let mut circuit = Circuit::new("tune");
            let n1 = circuit.new_net();
            circuit.add_part(
                "R1",
                PartKind::Resistor,
                Some(Value::new(1000.0, Unit::Ohm)),
                vec![n1, NetId::GROUND],
            );
            Self {
                circuit,
                level: 5.0,
            }
        }
    }

    impl Backend for TuneBackend {
        fn execute(&mut self, line: &str) -> Result<String, String> {
            if line.starts_with("run") {
                return Ok("  tran\n  \u{2524}\u{2801}\n".to_string());
            }
            if let Some(v) = line.strip_prefix("set R1.value ") {
                self.level = v.parse().unwrap_or(self.level);
            }
            Ok(format!("ok: {line}"))
        }
        fn circuit(&self) -> &Circuit {
            &self.circuit
        }
        fn op_point(&self) -> NodeVoltages {
            let mut v = NodeVoltages::new();
            v.set(NetId(1), self.level);
            v
        }
        fn transient(&self) -> Vec<Signal> {
            // A ramp whose t=0 sample is 0 V — distinct from the op-point level.
            vec![Signal::time(
                "v(n1)",
                Unit::Volt,
                vec![0.0, 1e-3, 2e-3],
                vec![0.0, 1.0, 2.0],
            )]
        }
    }

    /// Gate: the overlay reflects a live tune while a transient is loaded. After a
    /// `run`, the playback is loaded but the overlay must still show the DC
    /// op-point (`v(n1)=5V`), not the playback's `t=0` sample (0 V); a tune then
    /// moves the op-point and the overlay follows — the regression the scrubbing
    /// flag fixes (`CP-UX-M8.1`).
    #[test]
    fn overlay_reflects_a_tune_with_a_transient_loaded() {
        let mut a = App::new(TuneBackend::new());
        a.focus = PanelId::Shell;
        typ(&mut a, "run tran 2m");
        key(&mut a, KeyCode::Enter);
        a.overlay = true;
        assert!(
            !a.playback().is_empty(),
            "the transient loaded into playback"
        );
        assert!(!a.is_scrubbing(), "a fresh run is not scrubbing");

        // Not scrubbing → the overlay shows the live op-point, not the t=0 sample.
        let frame = a.render(80, 24);
        assert!(
            frame.contains("n1=5V"),
            "overlay shows the DC op-point, not the t=0 playback sample:\n{frame}"
        );

        // A live tune moves the op-point and the overlay follows immediately.
        a.apply_edit(EditAction::Tune {
            reference: "R1".to_string(),
            value: "8".to_string(),
        });
        let frame = a.render(80, 24);
        assert!(
            frame.contains("n1=8V"),
            "overlay reflects the tuned op-point:\n{frame}"
        );
        assert!(
            !a.is_scrubbing(),
            "a command/edit returns the overlay to the op-point"
        );
    }

    /// Scrubbing the scope transport hands the overlay to the playhead; the next
    /// command returns it to the live op-point.
    #[test]
    fn scrubbing_flag_gates_the_overlay_source() {
        let mut a = App::new(TuneBackend::new());
        a.focus = PanelId::Shell;
        typ(&mut a, "run");
        key(&mut a, KeyCode::Enter);
        assert!(!a.is_scrubbing());
        // Focus the scope and step the transport → scrubbing.
        a.focus = PanelId::Scope;
        key(&mut a, KeyCode::Right);
        assert!(a.is_scrubbing(), "a transport key starts scrubbing");
        // Running another command clears scrubbing again.
        a.focus = PanelId::Shell;
        typ(&mut a, "run op");
        key(&mut a, KeyCode::Enter);
        assert!(
            !a.is_scrubbing(),
            "a command returns the overlay to the op-point"
        );
    }

    /// An idle screen animates nothing and does not churn: ticking it leaves the
    /// rendered frame byte-identical, so the diffing run loop never repaints
    /// (`CP-UX-M8.1`).
    #[test]
    fn idle_screen_does_not_animate_or_churn() {
        let mut a = app();
        assert!(
            !a.is_animating(),
            "an idle (overlay-off) screen animates nothing"
        );
        let before = a.render(80, 24);
        a.tick();
        a.tick();
        let after = a.render(80, 24);
        assert_eq!(
            before, after,
            "ticking an idle screen must not change the frame"
        );
    }

    /// A playing transient playback animates (so the loop drives ticks); a paused
    /// one does not.
    #[test]
    fn playing_playback_is_the_only_reason_an_empty_circuit_animates() {
        let mut a = App::new(PlayBackend::new());
        a.focus = PanelId::Shell;
        typ(&mut a, "run");
        key(&mut a, KeyCode::Enter);
        assert!(
            !a.is_animating(),
            "a freshly loaded, paused playback is idle"
        );
        a.play();
        assert!(a.is_animating(), "a playing playback animates");
        a.pause();
        assert!(!a.is_animating(), "pausing stops the animation");
    }

    /// Gate: the scope renders within an 80×24 pane without clipping its x-axis.
    /// The transient is re-plotted at the live pane size, so the axis rule and the
    /// time-range labels are present even though the shell first rendered at a
    /// fixed size (`CP-UX-M8.1`).
    #[test]
    fn scope_renders_within_an_80x24_pane_without_clipping_the_axis() {
        let mut a = App::new(PlayBackend::new());
        a.focus = PanelId::Shell;
        typ(&mut a, "run tran 3m");
        key(&mut a, KeyCode::Enter);
        // Find the scope pane rect and pull just its rows out of the frame.
        let (w, h) = (80, 24);
        let scope_rect = a
            .geometry(w, h)
            .into_iter()
            .find(|(id, _)| *id == PanelId::Scope)
            .unwrap()
            .1;
        let frame = a.render(w, h);
        let rows: Vec<&str> = frame.lines().collect();
        let scope_text: String = rows[scope_rect.y..scope_rect.y + scope_rect.h].join("\n");
        // The x-axis rule and a time-range label both survive inside the pane.
        assert!(
            scope_text.contains('\u{2514}'),
            "the scope keeps its x-axis rule in an 80x24 pane:\n{scope_text}"
        );
        assert!(
            scope_text.contains('\u{2524}'),
            "the scope keeps its y-axis:\n{scope_text}"
        );
        // A braille trace was drawn (the plot has real height, not clipped away).
        assert!(
            scope_text
                .chars()
                .any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)),
            "the waveform drew braille dots inside the pane:\n{scope_text}"
        );
    }

    /// Edit-mode chrome takes precedence over the overlay: with the overlay toggled
    /// on, entering edit mode still shows the selection marker and the edit hint
    /// (`CP-UX-M8.1`).
    #[test]
    fn edit_chrome_shows_even_when_overlay_is_on() {
        let mut a = app();
        a.overlay = true;
        a.focus = PanelId::Schematic;
        key(&mut a, KeyCode::Char('e')); // enter edit mode
        assert!(a.edit_mode(), "edit mode is active");
        let frame = a.render(80, 24);
        assert!(
            frame.contains("[edit]"),
            "the schematic pane shows the edit title over the overlay:\n{frame}"
        );
        assert!(
            frame.contains("[a]dd") || frame.contains('\u{25b8}'),
            "the edit hint / selection marker is visible:\n{frame}"
        );
    }

    /// A click on an open palette interacts with it (runs the clicked command)
    /// rather than falling through to the pane behind; a click outside dismisses it
    /// (`CP-UX-M8.1`).
    #[test]
    fn palette_click_selects_and_dismisses() {
        let (w, h) = (80, 24);
        let mut a = app();
        a.set_size(w, h);
        a.open_palette(); // query empty → every command matches, in list order
        let rect = a.palette_rect(w, h);
        // The first match row sits two rows below the box top (border + query line).
        // COMMANDS[0] is "focus schematic".
        let click_row = rect.y + 2;
        let click_col = rect.x + 3;
        a.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseKind::Down,
            col: click_col,
            row: click_row,
        }));
        assert!(
            !a.palette_open(),
            "clicking a command row runs it and closes the palette"
        );
        assert_eq!(
            a.focus(),
            PanelId::Schematic,
            "the clicked command took effect"
        );

        // A click well outside the box just dismisses the palette.
        a.open_palette();
        a.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseKind::Down,
            col: 0,
            row: h - 1,
        }));
        assert!(
            !a.palette_open(),
            "a click outside the box dismisses the palette"
        );
    }

    /// The shell prompt draws an input caret while the shell holds focus.
    #[test]
    fn shell_prompt_has_an_input_caret_when_focused() {
        let mut a = app();
        a.focus = PanelId::Shell;
        typ(&mut a, "add R2");
        let frame = a.render(80, 24);
        assert!(
            frame.contains("> add R2\u{258f}"),
            "the caret follows the typed input:\n{frame}"
        );
    }
}
