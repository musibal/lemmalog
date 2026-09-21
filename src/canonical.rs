//! Entity resolution by canonicalization: the LLM proposes star-shaped
//! `alias(Local, Canonical)` edges (confidence-tagged), Datalog derives the
//! symmetric-transitive closure, and canonical views project facts through
//! the mapping — read-side only, so retracting a bad alias rebuilds the
//! closure and every downstream view in the same epoch.
//!
//! Three safety properties (see the design discussion):
//! 1. **Star shape, not free-form equivalence**: each local name points at
//!    a declared canonical. Topology violations (a local with two
//!    canonicals; a name that is both local and canonical) derive
//!    `alias_conflict` facts instead of merging identities — surface them
//!    to the agent rather than silently unifying.
//! 2. **Read-side views**: raw facts are never rewritten; canonical
//!    projections are derived relations.
//! 3. **Confidence propagates**: alias edges carry semiring confidence, so
//!    a two-hop `same_as` carries the product of the path — weak merges
//!    are visibly low-confidence in queries and `why()` trees.

use crate::eval::{Ann, Engine};
use crate::intern::Value;

/// The canonicalization rule batch. `entity/1` seeds reflexivity; views
/// join on the DIRECTIONAL `maps_to` (local -> canonical, reflexive
/// fallback) so raw facts project to exactly one canonical spelling —
/// the symmetric `same_as` closure stays available for sameness queries.
pub const CANONICAL_RULES: &str = "\
same_as(X, X) :- entity(X).\n\
same_as(X, Y) :- alias(X, Y).\n\
same_as(X, Y) :- alias(Y, X).\n\
same_as(X, Z) :- same_as(X, Y), same_as(Y, Z).\n\
aliased(X) :- alias(X, _).\n\
maps_to(X, X) :- entity(X), !aliased(X).\n\
maps_to(L, C) :- alias(L, C).\n\
alias_conflict(L) :- alias(L, C1), alias(L, C2), C1 \\= C2.\n\
alias_conflict(N) :- alias(N, _), alias(_, N).\n";

impl Engine {
    /// Declare `entity(N)` for every symbol appearing in the given
    /// predicates' rows — the reflexive domain for `same_as`.
    pub fn seed_entities(&mut self, preds: &[&str]) -> usize {
        let mut names: Vec<String> = Vec::new();
        for p in preds {
            for key in self.relation_keys(p) {
                for v in &key {
                    if let Value::Sym(s) = v {
                        let n = self.interner.resolve(*s).to_string();
                        if !names.contains(&n) {
                            names.push(n);
                        }
                    }
                }
            }
        }
        let mut n = 0usize;
        for name in names {
            let sym = self.sym(&name);
            if self.declare("entity", &[sym], Ann::unit()) {
                n += 1;
            }
        }
        n
    }
}

/// Generate a canonical view rule for a relation: `{rel}_canon(...)` with
/// every symbol-typed position projected through `same_as`. Arity 3 is
/// assumed to be (subject, relation-name, object); arity 2 (subject,
/// object); other arities project every position.
///
/// The arity-3 middle position is kept verbatim: an alias proposed for an
/// entity must not silently rewrite a relation name. See
/// [`vocabulary_view_rule`] for the opt-in that does project it.
pub fn canonical_view_rule(rel: &str, arity: usize) -> String {
    let (raw_args, canon_args): (Vec<String>, Vec<String>) = (0..arity)
        .map(|i| (format!("A{i}"), format!("B{i}")))
        .unzip();
    let mut body = vec![format!("{rel}({})", raw_args.join(", "))];
    for i in 0..arity {
        // arity-3 middle position is the relation name: keep it verbatim
        let skip = arity == 3 && i == 1;
        if !skip {
            // directional: local -> canonical, with reflexive fallback
            body.push(format!("maps_to(A{i}, B{i})"));
        }
    }
    let head = if arity == 3 {
        format!("{rel}_canon({}, A1, {})", canon_args[0], canon_args[2])
    } else {
        format!("{rel}_canon({})", canon_args.join(", "))
    };
    format!("{head} :- {}.\n", body.join(", "))
}

