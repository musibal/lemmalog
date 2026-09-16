//! The annotation parameter, exercised at carriers that are not `Ann`.
//!
//! Upstream's own suite runs the default instantiation, so it proves the
//! parameterisation changed nothing - but it never builds an `Engine<A>` for
//! any other `A`, and so it cannot see whether the trait hooks are actually
//! wired into the fixpoint, what the evaluator hands them, or what the trait's
//! own defaults do when a carrier leans on them.

use std::collections::BTreeSet;

use lemmalog::{eval::Key, AggFn, Annotation, ClauseId, Engine, Value};

/// A carrier that counts the body atoms multiplied into an annotation and
/// records every derivation stamped onto it, and that **deliberately
/// implements no [`lemmalog::Interpret`]**.
///
/// That absence is an assertion, not an omission. The evaluator is bounded by
/// `A: Annotation`, and `Interpret<V>: Annotation` is a subtrait of it, so a
/// bound on `Annotation` grants nothing about `Interpret`. If any evaluation
/// path could read an annotation through `interpret`, `Engine<A>` would need
/// an `Interpret` bound and this file would stop compiling. It compiling is
/// the proof that a read-time interpretation - an arithmetic mean, a median,
/// anything the fixpoint could not lawfully fold - cannot enter the fixpoint.
#[derive(Debug, Clone, PartialEq)]
struct Tally {
    atoms: u32,
    /// What `times` and `negate` folded in. Kept apart from `derivations` so
    /// that counting derivations counts derivations.
    marks: BTreeSet<String>,
    /// One entry per derivation the evaluator stamped, as the carrier sees it:
    /// the clause identity it was handed, and that firing's body predicates.
    derivations: BTreeSet<(ClauseId, String)>,
}

impl Annotation for Tally {
    fn one() -> Self {
        Tally {
            atoms: 0,
            marks: BTreeSet::new(),
            derivations: BTreeSet::new(),
        }
    }

    fn zero() -> Self {
        Tally {
            atoms: 0,
            marks: BTreeSet::from(["<zero>".to_owned()]),
            derivations: BTreeSet::new(),
        }
    }

    fn times(&self, other: &Self) -> Self {
        // law 4: zero annihilates. Cheap here, and this carrier is the worked
        // example a future carrier author will copy.
        if self.is_zero() || other.is_zero() {
            return Self::zero();
        }

        Tally {
            atoms: self.atoms + other.atoms,
            marks: self.marks.union(&other.marks).cloned().collect(),
            derivations: self
                .derivations
                .union(&other.derivations)
                .cloned()
                .collect(),
        }
    }

    fn plus(&self, other: &Self) -> Self {
        Tally {
            atoms: self.atoms.max(other.atoms),
            marks: self.marks.union(&other.marks).cloned().collect(),
            derivations: self
                .derivations
                .union(&other.derivations)
                .cloned()
                .collect(),
        }
    }

    /// Reads the complement instead of pruning, the way a two-value carrier
    /// does: a blocked route still exists, still fires, and still stamps its
    /// own derivation. The trait default prunes, which is what `Minimal`
    /// below pins; this carrier deliberately does not, because a carrier that
    /// prunes can never observe two clauses that differ only in a negation.
    fn negate(found: Option<&Self>) -> Self {
        match found {
            None => Self::one(),
            Some(_) => Tally {
                atoms: 0,
                marks: BTreeSet::from(["<blocked>".to_owned()]),
                derivations: BTreeSet::new(),
            },
        }
    }

    fn derive(mut self, clause: ClauseId, body: &[Key]) -> Self {
        let preds: Vec<&str> = body.iter().map(|(pred, _)| pred.as_str()).collect();
        self.derivations.insert((clause, preds.join(",")));

        self
    }
}

/// A carrier that implements only the four methods the trait requires, so that
/// what the trait's *defaults* do on their own is visible rather than masked by
/// a carrier that happens to override them.
#[derive(Debug, Clone, PartialEq)]
struct Minimal(u32);

