//! Visual schematic capture — a mouse-driven grid canvas (`CP-UX-M9.1`).
//!
//! This is the drag-and-drop schematic editor: click to drop a part on a grid,
//! drag a part to move it, and drag from one part's terminal to another's to wire
//! them. Crucially it is **not a parallel circuit model** — every topological edit
//! it makes is lowered to the same [`EditAction`] grammar
//! the typed shell line and the keyboard editor use (`CP-UX-M5.1`/`M8`), so the
//! `Circuit` the canvas builds *is* the one the simulator runs. The canvas owns
//! only the *visual* layer the circuit graph does not carry: where each part sits
//! on the grid, and the node name each terminal was given.
//!
//! Like the rest of [`app`](crate::app) it is a pure state machine: it consumes
//! abstract [`CanvasEvent`]s (grid cells, no pixels, no tty) and returns the
//! [`EditAction`]s a gesture produced, which the [`App`](crate::app::App) feeds
//! through its backend. A mouse *move* while a part terminal is held draws a wire;
//! a mouse *move* while a part body is held moves the part; a plain click on empty
//! space places the selected part kind. Everything here is unit-testable by
//! feeding cells and asserting on the resulting circuit.

use crate::app::EditAction;

/// A cell on the capture grid (origin top-left), in canvas coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub col: usize,
    pub row: usize,
}

impl Cell {
    /// A grid cell at `(col, row)`.
    pub fn new(col: usize, row: usize) -> Self {
        Self { col, row }
    }
}

/// The kind of part the next placement drops onto the canvas.
///
/// A deliberately small palette of the two-terminal primitives — the ones a
/// click-to-place grid handles unambiguously. Each maps to the shell's `add`
/// grammar via its [`designator`](PartKind::designator) and a sensible
/// [`default_value`](PartKind::default_value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    Resistor,
    Capacitor,
    Inductor,
    VoltageSource,
    CurrentSource,
}

impl PartKind {
    /// The reference-designator letter (`R`/`C`/`L`/`V`/`I`) this kind places.
    pub fn designator(&self) -> char {
        match self {
            PartKind::Resistor => 'R',
            PartKind::Capacitor => 'C',
            PartKind::Inductor => 'L',
            PartKind::VoltageSource => 'V',
            PartKind::CurrentSource => 'I',
        }
    }

    /// The default value a fresh placement gets, in the shell's value notation.
    pub fn default_value(&self) -> &'static str {
        match self {
            PartKind::Resistor => "1k",
            PartKind::Capacitor => "1u",
            PartKind::Inductor => "1m",
            PartKind::VoltageSource => "5",
            PartKind::CurrentSource => "1m",
        }
    }
}

/// A part placed on the canvas: its reference, kind, anchor cell, and the node
/// name each of its two terminals carries.
///
/// A part spans three cells on one row: the left terminal at [`anchor`](Placement::anchor),
/// the body one cell right, and the right terminal two cells right. The node
/// names are what wiring merges, so they are the canvas's handle on the real
/// circuit's nets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub reference: String,
    pub kind: PartKind,
    /// The left-terminal cell; the body and right terminal follow it on the row.
    pub anchor: Cell,
    /// The node names of `[left terminal, right terminal]`.
    pub nodes: [String; 2],
}

impl Placement {
    /// The body (centre) cell — what a drag grabs to move the part.
    pub fn body(&self) -> Cell {
        Cell::new(self.anchor.col + 1, self.anchor.row)
    }

    /// The two terminal cells `[left, right]` — what a drag grabs to draw a wire.
    pub fn terminals(&self) -> [Cell; 2] {
        [self.anchor, Cell::new(self.anchor.col + 2, self.anchor.row)]
    }
}

/// A mouse gesture on the canvas, in grid-cell coordinates.
///
/// The [`App`](crate::app::App) translates real pointer events into these: a
/// button press is [`Down`](CanvasEvent::Down), motion with the button held is
/// [`Drag`](CanvasEvent::Drag), and release is [`Up`](CanvasEvent::Up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasEvent {
    Down(Cell),
    Drag(Cell),
    Up(Cell),
}

