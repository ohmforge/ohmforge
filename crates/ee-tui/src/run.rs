//! The crossterm raw-mode glue for the live app (`CP-UX-M1.4`).
//!
//! This module is intentionally thin: it is the *only* place in `ee-tui` that
//! touches a real terminal. All it does is enter raw mode, the alternate screen,
//! and mouse capture, translate each crossterm event into an [`AppEvent`], feed
//! it to the pure [`App`] state machine, and print whatever [`App::render`]
//! returns. The interesting logic is in [`app`](crate::app) and is unit-tested
//! without a tty; the translation functions here ([`map_key`], [`map_mouse`])
//! are pure and tested too, so only the `run` loop itself (I/O) is untested.
//!
//! The binary calls [`run`] with a [`Backend`] (e.g. `ee-shell`'s `Shell`):
//!
//! ```no_run
//! # use ee_tui::app::Backend;
//! # fn demo(shell: impl Backend) -> std::io::Result<()> {
//! ee_tui::run(shell) // takes over the terminal until the user quits
//! # }
//! ```

use std::io::{self, Write};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode as CtKeyCode, KeyEvent as CtKey,
    KeyEventKind, KeyModifiers, MouseButton, MouseEvent as CtMouse, MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute, queue};

use crate::app::{App, AppEvent, Backend, KeyCode, KeyEvent, MouseEvent, MouseKind};

/// Run the live TUI over `backend`, taking over the terminal until the user
/// quits (Ctrl-C, or the palette's `quit`).
///
/// Sets up and tears down raw mode / alternate screen / mouse capture around the
/// event loop, restoring the terminal even if the loop returns an error. This is
/// the `pub fn run` entry point the binary wires up; it does not touch the `ee`
/// binary itself.
pub fn run<B: Backend>(backend: B) -> io::Result<()> {
    let mut out = io::stdout();
    enable_raw_mode()?;
    execute!(out, EnterAlternateScreen, EnableMouseCapture, cursor::Hide)?;

    let mut app = App::new(backend);
    let result = event_loop(&mut app, &mut out);

    execute!(out, cursor::Show, DisableMouseCapture, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    result
}

/// The draw/read loop: draw the frame, read one event, feed it to the app.
fn event_loop<B: Backend>(app: &mut App<B>, out: &mut impl Write) -> io::Result<()> {
    loop {
        let (cols, rows) = crossterm::terminal::size()?;
        let (w, h) = (cols as usize, rows as usize);
        app.set_size(w, h);
        draw(out, &app.render(w, h))?;

        if app.should_quit() {
            return Ok(());
        }

        if let Some(event) = map_event(event::read()?, w, h) {
            app.handle_event(event);
        }
    }
}

/// Paint a rendered frame to the alternate screen.
fn draw(out: &mut impl Write, frame: &str) -> io::Result<()> {
    queue!(out, cursor::MoveTo(0, 0), Clear(ClearType::All))?;
    for (row, line) in frame.lines().enumerate() {
        queue!(out, cursor::MoveTo(0, row as u16))?;
        out.write_all(line.as_bytes())?;
    }
    out.flush()
}

/// Translate a crossterm [`Event`] into an [`AppEvent`], or drop it.
fn map_event(event: Event, _w: usize, _h: usize) -> Option<AppEvent> {
    match event {
        Event::Key(k) => map_key(k).map(AppEvent::Key),
        Event::Mouse(m) => map_mouse(m).map(AppEvent::Mouse),
        Event::Resize(cols, rows) => Some(AppEvent::Resize(cols as usize, rows as usize)),
        _ => None,
    }
}

/// Translate a crossterm key event into the app's abstract [`KeyEvent`].
///
/// Pure: only the key code and the Ctrl modifier survive. Key *release* events
/// (Windows) are dropped so a press is not counted twice.
pub(crate) fn map_key(k: CtKey) -> Option<KeyEvent> {
    if k.kind == KeyEventKind::Release {
        return None;
    }
    let code = match k.code {
        CtKeyCode::Char(c) => KeyCode::Char(c),
        CtKeyCode::Enter => KeyCode::Enter,
        CtKeyCode::Esc => KeyCode::Esc,
        CtKeyCode::Tab => KeyCode::Tab,
        CtKeyCode::BackTab => KeyCode::BackTab,
        CtKeyCode::Backspace => KeyCode::Backspace,
        CtKeyCode::Up => KeyCode::Up,
        CtKeyCode::Down => KeyCode::Down,
        CtKeyCode::Left => KeyCode::Left,
        CtKeyCode::Right => KeyCode::Right,
        _ => return None,
    };
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    Some(KeyEvent { code, ctrl })
}

/// Translate a crossterm mouse event into the app's [`MouseEvent`], or drop it.
pub(crate) fn map_mouse(m: CtMouse) -> Option<MouseEvent> {
    let kind = match m.kind {
        MouseEventKind::Down(MouseButton::Left) => MouseKind::Down,
        MouseEventKind::ScrollUp => MouseKind::ScrollUp,
        MouseEventKind::ScrollDown => MouseKind::ScrollDown,
        _ => return None,
    };
    Some(MouseEvent {
        kind,
        col: m.column as usize,
        row: m.row as usize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_char_maps_without_ctrl() {
        let k = map_key(CtKey::new(CtKeyCode::Char('a'), KeyModifiers::NONE)).unwrap();
        assert_eq!(k.code, KeyCode::Char('a'));
        assert!(!k.ctrl);
    }

    #[test]
    fn ctrl_p_carries_the_ctrl_modifier() {
        let k = map_key(CtKey::new(CtKeyCode::Char('p'), KeyModifiers::CONTROL)).unwrap();
        assert_eq!(k.code, KeyCode::Char('p'));
        assert!(k.ctrl, "Ctrl modifier survives translation");
    }

    #[test]
    fn arrows_and_tab_map_through() {
        for (ct, want) in [
            (CtKeyCode::Up, KeyCode::Up),
            (CtKeyCode::Tab, KeyCode::Tab),
            (CtKeyCode::Enter, KeyCode::Enter),
            (CtKeyCode::Esc, KeyCode::Esc),
        ] {
            let k = map_key(CtKey::new(ct, KeyModifiers::NONE)).unwrap();
            assert_eq!(k.code, want);
        }
    }

    #[test]
    fn key_release_is_dropped() {
        let mut k = CtKey::new(CtKeyCode::Char('a'), KeyModifiers::NONE);
        k.kind = KeyEventKind::Release;
        assert!(map_key(k).is_none(), "release events are not re-counted");
    }

    #[test]
    fn left_click_maps_to_a_down_at_its_cell() {
        let m = CtMouse {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };
        let mapped = map_mouse(m).unwrap();
        assert_eq!(mapped.kind, MouseKind::Down);
        assert_eq!((mapped.col, mapped.row), (12, 3));
    }

    #[test]
    fn mouse_move_is_dropped() {
        let m = CtMouse {
            kind: MouseEventKind::Moved,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        assert!(map_mouse(m).is_none());
    }

    #[test]
    fn resize_event_maps_to_the_new_size() {
        let ev = map_event(Event::Resize(120, 40), 80, 24);
        assert_eq!(ev, Some(AppEvent::Resize(120, 40)));
    }
}