/// Generate a view that projects the RELATION NAME of an arity-3 relation
/// through `maps_to`, collapsing vocabulary drift on the read side.
///
/// [`canonical_view_rule`] deliberately keeps that position verbatim, which
/// is the safe default: entity aliases and relation aliases are different
/// claims, and an alias meant for an entity must never rewrite a predicate.
/// But relation drift is usually the larger problem — one store held 3,227
/// relations used exactly once out of 4,476, every `estado_20260916` and
/// `estado_medido` a separate name for `estado` — and none of it is
/// reachable through the entity-only view.
///
/// Keep the two alias populations apart: seed relation names as entities
/// only when you intend this view, or an entity alias and a relation alias
/// will contend for one `maps_to` row.
///
/// Read-side like every other canonical view: raw facts are untouched and
/// retracting an alias rebuilds the projection in the same epoch.
pub fn vocabulary_view_rule(rel: &str) -> String {
    format!("{rel}_vocab(S, R2, O) :- {rel}(S, R1, O), maps_to(R1, R2).\n")
}

/// Assert one alias edge with confidence (star-shaped: local -> canonical).
pub fn assert_alias(e: &mut Engine, local: &str, canonical: &str, conf: f64) -> bool {
    let (l, c) = (e.sym(local), e.sym(canonical));
    e.declare("alias", &[l, c], Ann::base(conf, ["reconcile"]))
}

/// Install the canonicalization batch, seed the entity domain from the
/// given predicates, and install canonical views for them. Returns the
/// batch id for the rule install.
pub fn install_canonicalization(
    e: &mut Engine,
    preds: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let batch = e.install_program(CANONICAL_RULES)?;
    e.seed_entities(preds);
    let mut views = String::new();
    let mut arities: Vec<(String, usize)> = Vec::new();
    for p in preds {
        if let Some(rel) = e.relations.get(*p) {
            if let Some(a) = rel.rows.first().map(|r| r.key.len()) {
                arities.push((p.to_string(), a));
            }
        }
    }
    for (p, a) in &arities {
        views.push_str(&canonical_view_rule(p, *a));
    }
    if !views.is_empty() {
        e.install_program(&views)?;
    }
    Ok(batch)
}

/// Current conflicts (topology violations to surface, not merge).
pub fn alias_conflicts(e: &Engine) -> Vec<String> {
    e.relation_keys("alias_conflict")
        .into_iter()
        .map(|k| e.render_fact("alias_conflict", &k))
        .collect()
}

#[cfg(feature = "llm")]
pub mod reconcile {
    use super::*;
    use crate::llm::HttpEmbedder;
    use crate::llm::OpenAiClient;
    use crate::semantics::Embedder;

    pub const RECONCILE_PROMPT: &str = "\
You reconcile entity names in a knowledge graph. You are given candidate \
pairs of names that MIGHT refer to the same real entity. Two names are \
aliases ONLY IF they refer to ONE SPECIFIC, INDIVIDUAL thing — the same \
one person, the same one object, the same one place, the same one event. \
For each pair you are confident refers to that same individual, output one \
line:\n\
local --alias_of[CONFIDENCE]--> canonical\n\
CONFIDENCE in [0,1]. Choose the fuller, cleaner name as the canonical.\n\
NOT aliases — skip these every time:\n\
- different members of a category: 'horse painting' and 'abstract \
painting' are two different paintings, not one.\n\
- a category and a member: 'painting' vs 'watercolor of a horse'.\n\
- phrases carrying time or events: 'adopted last year' is an event, not \
an entity name.\n\
- topics that merely relate: 'friends and family' vs 'friends'.\n\
Short for a full name IS an alias ('Mel' = 'Melanie'); so is a nickname \
or description of ONE thing ('my car' = 'Honda Civic' when both name the \
one car). When unsure, SKIP the pair. Output only the lines.";