/// What a cell press landed on.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Hit {
    /// Empty grid — a click here places a part.
    Empty,
    /// A part's body — a drag here moves the part. Carries its index.
    Body(usize),
    /// A part's terminal — a drag here draws a wire. Carries `(part index,
    /// terminal 0/1)`.
    Terminal(usize, usize),
}

/// The in-progress gesture.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Gesture {
    /// Nothing held.
    Idle,
    /// Button down, no motion yet — still could become a click, a move or a wire.
    Pressed { at: Cell, hit: Hit },
    /// Dragging a placed part by its body.
    MovingPart { index: usize },
    /// Dragging a wire from a terminal's node.
    Wiring { from_node: String },
}

/// The visual schematic-capture state machine.
///
/// Holds the placed parts, the part kind the next click drops, and the live
/// gesture. Feed it [`CanvasEvent`]s with [`handle`](Canvas::handle); it returns
/// the [`EditAction`]s the gesture produced (empty for a pure visual move).
#[derive(Debug)]
pub struct Canvas {
    placements: Vec<Placement>,
    selected: PartKind,
    gesture: Gesture,
    /// Monotonic suffix for fresh node names (`n1`, `n2`, …).
    next_node: u32,
    /// Per-designator counters for fresh references (`R1`, `R2`, …).
    ref_counts: std::collections::BTreeMap<char, u32>,
}

impl Default for Canvas {
    fn default() -> Self {
        Self {
            placements: Vec::new(),
            selected: PartKind::Resistor,
            gesture: Gesture::Idle,
            next_node: 0,
            ref_counts: std::collections::BTreeMap::new(),
        }
    }
}

impl Canvas {
    /// A fresh, empty canvas with the resistor selected.
    pub fn new() -> Self {
        Self::default()
    }

    /// The part kind the next placement will drop.
    pub fn selected_kind(&self) -> PartKind {
        self.selected
    }

    /// Choose the part kind the next click places.
    pub fn select_kind(&mut self, kind: PartKind) {
        self.selected = kind;
    }

    /// The parts placed on the canvas so far.
    pub fn placements(&self) -> &[Placement] {
        &self.placements
    }

    /// The placement with the given reference, if any (for tests and rendering).
    pub fn placement(&self, reference: &str) -> Option<&Placement> {
        self.placements.iter().find(|p| p.reference == reference)
    }

    /// React to one mouse gesture event, returning the [`EditAction`]s it
    /// produced. Pure: a placement/wire returns the edit to apply, a visual move
    /// returns nothing, and every circuit-topology change goes through the
    /// returned actions — never a side channel.
    pub fn handle(&mut self, event: CanvasEvent) -> Vec<EditAction> {
        match event {
            CanvasEvent::Down(cell) => {
                let hit = self.hit_test(cell);
                self.gesture = Gesture::Pressed { at: cell, hit };
                Vec::new()
            }
            CanvasEvent::Drag(cell) => {
                self.on_drag(cell);
                Vec::new()
            }
            CanvasEvent::Up(cell) => self.on_up(cell),
        }
    }

    /// Promote a press into a move/wire once the pointer leaves the press cell, or
    /// advance an in-flight part move.
    fn on_drag(&mut self, cell: Cell) {
        match &self.gesture {
            Gesture::Pressed { at, hit } if cell != *at => match hit {
                Hit::Body(index) => self.gesture = Gesture::MovingPart { index: *index },
                Hit::Terminal(index, term) => {
                    let from_node = self.placements[*index].nodes[*term].clone();
                    self.gesture = Gesture::Wiring { from_node };
                }
                // A drag that began on empty grid is a no-op (no rubber-band select).
                Hit::Empty => {}
            },
            Gesture::MovingPart { index } => {
                // Live-move the part body to track the pointer (visual only).
                let index = *index;
                self.set_body(index, cell);
            }
            _ => {}
        }
    }