impl Annotation for Minimal {
    fn one() -> Self {
        Minimal(1)
    }

    fn zero() -> Self {
        Minimal(0)
    }

    fn times(&self, other: &Self) -> Self {
        Minimal(self.0 * other.0)
    }

    fn plus(&self, other: &Self) -> Self {
        Minimal(self.0.max(other.0))
    }
}

/// A carrier that records what the evaluator handed to `aggregate`: the head's
/// aggregate functions, and how many row annotations came with them.
///
/// The kinds are the point. `count` and `sum` fold the same group of rows, so
/// a carrier that wants to treat them differently - a count is right or wrong
/// as a whole, where a sum is only as wrong as the rows it adds up - cannot
/// get there from the rows alone.
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Zero,
    Fact,
    Aggregated { kinds: Vec<AggFn>, rows: usize },
}

impl Annotation for Seen {
    fn one() -> Self {
        Seen::Fact
    }

    fn zero() -> Self {
        Seen::Zero
    }

    fn times(&self, other: &Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return Seen::Zero;
        }

        self.clone()
    }

    fn plus(&self, other: &Self) -> Self {
        if self.is_zero() {
            return other.clone();
        }

        self.clone()
    }

    fn aggregate<'a>(fns: &[AggFn], rows: impl Iterator<Item = &'a Self>) -> Self
    where
        Self: 'a,
    {
        Seen::Aggregated {
            kinds: fns.to_vec(),
            rows: rows.count(),
        }
    }
}

#[test]
fn a_carrier_the_evaluator_cannot_interpret_still_drives_the_whole_fixpoint() {
    let mut engine: Engine<Tally> = Engine::default();
    engine
        .install_program("place: place_of(P, H) :- runs_in(P, C), hosted_by(C, H).")
        .expect("the program parses");

    let scribe = engine.sym("scribe");
    let atlas = engine.sym("atlas");
    let borealis = engine.sym("borealis");
    engine.declare("runs_in", &[scribe, atlas], Tally::base());
    engine.declare("hosted_by", &[atlas, borealis], Tally::base());
    engine.run();

    let fact = engine
        .fact("place_of", &[scribe, borealis])
        .expect("the rule fires");

    // `atoms: 2` is one() (0) times each body fact (1 each), so a `times` the
    // evaluator failed to call reads as a different number rather than as
    // silence.
    //
    // The two `derivations` entries are the load-bearing half. This is ONE
    // logical derivation, and `derive` is handed it TWICE, once per positive
    // body atom position, with the body keys in FIRING ORDER - the delta atom
    // first, the rest in body order. The evaluator does not sort them and
    // never has.
    //
    // Any carrier whose reading counts distinct derivations - which is every
    // reason `derive` exists - must sort the keys itself before fingerprinting
    // them, or one logical derivation is counted once per permutation of its
    // body and a mean over derivations becomes permutation-weighted. If a
    // future change sorts at the call site instead, this set collapses to one
    // entry and this assertion is what says so.
    assert_eq!(fact.ann.atoms, 2);
    assert_eq!(
        bodies(&fact.ann),
        BTreeSet::from([
            "hosted_by,runs_in".to_owned(),
            "runs_in,hosted_by".to_owned()
        ])
    );
    assert_eq!(
        clauses(&fact.ann).len(),
        1,
        "one clause derived this fact, however many times it fired"
    );
}

