//! CP-PLAT-M10.2 — property tests for the boolean [`Expr`] parser, the HDL-lite
//! text path. Invariants asserted:
//!
//! - **Never panics** on arbitrary UTF-8 or on boolean-expression-shaped tokens.
//! - A **grammar-generated valid expression always parses**, and the parsed tree
//!   **evaluates totally and deterministically** (a `Logic` out for every input
//!   assignment, same answer every time — the `X`/`Z`-propagating contract).
//!
//! `proptest` is a dev-dependency only, so `ohmforge-digital` stays
//! dependency-free.

use ohmforge_digital::hdl::Expr;
use ohmforge_digital::Logic;
use proptest::prelude::*;

const NAMES: [&str; 4] = ["a", "b", "c", "d"];

fn any_logic() -> impl Strategy<Value = Logic> {
    prop_oneof![
        Just(Logic::Low),
        Just(Logic::High),
        Just(Logic::X),
        Just(Logic::Z),
    ]
}

/// A recursive strategy that emits *valid* boolean-expression source over the
/// four declared input names, constants, NOT, and the binary operators.
fn valid_expr() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        Just("a".to_string()),
        Just("b".to_string()),
        Just("c".to_string()),
        Just("d".to_string()),
        Just("0".to_string()),
        Just("1".to_string()),
    ];
    leaf.prop_recursive(6, 48, 2, |inner| {
        prop_oneof![
            inner.clone().prop_map(|e| format!("!{e}")),
            inner.clone().prop_map(|e| format!("({e})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} & {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} | {b})")),
            (inner.clone(), inner).prop_map(|(a, b)| format!("({a} ^ {b})")),
        ]
    })
}

proptest! {
    // Arbitrary text must never panic — only Ok or an ExprError.
    #[test]
    fn parse_never_panics(s in "(?s).{0,64}") {
        let _ = Expr::parse(&s, &NAMES);
    }

    // Boolean-expression-shaped tokens (operators, parens, names, word aliases)
    // must never panic.
    #[test]
    fn parse_boolean_tokens_never_panic(
        s in "[ ()!&|^01abcd]{0,48}|(a|b|c|d|and|or|xor|not|!| |\\(|\\)){0,24}"
    ) {
        let _ = Expr::parse(&s, &NAMES);
    }

    // A grammar-valid expression always parses, and its evaluation is total and
    // deterministic for any 4-input assignment.
    #[test]
    fn valid_expr_parses_and_evals(
        src in valid_expr(),
        inputs in prop::collection::vec(any_logic(), 4),
    ) {
        let e = Expr::parse(&src, &NAMES)
            .unwrap_or_else(|err| panic!("grammar-valid `{src}` failed to parse: {err}"));
        let out1 = e.eval(&inputs);
        let out2 = e.eval(&inputs);
        prop_assert_eq!(out1, out2);
    }
}
