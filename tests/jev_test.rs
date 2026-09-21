//! `jev` client contract and alignment routing, on canned transports: the
//! fixtures are verbatim responses captured from `api.typesafe.ai`, so a
//! wire-format change breaks these tests rather than production.

use lemmalog::jev::{parse_response, Answer, JevClient, Question};

/// Verbatim response to a Score + Noul request (captured 2026-09-19,
/// model jev-1.13.0).
const REAL_SCORE_NOUL: &str = r#"{
 "model": "jev-1.13.0",
 "answers": {
  "align": {"type":"score","score":0.88,"confidence":0.0,
            "legend":{"0":"distintas","1":"quiza","2":"la misma"},
            "probabilities":{"0":0.44,"1":0.23,"2":0.33}},
  "ruido": {"type":"noul","noul":0.87}
 },
 "usage": {"input_tokens": 343, "output_tokens": 34}
}"#;

#[test]
fn parses_the_real_wire_form() {
    let r = parse_response(REAL_SCORE_NOUL).expect("parses");
    assert_eq!(r.model, "jev-1.13.0");
    assert_eq!(r.usage.input_tokens, 343);
    assert_eq!(r.usage.output_tokens, 34);
    match r.answers.get("align").expect("align present") {
        Answer::Score {
            score,
            confidence,
            probabilities,
        } => {
            assert!((score - 0.88).abs() < 1e-9);
            assert_eq!(*confidence, 0.0);
            // keyed by integer level, ordered
            assert_eq!(probabilities[0].0, 0);
            assert_eq!(probabilities[2].0, 2);
            let mass: f64 = probabilities.iter().map(|(_, p)| p).sum();
            assert!((mass - 1.0).abs() < 0.02, "distribution should sum to ~1");
        }
        other => panic!("expected a score, got {other:?}"),
    }
    match r.answers.get("ruido").expect("ruido present") {
        Answer::Noul { noul } => assert!((noul - 0.87).abs() < 1e-9),
        other => panic!("expected a noul, got {other:?}"),
    }
}

#[test]
fn a_noul_reports_no_confidence_by_design() {
    let r = parse_response(REAL_SCORE_NOUL).unwrap();
    assert_eq!(r.answers["ruido"].confidence(), None);
    assert_eq!(r.answers["align"].confidence(), Some(0.0));
}

#[test]
fn an_unknown_answer_kind_is_skipped_not_fatal() {
    let raw = r#"{"answers":{"a":{"type":"noul","noul":0.5},
                             "b":{"type":"something_new","value":1}},
                  "usage":{"input_tokens":1,"output_tokens":1}}"#;
    let r = parse_response(raw).expect("still parses");
    assert_eq!(r.answers.len(), 1, "the known answer survives");
    assert!(r.answers.contains_key("a"));
}

