//! CLI reachability smoke tests for the `of` binary (CP-PLAT.6). These prove the
//! copilot is reachable from the command line a user actually types — the gap the
//! M8 audit flagged. They spawn the built binary via `CARGO_BIN_EXE_of` (set by
//! Cargo for integration tests) and need no network or API key.

use std::io::Write;
use std::process::{Command, Stdio};

/// `of review` on a piped eesh script runs the offline (no-key) copilot and
/// prints a grounded review, exiting 0.
#[test]
fn of_review_runs_offline_on_a_piped_script() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_of"))
        .arg("review")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn of");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"add V1 in gnd 5\nadd R1 in out 1k\nadd D1 out gnd\n")
        .unwrap();
    let out = child.wait_with_output().expect("wait of");
    assert!(
        out.status.success(),
        "of review exited non-zero; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.stdout.is_empty(),
        "of review printed nothing to stdout"
    );
}

/// `of drc` is the alias and behaves the same.
#[test]
fn of_drc_is_an_alias_for_review() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_of"))
        .arg("drc")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn of");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"add V1 in gnd 5\nadd R1 in gnd 1k\n")
        .unwrap();
    let out = child.wait_with_output().expect("wait of");
    assert!(out.status.success(), "of drc exited non-zero");
}

/// `of help` lists the AI subcommands so they're discoverable.
#[test]
fn of_help_lists_the_ai_subcommands() {
    let out = Command::new(env!("CARGO_BIN_EXE_of"))
        .arg("help")
        .output()
        .expect("run of help");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("of ask"), "help should mention `of ask`");
    assert!(text.contains("review"), "help should mention `review`");
}