    /// Resolve the release: a click places (empty) or selects (on a part); a part
    /// move finalises visually; a wire from a terminal to a *different* terminal
    /// emits a [`Wire`](EditAction::Wire).
    fn on_up(&mut self, cell: Cell) -> Vec<EditAction> {
        let gesture = std::mem::replace(&mut self.gesture, Gesture::Idle);
        match gesture {
            // A press that never moved is a click.
            Gesture::Pressed { hit, .. } => match hit {
                Hit::Empty => self.place(cell).into_iter().collect(),
                // Clicking a part just selects it (no edit); selection is implicit
                // in the hit, nothing to record.
                Hit::Body(_) | Hit::Terminal(_, _) => Vec::new(),
            },
            Gesture::MovingPart { index } => {
                self.set_body(index, cell);
                Vec::new()
            }
            Gesture::Wiring { from_node } => match self.hit_test(cell) {
                Hit::Terminal(index, term) => {
                    let to_node = self.placements[index].nodes[term].clone();
                    if to_node == from_node {
                        Vec::new()
                    } else {
                        vec![EditAction::Wire {
                            node_a: from_node,
                            node_b: to_node,
                        }]
                    }
                }
                _ => Vec::new(),
            },
            Gesture::Idle => Vec::new(),
        }
    }

    /// Place the selected kind so its body sits at `cell`, returning the `add`
    /// edit. Fresh reference + two fresh node names; the value is the kind's
    /// default. The anchor (left terminal) is one cell left of the body, clamped
    /// so it never underflows column 0.
    fn place(&mut self, cell: Cell) -> Option<EditAction> {
        let kind = self.selected;
        let d = kind.designator();
        let n = self.ref_counts.entry(d).or_insert(0);
        *n += 1;
        let reference = format!("{d}{n}");
        let node_a = self.fresh_node();
        let node_b = self.fresh_node();
        let anchor = Cell::new(cell.col.saturating_sub(1), cell.row);
        self.placements.push(Placement {
            reference: reference.clone(),
            kind,
            anchor,
            nodes: [node_a.clone(), node_b.clone()],
        });
        Some(EditAction::Place {
            reference,
            node_a,
            node_b,
            value: kind.default_value().to_string(),
        })
    }

    /// Move a placed part so its body lands on `cell` (clamped like [`place`]).
    fn set_body(&mut self, index: usize, cell: Cell) {
        if let Some(p) = self.placements.get_mut(index) {
            p.anchor = Cell::new(cell.col.saturating_sub(1), cell.row);
        }
    }

    /// Allocate the next fresh node name.
    fn fresh_node(&mut self) -> String {
        self.next_node += 1;
        format!("n{}", self.next_node)
    }

