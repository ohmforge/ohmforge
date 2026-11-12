//! The live, dockable TUI application state machine (`CP-UX-M1.4`).
//!
//! This is the first *interactive* layer of `ee-tui`. Everything below the
//! terminal I/O is a pure state machine: focus switching, panel resize/dock, the
//! command palette (fuzzy filter + selection), and the shell input line are all
//! methods that take an abstract [`AppEvent`] and mutate [`App`], and
//! [`App::render`] is a pure function from state to a single `String` of a given
//! width and height. No `crossterm`, no raw mode, no `tty` — so the whole thing
//! is unit-testable the way [`Screen`](crate::Screen) and [`Plot`](crate::Plot)
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

use ee_core::value::Value;

use crate::current::{branch_currents, BranchCurrents};
use crate::overlay::{NodeVoltages, Overlay};
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
            _ if self.focus == PanelId::Shell => self.on_shell_key(key),
            _ => {}
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

    /// Advance the current-flow animation by one frame. The run loop calls this on
    /// each redraw tick; it is a pure state bump so the flow arrows march.
    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
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
            PanelId::Schematic if self.overlay => {
                let title = focus_title("op-point", focused);
                Overlay::new(&title)
                    .width(rect.w)
                    .color(false)
                    .flow(&self.currents, self.tick)
                    .pane_lines(self.backend.circuit(), &self.voltages, rect.h)
            }
            PanelId::Schematic => {
                let rows = Schematic::new("schematic")
                    .width(rect.w)
                    .rows(self.backend.circuit());
                let (base, name) = if self.edit.active {
                    (self.edit_schematic_body(&rows), "schematic [edit]")
                } else if rows.is_empty() {
                    ("(no parts)".to_string(), "schematic")
                } else {
                    (rows.join("\n"), "schematic")
                };
                let title = focus_title(name, focused);
                Pane::new(&title, &base).render(rect.w, rect.h)
            }
            PanelId::Scope => {
                let body = if self.scope.is_empty() {
                    "(run a transient to plot)".to_string()
                } else {
                    self.scope.clone()
                };
                let title = focus_title("scope", focused);
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

    /// The shell pane body: the most recent log lines, then the input prompt.
    fn shell_body(&self, pane_h: usize) -> String {
        let rows = pane_h.saturating_sub(2); // interior rows inside the frame
        let prompt = format!("> {}", self.input);
        if rows == 0 {
            return prompt;
        }
        let log_rows = rows - 1;
        let start = self.log.len().saturating_sub(log_rows);
        let mut body: Vec<&str> = self.log[start..].iter().map(String::as_str).collect();
        body.push(&prompt);
        body.join("\n")
    }

    /// Draw the command palette as a centred, framed overlay.
    fn render_palette(&self, grid: &mut [Vec<char>], w: usize, h: usize) {
        let matches = self.palette_matches();
        let box_w = 40.min(w.saturating_sub(4)).max(12);
        // One row for the query, up to eight match rows, plus two border rows.
        let shown = matches.len().min(8);
        let box_h = (shown + 3).min(h.saturating_sub(2)).max(3);

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
        let x = (w - box_w) / 2;
        let y = (h - box_h) / 2;
        blit(grid, &lines, x, y);
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
}
