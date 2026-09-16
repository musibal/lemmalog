//! The clock-sync fast path: advancing the clock between two reads may skip
//! the full re-derivation only when nothing the temporal view depends on
//! changes across the advance.

use lemmalog::{Ann, Engine, Value};

fn edge(e: &mut Engine, subj: &str, pred: &str, obj: &str, vf: i64, vt: i64) {
    let mut args = vec![e.sym(subj), e.sym(pred), e.sym(obj)];
    args.extend([Value::Int(vf), Value::Int(vt), Value::Int(1)]);
    e.declare("edge", &args, Ann::unit());
}

const CURRENT: &str = "current(E,R,O) :- edge(E,R,O,VF,VT,_), now(T), VF =< T, T < VT.";

fn current(e: &mut Engine, s: &str) -> bool {
    let sym = e.sym(s);
    !e.query("current", &[Some(sym), None, None]).is_empty()
}

/// A read that advances the clock with no valid-from/valid-to in between
/// leaves the view untouched, so the recompute is skippable — and the view
/// must still answer as of the new clock.
#[test]
fn inert_advance_keeps_view_and_answers_as_of_new_clock() {
    let mut e = Engine::new();
    e.install_program(CURRENT).unwrap();
    edge(&mut e, "alice", "works_at", "acme", 0, i64::MAX);
    e.set_now(100);
    e.run();
    assert!(current(&mut e, "alice"), "visible at t=100");

    assert!(
        e.clock_advance_is_inert(100, 200),
        "open-ended edge crosses no boundary, so the advance is inert"
    );
    e.set_now(200);
    assert!(current(&mut e, "alice"), "still visible at t=200 without recompute");
}

/// The advance that does cross an expiry must NOT be treated as inert, and
/// going through the invalidation must actually retire the fact.
#[test]
fn advance_across_expiry_is_not_inert_and_retires_the_fact() {
    let mut e = Engine::new();
    e.install_program(CURRENT).unwrap();
    edge(&mut e, "bob", "works_at", "acme", 0, 150);
    e.set_now(100);
    e.run();
    assert!(current(&mut e, "bob"), "visible at t=100, expires at 150");

    assert!(
        !e.clock_advance_is_inert(100, 200),
        "valid-to 150 lies in (100,200]"
    );
    e.set_now(200);
    e.invalidate_derived();
    e.run();
    assert!(!current(&mut e, "bob"), "expired by t=200");
}

/// A valid-from landing inside the interval is the same test in the other
/// direction: the fact appears.
#[test]
fn advance_across_valid_from_is_not_inert() {
    let mut e = Engine::new();
    e.install_program(CURRENT).unwrap();
    edge(&mut e, "carol", "works_at", "acme", 150, i64::MAX);
    e.set_now(100);
    e.run();
    assert!(!current(&mut e, "carol"), "not yet valid at t=100");

    assert!(
        !e.clock_advance_is_inert(100, 200),
        "valid-from 150 lies in (100,200]"
    );
    e.set_now(200);
    e.invalidate_derived();
    e.run();
    assert!(current(&mut e, "carol"), "valid by t=200");
}

/// The interval is half-open: a boundary exactly at the old clock was already
/// accounted for, one exactly at the new clock has not been.
#[test]
fn boundary_is_half_open() {
    let mut e = Engine::new();
    e.install_program(CURRENT).unwrap();
    edge(&mut e, "dana", "works_at", "acme", 0, 100);
    assert!(e.clock_advance_is_inert(100, 200), "expiry at old is not a flip");
    edge(&mut e, "erin", "works_at", "acme", 0, 200);
    assert!(!e.clock_advance_is_inert(100, 200), "expiry at new is a flip");
}

/// The `uso_relacion` aggregate present in the live store compares the clock
/// against the same edge boundaries as `current/3`, so it must NOT forfeit
/// the fast path — the whole point of testing the shape rather than matching
/// one known clause.
#[test]
fn other_edge_bounded_clock_rules_keep_fast_path() {
    let mut e = Engine::new();
    e.install_program(CURRENT).unwrap();
    e.install_program(
        "uso_relacion(R, count(O)) :- edge(_, R, O, VF, VT, _), now(T), VF =< T, T < VT.",
    )
    .unwrap();
    edge(&mut e, "frank", "works_at", "acme", 0, i64::MAX);
    assert!(
        e.clock_advance_is_inert(100, 200),
        "an aggregate guarded by the same VF/VT comparison is still boundary-bounded"
    );
}

/// A bare comparison against a valid-from is boundary-bounded too: it flips
/// exactly at an edge timestamp the scan covers.
#[test]
fn comparison_against_valid_from_keeps_fast_path() {
    let mut e = Engine::new();
    e.install_program(CURRENT).unwrap();
    edge(&mut e, "gail", "works_at", "acme", 0, i64::MAX);
    e.install_program("stale(E) :- now(T), edge(E,_,_,VF,_,_), VF < T.")
        .unwrap();
    assert!(e.clock_advance_is_inert(100, 200));
}

/// Comparing the clock against a literal instant flips at that constant,
/// which no scan of `edge` can see.
#[test]
fn clock_against_constant_disables_fast_path() {
    let mut e = Engine::new();
    e.install_program(CURRENT).unwrap();
    edge(&mut e, "hana", "works_at", "acme", 0, i64::MAX);
    e.install_program("epoch_passed(E) :- edge(E,_,_,_,_,_), now(T), T > 150.")
        .unwrap();
    assert!(!e.clock_advance_is_inert(100, 200));
}

/// Comparing the clock against a timestamp carried by another predicate
/// flips at a value living outside `edge`.
#[test]
fn clock_against_foreign_timestamp_disables_fast_path() {
    let mut e = Engine::new();
    e.install_program(CURRENT).unwrap();
    edge(&mut e, "ivan", "works_at", "acme", 0, i64::MAX);
    e.install_program("due(X) :- deadline(X, Ts), now(T), Ts =< T.")
        .unwrap();
    assert!(!e.clock_advance_is_inert(100, 200));
}

/// Arithmetic on the clock can change on any advance, boundary or not.
#[test]
fn arithmetic_on_clock_disables_fast_path() {
    let mut e = Engine::new();
    e.install_program(CURRENT).unwrap();
    edge(&mut e, "jane", "works_at", "acme", 0, i64::MAX);
    e.install_program("aged(E) :- edge(E,_,_,VF,_,_), now(T), VF < T - 30.")
        .unwrap();
    assert!(!e.clock_advance_is_inert(100, 200));
}

/// Projecting the clock into a derived head makes that relation change on
/// every advance.
#[test]
fn clock_in_head_disables_fast_path() {
    let mut e = Engine::new();
    e.install_program(CURRENT).unwrap();
    edge(&mut e, "kurt", "works_at", "acme", 0, i64::MAX);
    e.install_program("stamped(E,T) :- edge(E,_,_,_,_,_), now(T).")
        .unwrap();
    assert!(!e.clock_advance_is_inert(100, 200));
}

/// Without the canonical rule nothing is clock-dependent, so nothing to do.
#[test]
fn engine_without_clock_rule_is_always_inert() {
    let mut e = Engine::new();
    e.install_program("parent(X,Y) :- edge(X,\"p\",Y,_,_,_).").unwrap();
    edge(&mut e, "gus", "p", "hal", 0, i64::MAX);
    assert!(e.clock_advance_is_inert(100, 200));
}