    /// Render the capture grid to `height` rows of `width` cells: each part's two
    /// terminals as `○`, its body as the designator letter, joined by a `─` lead.
    /// A pure, deterministic view of the visual layer (the circuit graph the
    /// edits build is drawn by the ordinary schematic/overlay panes).
    pub fn render(&self, width: usize, height: usize) -> String {
        let mut grid = vec![vec![' '; width.max(1)]; height.max(1)];
        let put = |grid: &mut Vec<Vec<char>>, c: Cell, ch: char| {
            if c.row < grid.len() && c.col < grid[0].len() {
                grid[c.row][c.col] = ch;
            }
        };
        for p in &self.placements {
            let [t0, t1] = p.terminals();
            put(&mut grid, t0, '○');
            put(&mut grid, p.body(), p.kind.designator());
            put(&mut grid, t1, '○');
        }
        grid.into_iter()
            .map(|r| r.into_iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Classify what grid `cell` lands on. A body takes precedence over a terminal
    /// only when they collide (they do not for a single part), and the first part
    /// in placement order wins an overlap.
    fn hit_test(&self, cell: Cell) -> Hit {
        for (i, p) in self.placements.iter().enumerate() {
            if p.body() == cell {
                return Hit::Body(i);
            }
            for (t, tc) in p.terminals().iter().enumerate() {
                if *tc == cell {
                    return Hit::Terminal(i, t);
                }
            }
        }
        Hit::Empty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a down-then-up (a click) at `cell` and return the actions.
    fn click(canvas: &mut Canvas, cell: Cell) -> Vec<EditAction> {
        assert!(canvas.handle(CanvasEvent::Down(cell)).is_empty());
        canvas.handle(CanvasEvent::Up(cell))
    }

    /// A click on empty grid places the selected kind, and the produced edit is a
    /// `Place` — i.e. it lowers to the shell's `add` verb, which the existing
    /// snapshot/undo machinery already wraps, so a placement is undoable for free
    /// via the shared mutation path (`CP-UX-M5.1`/`CP-UX-M2.4`).
    #[test]
    fn a_click_places_the_selected_kind_through_the_add_path() {
        let mut c = Canvas::new();
        c.select_kind(PartKind::Resistor);
        let actions = click(&mut c, Cell::new(5, 3));
        assert_eq!(
            actions,
            vec![EditAction::Place {
                reference: "R1".into(),
                node_a: "n1".into(),
                node_b: "n2".into(),
                value: "1k".into(),
            }],
            "a place lowers to the undoable `add` verb"
        );
        assert_eq!(c.placements().len(), 1);

        // A second resistor gets the next reference and fresh nodes.
        c.select_kind(PartKind::VoltageSource);
        let actions = click(&mut c, Cell::new(5, 8));
        assert_eq!(
            actions,
            vec![EditAction::Place {
                reference: "V1".into(),
                node_a: "n3".into(),
                node_b: "n4".into(),
                value: "5".into(),
            }]
        );
    }

    /// Pressing a placed part's body, moving, and releasing moves it to the new
    /// cell — a purely visual edit that emits **no** circuit action.
    #[test]
    fn drag_moves_a_placed_part() {
        let mut c = Canvas::new();
        click(&mut c, Cell::new(5, 3));
        let before = c.placement("R1").unwrap().body();
        assert_eq!(before, Cell::new(5, 3));

        // Grab the body and drag it to a new cell.
        assert!(c.handle(CanvasEvent::Down(before)).is_empty());
        assert!(c.handle(CanvasEvent::Drag(Cell::new(20, 10))).is_empty());
        let actions = c.handle(CanvasEvent::Up(Cell::new(20, 10)));
        assert!(actions.is_empty(), "a move is visual only, no circuit edit");

        let after = c.placement("R1").unwrap().body();
        assert_eq!(after, Cell::new(20, 10), "the part moved to the drop cell");
        assert_eq!(c.placements().len(), 1, "still one part");
    }

    /// Dragging from one terminal to another emits a `Wire` of the two terminals'
    /// node names.
    #[test]
    fn dragging_between_terminals_emits_a_wire() {
        let mut c = Canvas::new();
        click(&mut c, Cell::new(5, 3)); // R1: nodes n1,n2
        click(&mut c, Cell::new(5, 8)); // R2: nodes n3,n4

        let r1_right = c.placement("R1").unwrap().terminals()[1];
        let r2_left = c.placement("R2").unwrap().terminals()[0];

        assert!(c.handle(CanvasEvent::Down(r1_right)).is_empty());
        assert!(c.handle(CanvasEvent::Drag(r2_left)).is_empty());
        let actions = c.handle(CanvasEvent::Up(r2_left));
        assert_eq!(
            actions,
            vec![EditAction::Wire {
                node_a: "n2".into(),
                node_b: "n3".into(),
            }]
        );
    }

    /// A wire that ends on the same terminal, or on empty grid, is a clean no-op.
    #[test]
    fn a_dangling_or_self_wire_does_nothing() {
        let mut c = Canvas::new();
        click(&mut c, Cell::new(5, 3));
        let t = c.placement("R1").unwrap().terminals()[0];
        c.handle(CanvasEvent::Down(t));
        c.handle(CanvasEvent::Drag(Cell::new(40, 40))); // empty grid
        assert!(c.handle(CanvasEvent::Up(Cell::new(40, 40))).is_empty());
    }
}