#[test]
fn two_unnamed_clauses_of_one_head_are_two_derivations() {
    // The shape the corpus actually has: no clause is named, and two clauses
    // of one head differ only in which predicate they negate. The engine
    // labels both `rule/safe`, and before the clause identity existed the
    // carrier could not tell them apart - one derivation where there are two,
    // which is the laundering direction under a mean.
    let mut engine: Engine<Tally> = Engine::default();
    engine
        .install_program(
            "safe(P) :- thing(P), !problem(P).\n\
             safe(P) :- thing(P), !broken(P).",
        )
        .expect("the program parses");

    let relay = engine.sym("relay");
    engine.declare("thing", &[relay], Tally::base());
    engine.declare("problem", &[relay], Tally::base());
    engine.run();

    let fact = engine
        .fact("safe", &[relay])
        .expect("both clauses derive safe(relay)");

    assert!(
        fact.ann.marks.contains("<blocked>"),
        "the clause negating problem(relay) fired too, so both routes are in play"
    );
    assert_eq!(
        bodies(&fact.ann),
        BTreeSet::from(["thing".to_owned()]),
        "a negated literal contributes no body key, by design"
    );
    assert_eq!(
        clauses(&fact.ann).len(),
        2,
        "two clauses, two derivations - the label `rule/safe` cannot say so"
    );
}

#[test]
fn the_trait_defaults_alone_reproduce_negation_as_absence() {
    let mut engine: Engine<Minimal> = Engine::default();
    engine
        .install_program("ok: safe(P) :- pkg(P), !banned(P).")
        .expect("the program parses");

    let alpha = engine.sym("alpha");
    let beta = engine.sym("beta");
    engine.declare("pkg", &[alpha], Minimal::one());
    engine.declare("pkg", &[beta], Minimal::one());
    engine.declare("banned", &[alpha], Minimal::one());
    engine.run();

    // Pruning is decided by `is_zero`, not by what `negate` returns, so a
    // carrier that takes every default it is offered still has to prune on a
    // blocked body. `Engine<Ann>` derives no `safe(alpha)` and neither may this.
    assert!(
        engine.fact("safe", &[alpha]).is_none(),
        "a banned package must not be safe"
    );
    assert_eq!(
        engine
            .fact("safe", &[beta])
            .expect("an unbanned package is safe")
            .ann,
        Minimal(1),
        "the rule fires at all, so the absence above is pruning and not a dead program"
    );
}

#[test]
fn an_aggregate_head_tells_the_annotation_which_aggregate_it_is_folding() {
    let mut engine: Engine<Seen> = Engine::default();
    engine
        .install_program(
            "tool_count(P, count(T)) :- has_tool(P, T).\n\
             tool_total(P, sum(N)) :- tool_size(P, N).",
        )
        .expect("the program parses");

    let scribe = engine.sym("scribe");
    for tool in ["socat", "rg"] {
        let tool = engine.sym(tool);
        engine.declare("has_tool", &[scribe, tool], Seen::one());
    }
    for size in [3, 4] {
        engine.declare("tool_size", &[scribe, Value::Int(size)], Seen::one());
    }
    engine.run();

    // Two heads, one group of two rows each, and the same evaluator call site.
    // The only thing that separates them is the head's own aggregate kind, so
    // a call site that passed a constant - or nothing - reads as a different
    // value here rather than as silence.
    assert_eq!(
        engine
            .fact("tool_count", &[scribe, Value::Int(2)])
            .expect("the count head derives")
            .ann,
        Seen::Aggregated {
            kinds: vec![AggFn::Count],
            rows: 2,
        }
    );
    assert_eq!(
        engine
            .fact("tool_total", &[scribe, Value::Int(7)])
            .expect("the sum head derives")
            .ann,
        Seen::Aggregated {
            kinds: vec![AggFn::Sum],
            rows: 2,
        }
    );
}

/// The distinct clause identities that stamped this annotation.
fn clauses(t: &Tally) -> BTreeSet<ClauseId> {
    t.derivations.iter().map(|(c, _)| *c).collect()
}

/// The body predicate lists, one per firing the evaluator stamped.
fn bodies(t: &Tally) -> BTreeSet<String> {
    t.derivations.iter().map(|(_, b)| b.clone()).collect()
}

impl Tally {
    /// One base fact, as the caller asserts it.
    fn base() -> Self {
        Tally {
            atoms: 1,
            marks: BTreeSet::new(),
            derivations: BTreeSet::new(),
        }
    }
}