    /// One reconciliation pass: collect unique entity names, gate
    /// candidate pairs by embedding similarity when an embedder base is
    /// given (otherwise offer all pairs up to a cap), ask the model, and
    /// assert confidence-tagged alias edges. Returns the asserted aliases.
    pub fn reconcile_entities(
        e: &mut Engine,
        chat: &OpenAiClient,
        embed_base: Option<&str>,
        preds: &[&str],
    ) -> Result<Vec<(String, String, f64)>, String> {
        // collect unique entity names
        let mut names: Vec<String> = Vec::new();
        for p in preds {
            for key in e.relation_keys(p) {
                for v in &key {
                    if let Value::Sym(s) = v {
                        let n = e.interner.resolve(*s).to_string();
                        if !names.contains(&n) {
                            names.push(n);
                        }
                    }
                }
            }
        }
        if names.len() < 2 {
            return Ok(Vec::new());
        }
        // candidate pairs: similarity-gated when possible
        let pairs: Vec<(String, String)> = match embed_base {
            Some(base) => {
                let embedder = HttpEmbedder::new(base, "text-embedding-nomic-embed-text-v1.5");
                let mut gated = Vec::new();
                for i in 0..names.len() {
                    for j in i + 1..names.len() {
                        let a = embedder.embed(&names[i]);
                        let b = embedder.embed(&names[j]);
                        let cos = crate::semantics::cosine_pub(&a, &b);
                        if cos > 0.72 {
                            gated.push((names[i].clone(), names[j].clone()));
                        }
                    }
                }
                gated
            }
            None => {
                let mut all = Vec::new();
                for i in 0..names.len() {
                    for j in i + 1..names.len() {
                        all.push((names[i].clone(), names[j].clone()));
                    }
                }
                all.truncate(60);
                all
            }
        };
        if pairs.is_empty() {
            return Ok(Vec::new());
        }
        let listing = pairs
            .iter()
            .map(|(a, b)| format!("- {a} | {b}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = chat
            .chat(
                RECONCILE_PROMPT,
                &format!("Candidate pairs:\n{listing}\n\nLines only:"),
            )
            .map_err(|e| e.to_string())?;
        let mut asserted = Vec::new();
        for f in crate::agent::parse_protocol_strict(&out, 0.8) {
            if f.pred == "alias_of" {
                assert_alias(e, &f.subj, &f.obj, f.confidence);
                asserted.push((f.subj, f.obj, f.confidence));
            }
        }
        Ok(asserted)
    }
}

/// Entity alignment through a System One model: the three things you can do
/// with a candidate pair ARE the three levels of one `Score` question, so
/// the routing falls out of the level wording instead of a threshold fitted
/// to your data.
///
/// This is the same job as [`reconcile`], done without generating text. The
/// difference that matters is the number: `reconcile` asks a chat model to
/// write a confidence into a line of prose and parses it back (defaulting to
/// 0.8 when the model omits it), whereas a `Score` answer carries a
/// calibrated confidence derived from the probability distribution. The
/// engine multiplies these down proof chains, so an invented 0.8 and a
/// measured 0.41 are not interchangeable.
///
/// The other difference: `reconcile` can only emit an alias or skip. A pair
/// that is neither safe to merge nor safe to drop has nowhere to go. Here it
/// lands in [`Route::Curator`], which is the outcome most candidate pairs
/// actually deserve.
#[cfg(feature = "jev")]
pub mod jev_align {
    use super::*;
    use crate::jev::{Answer, JevClient, Question};

    /// The three levels, in order. Level 0 is "leave alone", level 2 is
    /// "merge". Wording these is the whole configuration surface: change
    /// the middle level and pairs move between the curator and the floor.
    pub const LEVELS: [&str; 3] = [
        "different relations: they describe different facts and must not be unified",
        "related but possibly not the same: a human should decide",
        "the same relation written two ways: they should be unified with alias_of",
    ];

    /// Cut points for rounding a score to an outcome. A 3-level score runs
    /// 0..2, so the boundaries sit halfway between levels. Not tuned.
    const LOWER: f64 = 0.5;
    const UPPER: f64 = 1.5;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Route {
        /// Safe to merge: assert `alias_of`.
        Merge,
        /// Neither safe to merge nor safe to drop — hand it to a person.
        Curator,
        /// Leave the two names unlinked.
        Leave,
    }

    /// One pair's verdict, with the companion signals a curator needs to see
    /// why it landed where it did.
    #[derive(Debug, Clone)]
    pub struct Verdict {
        pub a: String,
        pub b: String,
        pub score: f64,
        /// Calibrated: low means the model is telling you it does not know.
        pub confidence: f64,
        /// Companion noul: same subsystem or technical domain.
        pub same_domain: f64,
        /// Companion noul: the difference is only writing noise (a date
        /// glued to the name, a preposition, a redundant suffix).
        pub noise_only: f64,
        pub route: Route,
    }

    impl Verdict {
        /// Canonical is the shorter name: a date or a redundant suffix glued
        /// on is what makes the other one longer.
        pub fn canonical(&self) -> (&str, &str) {
            if self.a.len() <= self.b.len() {
                (&self.b, &self.a)
            } else {
                (&self.a, &self.b)
            }
        }
    }

    /// (asserted aliases as `(local, canonical, confidence)`, curator queue,
    /// failures as `(a, b, reason)`).
    pub type Reconciled = (Vec<(String, String, f64)>, Vec<Verdict>, Vec<(String, String, String)>);

    fn route_of(score: f64) -> Route {
        if score >= UPPER {
            Route::Merge
        } else if score >= LOWER {
            Route::Curator
        } else {
            Route::Leave
        }
    }

    /// Judge one candidate pair. One call per pair, on purpose: each pair is
    /// its own state, and batching independent states into one request
    /// measurably moves the scores (up to 0.73 on relation names) because
    /// the other pairs act as distractors.
    pub fn align_pair(
        client: &mut JevClient,
        domain: &str,
        a: &str,
        b: &str,
    ) -> Result<Verdict, String> {
        let state = serde_json::json!({ "rel_a": a, "rel_b": b });
        let questions = vec![
            (
                "align",
                Question::Score {
                    instructions: format!(
                        "`rel_a` and `rel_b` are RELATION NAMES from a knowledge graph about \
                         {domain}. A relation connects a subject to an object. Decide whether \
                         `rel_a` and `rel_b` name the SAME semantic relation written two \
                         different ways, or different relations. A suffix that adds a date, a \
                         status, or a temporal nuance does NOT make them different when the \
                         underlying relation is the same."
                    ),
                    criteria: LEVELS.iter().map(|s| s.to_string()).collect(),
                },
            ),
            (
                "same_domain",
                Question::Noul {
                    instructions: "`rel_a` and `rel_b` refer to the same subsystem or technical \
                                   domain."
                        .to_string(),
                },
            ),
            (
                "noise_only",
                Question::Noul {
                    instructions: "The only difference between `rel_a` and `rel_b` is writing \
                                   noise: prepositions, articles, a date glued to the name, or a \
                                   redundant suffix. The underlying meaning is the same."
                        .to_string(),
                },
            ),
        ];
        let resp = client.ask(&state, &questions)?;
        let (score, confidence) = match resp.answers.get("align") {
            Some(Answer::Score {
                score, confidence, ..
            }) => (*score, *confidence),
            _ => return Err(format!("no score answer for {a} | {b}")),
        };
        let noul = |id: &str| -> Result<f64, String> {
            match resp.answers.get(id) {
                Some(Answer::Noul { noul }) if noul.is_finite() => Ok(*noul),
                Some(_) => Err(format!("invalid {id} answer for {a} | {b}")),
                None => Err(format!("missing {id} answer for {a} | {b}")),
            }
        };
        let same_domain = noul("same_domain")?;
        let noise_only = noul("noise_only")?;
        Ok(Verdict {
            a: a.to_string(),
            b: b.to_string(),
            score,
            confidence,
            same_domain,
            noise_only,
            route: route_of(score),
        })
    }

    /// Candidate pairs for [`reconcile_with_jev`], gated by token overlap.
    ///
    /// Why this is here and not the caller's problem: one jev call per pair
    /// is mandatory (batching independent states moves scores by up to
    /// 0.73), so the pair count IS the bill. All-pairs over a real
    /// vocabulary of 2,285 relation names is 2,609,470 calls; at
    /// `min_jaccard` 0.5 it is 653.
    ///
    /// This is a RECALL gate, not a decision. It over-proposes on purpose —
    /// `paso_5` and `paso_6` both reduce to `{paso}` and score 1.0 here —
    /// because rejecting those is precisely what jev is for. Lexical
    /// similarity and jev's verdict correlate at 0.16 on real pairs: the
    /// cheap half proposes, the calibrated half decides.
    pub fn candidate_pairs(names: &[String], min_jaccard: f64) -> Vec<(String, String)> {
        // Deduplicate first: a repeated name would otherwise pair with
        // itself (jaccard 1.0, so it always clears the gate), buying a paid
        // call to ask whether `estado` is `estado` and offering a reflexive
        // alias for assertion.
        let mut seen = std::collections::BTreeSet::new();
        let names: Vec<&String> = names.iter().filter(|n| seen.insert(n.as_str())).collect();
        let toks: Vec<_> = names.iter().map(|n| crate::retrieval::tokens3(n)).collect();
        let mut out = Vec::new();
        for i in 0..names.len() {
            for j in i + 1..names.len() {
                let (a, b) = (&toks[i], &toks[j]);
                let inter = a.intersection(b).count();
                if inter == 0 {
                    continue;
                }
                let union = a.len() + b.len() - inter;
                if union > 0 && inter as f64 / union as f64 >= min_jaccard {
                    out.push((names[i].clone(), names[j].clone()));
                }
            }
        }
        out
    }

    /// Judge every candidate pair and assert the ones that clear both the
    /// merge cut point and `min_confidence`.
    ///
    /// Merging wrongly is the expensive mistake — every fact about either
    /// name now describes the merged one — so a high score the model is not
    /// confident about is demoted to the curator rather than asserted.
    /// Returns (asserted aliases, curator queue, failed pairs) — see
    /// [`Reconciled`].
    ///
    /// A failing pair does NOT abort the batch. Aborting used to discard the
    /// report of aliases this call had already asserted into `e`, which is
    /// the one thing the caller cannot reconstruct: the engine kept the
    /// edges and the caller got only an error string. Batches here run to
    /// hundreds of calls over minutes, so a transport blip mid-run is
    /// ordinary. Every failure is returned with its reason; an empty
    /// `asserted` next to a full failure list is the signature of a bad API
    /// key, and is now visible instead of looking like "nothing to merge".
    ///
    /// Alias edges are read-side and reversible: retracting one rebuilds the
    /// closure and every canonical view in the same epoch.
    pub fn reconcile_with_jev(
        e: &mut Engine,
        client: &mut JevClient,
        domain: &str,
        pairs: &[(String, String)],
        min_confidence: f64,
    ) -> Result<Reconciled, String> {
        let mut asserted = Vec::new();
        let mut curator = Vec::new();
        let mut failed = Vec::new();
        for (a, b) in pairs {
            let v = match align_pair(client, domain, a, b) {
                Ok(v) => v,
                Err(err) => {
                    failed.push((a.clone(), b.clone(), err));
                    continue;
                }
            };
            match v.route {
                Route::Merge if v.confidence >= min_confidence => {
                    let (local, canonical) = v.canonical();
                    assert_alias(e, local, canonical, v.confidence);
                    asserted.push((local.to_string(), canonical.to_string(), v.confidence));
                }
                // high score, low confidence: the model is unsure, so a
                // person looks rather than the graph getting merged
                Route::Merge | Route::Curator => curator.push(v),
                Route::Leave => {}
            }
        }
        Ok((asserted, curator, failed))
    }
}