#[test]
fn a_malformed_response_is_an_error_not_a_default() {
    assert!(parse_response("not json").is_err());
    assert!(parse_response(r#"{"usage":{}}"#).is_err(), "no answers key");
    // a score missing its confidence must not silently become 0.0
    let missing = r#"{"answers":{"a":{"type":"score","score":1.0}},"usage":{}}"#;
    assert!(parse_response(missing).is_err());
}

#[test]
fn rejects_out_of_range_answers() {
    assert!(parse_response(
        r#"{"answers":{"a":{"type":"noul","noul":2.0}}}"#
    )
    .is_err());
    assert!(parse_response(
        r#"{"answers":{"a":{"type":"score","score":1.0,"confidence":1.1,"probabilities":{"0":1.0}}}}"#
    )
    .is_err());
    assert!(parse_response(
        r#"{"answers":{"a":{"type":"score","score":1.0,"confidence":0.8,"probabilities":{"0":-0.1}}}}"#
    )
    .is_err());
    assert!(parse_response(
        r#"{"answers":{"a":{"type":"score","score":1.0,"confidence":0.8}}}"#
    )
    .is_err());
}

#[test]
fn the_request_carries_state_model_and_typed_questions() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let sink = seen.clone();
    let mut c = JevClient::with_transport(
        Box::new(move |body: &str| {
            *sink.lock().unwrap() = body.to_string();
            Ok(REAL_SCORE_NOUL.to_string())
        }),
        "jev-latest",
    );
    let state = serde_json::json!({"rel_a": "estado_20260916", "rel_b": "estado"});
    c.ask(
        &state,
        &[
            (
                "align",
                Question::Score {
                    instructions: "same?".into(),
                    criteria: vec!["no".into(), "maybe".into(), "yes".into()],
                },
            ),
            (
                "ruido",
                Question::Noul {
                    instructions: "only a date?".into(),
                },
            ),
        ],
    )
    .expect("ask succeeds");

    let sent: serde_json::Value = serde_json::from_str(&seen.lock().unwrap()).unwrap();
    assert_eq!(sent["model"], "jev-latest");
    assert_eq!(sent["state"]["rel_a"], "estado_20260916");
    assert_eq!(sent["questions"]["align"]["type"], "score");
    // Score criteria go on the wire as an ORDERED LIST; a map is the Choice
    // shape and the API rejects it.
    assert!(sent["questions"]["align"]["criteria"].is_array());
    assert_eq!(sent["questions"]["align"]["criteria"][2], "yes");
    assert_eq!(sent["questions"]["ruido"]["type"], "noul");
    assert!(sent["questions"]["ruido"].get("criteria").is_none());
    assert_eq!(c.calls, 1);
    assert_eq!(c.failures, 0);
}

#[test]
fn a_transport_failure_is_counted() {
    let mut c = JevClient::with_transport(Box::new(|_| Err("http 500".into())), "jev-latest");
    assert!(c.ask(&serde_json::json!({}), &[]).is_err());
    assert_eq!(c.failures, 1);
}

mod alignment {
    use super::*;
    use lemmalog::canonical::jev_align::{align_pair, reconcile_with_jev, Route};
    use lemmalog::eval::Engine;

    fn canned(score: f64, confidence: f64) -> JevClient {
        let body = format!(
            r#"{{"model":"jev-1.13.0","answers":{{
                 "align":{{"type":"score","score":{score},"confidence":{confidence},
                           "legend":{{"0":"a","1":"b","2":"c"}},
                           "probabilities":{{"0":0.3,"1":0.3,"2":0.4}}}},
                 "same_domain":{{"type":"noul","noul":0.85}},
                 "noise_only":{{"type":"noul","noul":0.1}}}},
               "usage":{{"input_tokens":1,"output_tokens":1}}}}"#
        );
        JevClient::with_transport(Box::new(move |_| Ok(body.clone())), "jev-latest")
    }

    #[test]
    fn the_cut_points_follow_from_three_levels() {
        for (score, want) in [
            (0.01, Route::Leave),
            (0.49, Route::Leave),
            (0.50, Route::Curator),
            (1.20, Route::Curator),
            (1.49, Route::Curator),
            (1.50, Route::Merge),
            (1.96, Route::Merge),
        ] {
            let mut c = canned(score, 0.9);
            let v = align_pair(&mut c, "software infrastructure", "a", "bb").unwrap();
            assert_eq!(v.route, want, "score {score} should route to {want:?}");
        }
    }

    #[test]
    fn canonical_is_the_shorter_name() {
        let mut c = canned(1.9, 0.9);
        let v = align_pair(&mut c, "d", "estado_20260916", "estado").unwrap();
        let (local, canonical) = v.canonical();
        assert_eq!(canonical, "estado", "the date-suffixed name is the local one");
        assert_eq!(local, "estado_20260916");
    }

    #[test]
    fn companion_nouls_ride_along_for_the_curator() {
        let mut c = canned(1.2, 0.2);
        let v = align_pair(&mut c, "d", "a", "bb").unwrap();
        assert!((v.same_domain - 0.85).abs() < 1e-9);
        assert!((v.noise_only - 0.1).abs() < 1e-9);
    }

    #[test]
    fn missing_companion_noul_is_an_error_not_nan() {
        for (missing, body) in [
            (
                "same_domain",
                r#"{"answers":{"align":{"type":"score","score":1.9,"confidence":0.9,"probabilities":{"0":0.0,"1":0.0,"2":1.0}}}}"#,
            ),
            (
                "noise_only",
                r#"{"answers":{"align":{"type":"score","score":1.9,"confidence":0.9,"probabilities":{"0":0.0,"1":0.0,"2":1.0}},"same_domain":{"type":"noul","noul":0.9}}}"#,
            ),
        ] {
            let mut c = JevClient::with_transport(
                Box::new(move |_| Ok(body.to_string())),
                "jev-latest",
            );
            let err = align_pair(&mut c, "d", "a", "bb").unwrap_err();
            assert!(err.contains(missing), "{err}");
        }
    }

    #[test]
    fn incomplete_companion_is_reported_without_alias_mutation() {
        let body = r#"{"answers":{"align":{"type":"score","score":1.9,"confidence":0.9,"probabilities":{"0":0.0,"1":0.0,"2":1.0}}}}"#;
        let mut c = JevClient::with_transport(
            Box::new(move |_| Ok(body.to_string())),
            "jev-latest",
        );
        let mut e = Engine::new();
        let pairs = vec![("estado_a".to_string(), "estado".to_string())];
        let (asserted, curator, failed) =
            reconcile_with_jev(&mut e, &mut c, "d", &pairs, 0.30).unwrap();
        assert!(asserted.is_empty());
        assert!(curator.is_empty());
        assert_eq!(failed.len(), 1);
        assert!(failed[0].2.contains("same_domain"), "{failed:?}");
        assert!(e.query("alias", &[None, None]).is_empty());
    }
    /// The property that protects the graph: merging wrongly is the
    /// expensive mistake, so a high score the model is NOT confident about
    /// goes to a person instead of being asserted.
    #[test]
    fn a_high_score_with_low_confidence_is_not_merged() {
        let mut e = Engine::new();
        let mut c = canned(1.9, 0.05);
        let pairs = vec![("estado_20260916".to_string(), "estado".to_string())];
        let (asserted, curator, _) =
            reconcile_with_jev(&mut e, &mut c, "d", &pairs, 0.30).unwrap();
        assert!(asserted.is_empty(), "nothing merged on 0.05 confidence");
        assert_eq!(curator.len(), 1, "it went to the curator instead");

        // same score, confidence over the bar: now it merges
        let mut e2 = Engine::new();
        let mut c2 = canned(1.9, 0.94);
        let (asserted2, curator2, _) =
            reconcile_with_jev(&mut e2, &mut c2, "d", &pairs, 0.30).unwrap();
        assert_eq!(asserted2.len(), 1);
        assert!(curator2.is_empty());
        let (local, canonical, conf) = &asserted2[0];
        assert_eq!(local, "estado_20260916");
        assert_eq!(canonical, "estado");
        assert!((conf - 0.94).abs() < 1e-9, "the CALIBRATED number is kept");
    }

    /// The gate has to over-propose: `paso_5` and `paso_6` are one token
    /// after stemming, so lexical similarity CANNOT separate them. Letting
    /// them through to jev is the design; deciding them here would merge
    /// two different steps.
    #[test]
    fn the_gate_proposes_the_traps_rather_than_deciding_them() {
        use lemmalog::canonical::jev_align::candidate_pairs;
        let names: Vec<String> = ["paso_5", "paso_6", "estado_20260916", "estado", "wrangler"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let pairs = candidate_pairs(&names, 0.5);
        let has = |a: &str, b: &str| {
            pairs
                .iter()
                .any(|(x, y)| (x == a && y == b) || (x == b && y == a))
        };
        assert!(has("estado_20260916", "estado"), "the real merge must survive the gate");
        assert!(has("paso_5", "paso_6"), "the trap is proposed, not decided");
        assert!(!has("wrangler", "estado"), "unrelated names share no token");
    }

    /// Found by the audit: a repeated name pairs with itself at jaccard 1.0,
    /// which buys a paid call to ask whether `estado` is `estado`.
    #[test]
    fn a_repeated_name_never_pairs_with_itself() {
        use lemmalog::canonical::jev_align::candidate_pairs;
        let names: Vec<String> = ["estado", "estado", "estado_fase0", "wrangler"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let p = candidate_pairs(&names, 0.5);
        assert!(p.iter().all(|(a, b)| a != b), "self-pair emitted: {p:?}");
        let dups = p.iter().filter(|(a, b)| {
            p.iter().filter(|(x, y)| (x == a && y == b) || (x == b && y == a)).count() > 1
        }).count();
        assert_eq!(dups, 0, "duplicate pair emitted: {p:?}");
        assert!(p.iter().any(|(a, b)| a == "estado" && b == "estado_fase0"));
    }

    /// The gate is the bill: a higher threshold must never cost more calls.
    #[test]
    fn a_higher_threshold_never_proposes_more_pairs() {
        use lemmalog::canonical::jev_align::candidate_pairs;
        let names: Vec<String> = [
            "estado", "estado_20260916", "estado_fase0", "paso_5", "paso_6", "branch",
            "branch_intentada", "branch_mergitada", "wrangler", "deploy_worker",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut prev = usize::MAX;
        for thr in [0.2, 0.3, 0.4, 0.5, 0.6, 0.9] {
            let n = candidate_pairs(&names, thr).len();
            assert!(n <= prev, "threshold {thr} proposed {n} > {prev}");
            prev = n;
        }
        assert_eq!(candidate_pairs(&names, 1.01).len(), 0, "nothing clears an impossible bar");
    }

    /// Found by the audit: a batch used to abort on the first transport
    /// failure, discarding the report of what it had ALREADY asserted into
    /// the engine. The edges survived; the caller got only an error.
    #[test]
    fn a_failure_mid_batch_keeps_the_report_of_what_was_asserted() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let n = std::sync::Arc::new(AtomicUsize::new(0));
        let body = r#"{"model":"jev-1.13.0","answers":{
             "align":{"type":"score","score":1.9,"confidence":0.94,
                      "legend":{"0":"a","1":"b","2":"c"},
                      "probabilities":{"0":0.0,"1":0.0,"2":1.0}},
             "same_domain":{"type":"noul","noul":0.9},
             "noise_only":{"type":"noul","noul":0.9}},
           "usage":{"input_tokens":1,"output_tokens":1}}"#;
        let mut c = JevClient::with_transport(
            Box::new(move |_| {
                // second pair blows up, the others succeed
                if n.fetch_add(1, Ordering::SeqCst) == 1 {
                    Err("http 503".into())
                } else {
                    Ok(body.to_string())
                }
            }),
            "jev-latest",
        );
        let mut e = Engine::new();
        let pairs: Vec<(String, String)> = [("estado_a", "estado"), ("boom_b", "boom"), ("modo_c", "modo")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let (asserted, _curator, failed) =
            reconcile_with_jev(&mut e, &mut c, "d", &pairs, 0.30).unwrap();
        assert_eq!(asserted.len(), 2, "the batch ran to the end: {asserted:?}");
        assert_eq!(failed.len(), 1, "the failure is reported, not swallowed");
        assert_eq!(failed[0].0, "boom_b");
        assert!(failed[0].2.contains("503"), "the reason survives: {}", failed[0].2);
    }

    /// The trap this replaces cosine+prose for: two names one edit apart
    /// that mean opposite things. Real score from the 1,469-pair sweep.
    #[test]
    fn the_lexical_trap_is_not_merged() {
        let mut e = Engine::new();
        let mut c = canned(0.88, 0.0);
        let pairs = vec![(
            "branch_intentada_luna_filtro_tipo_backfill".to_string(),
            "branch_mergitada_luna_filtro_tipo_backfill".to_string(),
        )];
        let (asserted, curator, _) =
            reconcile_with_jev(&mut e, &mut c, "d", &pairs, 0.30).unwrap();
        assert!(
            asserted.is_empty(),
            "intentada and mergitada are opposite lifecycle states"
        );
        assert_eq!(curator.len(), 1);
    }
}

mod vocabulary_view {
    use lemmalog::canonical::{canonical_view_rule, vocabulary_view_rule};

    /// The gap this closes: the entity view keeps the relation name
    /// verbatim, so relation drift is unreachable through it.
    #[test]
    fn the_entity_view_does_not_touch_the_relation_slot() {
        let r = canonical_view_rule("current", 3);
        assert!(r.contains("current_canon(B0, A1, B2)"), "got: {r}");
        assert!(r.contains("maps_to(A0, B0)"));
        assert!(r.contains("maps_to(A2, B2)"));
        assert!(
            !r.contains("maps_to(A1"),
            "the relation slot must stay verbatim here: {r}"
        );
    }

    #[test]
    fn the_vocabulary_view_projects_the_relation_slot() {
        let r = vocabulary_view_rule("current");
        assert!(r.contains("current_vocab(S, R2, O)"), "got: {r}");
        assert!(r.contains("maps_to(R1, R2)"), "got: {r}");
        // subject and object pass through: this view is only about the name
        assert!(r.contains("current(S, R1, O)"), "got: {r}");
    }
}
