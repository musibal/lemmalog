//! False negatives on goals with a BOUND CONSTANT ("is this concrete fact
//! true?"): the fact exists, the engine answers "no such fact".
//!
//! Two faces, both measured on the live store 2026-09-15:
//!   1. a ground goal that HOLDS binds nothing, so `ask` returns one empty
//!      row and every text surface rendered it as "" — an empty answer that
//!      reads as "does not exist" (`multi("provee")`, 94 rows when free);
//!   2. a digit-looking object is stored as `Int` and displayed bare, so the
//!      quoted spelling of the value just read (`"3"`) resolved to a symbol
//!      that names nothing and the goal missed (413 `current` rows have an
//!      Int object).
//! Controls are part of the test: a constant that is really absent must
//! still answer zero rows, or the "fix" would be a goal that always holds.

use lemmalog::{answer_text, AgentMemory, MockExtractor};

fn mem(extra: &str) -> AgentMemory<MockExtractor> {
    AgentMemory::new(MockExtractor::new(0.9), extra).unwrap()
}

/// A ground goal that holds must survive the text surface as an answer.
#[test]
fn ground_goal_that_holds_is_not_an_empty_answer() {
    let mut m = mem("multi(\"sabor\").");
    m.maintain(100);
    let rows = m.ask("multi(\"sabor\")").unwrap();
    assert_eq!(rows.len(), 1, "the engine always answered: one empty row = holds");
    // the sentinel is what loses the answer when joined
    assert!(rows.join("\n").is_empty(), "ground rows carry no bindings");
    let text = answer_text(&rows).expect("a held goal must not render as no answer");
    assert!(!text.trim().is_empty(), "empty text reads as 'no such fact'");
    assert!(
        text.starts_with("true"),
        "the surface must say it holds, got: {text}"
    );
    // ...and a goal over a real FACT of a real relation, not a program decl
    let mut m2 = mem("");
    m2.observe_at("sabor --coste--> picante", 100);
    m2.maintain(100);
    assert_eq!(m2.ask("current(\"sabor\", \"coste\", \"picante\")").unwrap().len(), 1);
    assert!(answer_text(&m2.ask("current(\"sabor\", \"coste\", \"picante\")").unwrap()).is_some());
}

/// CONTROLS: a constant that is absent answers zero rows, and the surface
/// then hands back None (the caller explains the miss).
#[test]
fn absent_constant_is_still_zero_rows() {
    let mut m = mem("multi(\"sabor\").");
    m.maintain(100);
    assert!(m.ask("multi(\"nope\")").unwrap().is_empty());
    assert!(m.ask("multi(\"sabor\")").unwrap().len() == 1);
    assert_eq!(answer_text(&m.ask("multi(\"nope\")").unwrap()), None);
    // no rows at all on an unknown predicate either
    assert_eq!(answer_text(&m.ask("nada(\"sabor\")").unwrap()), None);
}

/// CONTROLS: an aggregate head with an impossible second constant is 0 rows.
#[test]
fn aggregate_with_impossible_constant_is_zero_rows() {
    let mut m = mem("");
    m.observe_at(
        "hyp_1 --evidence--> src/a.rs:1\nhyp_1 --evidence--> src/b.rs:2",
        100,
    );
    m.install_rules("evidence_count(H, count(E)) :- current(H, \"evidence\", E).")
        .unwrap();
    m.maintain(100);
    assert_eq!(m.ask("evidence_count(\"hyp_1\", N)").unwrap(), vec!["N=2".to_string()]);
    // fully ground and true
    assert_eq!(m.ask("evidence_count(\"hyp_1\", 2)").unwrap().len(), 1);
    assert_eq!(answer_text(&m.ask("evidence_count(\"hyp_1\", 2)").unwrap()), Some(
        "true — the ground goal holds (no variables to bind)".to_string()
    ));
    // impossible count, and a subject that has none: both must miss
    assert!(m.ask("evidence_count(\"hyp_1\", 999999)").unwrap().is_empty());
    assert!(m.ask("evidence_count(\"zz_absent\", 999999)").unwrap().is_empty());
    assert_eq!(answer_text(&m.ask("evidence_count(\"hyp_1\", 999999)").unwrap()), None);
}

/// A number the store holds as `Int` must be found in BOTH spellings: the
/// bare one it is displayed with, and the quoted one the tool schema teaches.
#[test]
fn quoted_number_finds_the_stored_int() {
    let mut m = mem("");
    m.observe_at("sabor --coste--> 3", 100);
    m.maintain(100);
    assert_eq!(m.ask("current(\"sabor\", \"coste\", 3)").unwrap().len(), 1);
    assert_eq!(
        m.ask("current(\"sabor\", \"coste\", \"3\")").unwrap().len(),
        1,
        "the quoted spelling of the number that was just read"
    );
    // CONTROLS: an absent number stays 0 rows in both spellings, and a
    // number in a relation whose numbers are not that one does not leak
    assert!(m.ask("current(\"sabor\", \"coste\", 4)").unwrap().is_empty());
    assert!(m.ask("current(\"sabor\", \"coste\", \"4\")").unwrap().is_empty());
    assert!(m.ask("current(\"sabor\", \"otro\", \"3\")").unwrap().is_empty());
}
