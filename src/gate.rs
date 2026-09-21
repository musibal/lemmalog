//! Unified evidence gate for the Lemmalog MCP.
//!
//! This module deliberately has no transport.  It resolves evidence directly
//! against the in-process engine, asks JEV for the bounded blind review, and
//! persists only gate sessions and calibration records beside Lemmalog data.

use crate::eval::{answer_text, Engine};
use crate::jev::{Answer, JevClient, Question};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_GATE: AtomicU64 = AtomicU64::new(1);
const RUBRIC: [(&str, &str); 4] = [
    ("scope", "States what is in and out."),
    ("evidence", "Claims cite evidence or an investigation."),
    ("acceptance", "Success has observable checks."),
    (
        "safety",
        "Irreversible actions have a human/deterministic gate.",
    ),
];

fn base_dir() -> PathBuf {
    std::env::var("LEMMALOG_GATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".lemmalog/gates")
        })
}
fn calibration_path() -> PathBuf {
    std::env::var("LEMMALOG_GATE_CALIBRATION")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".lemmalog/gate-calibration.jsonl")
        })
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn session_path(id: &str) -> Result<PathBuf, String> {
    if !valid_id(id) {
        return Err("invalid gate_id".to_string());
    }
    Ok(base_dir().join(format!("{id}.json")))
}
fn write_atomic(path: &std::path::Path, bytes: &[u8], what: &str) -> Result<(), String> {
    fs::create_dir_all(path.parent().unwrap())
        .map_err(|e| format!("{what} directory: {e}"))?;
    // Two daemons can share one gate directory (`~/.lemmalog/gates` is shared by
    // both harnesses), so the scratch name must not be predictable: a fixed
    // `X.tmp` lets one writer rename the other's file away mid-write.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp = path.with_extension(format!("{}-{nonce}.tmp", std::process::id()));
    fs::write(&temp, bytes).map_err(|e| format!("write {what}: {e}"))?;
    fs::rename(&temp, path).map_err(|e| format!("commit {what}: {e}"))
}
fn write_session(state: &Value) -> Result<(), String> {
    let id = state
        .get("gate_id")
        .and_then(Value::as_str)
        .ok_or("gate session has no gate_id")?;
    let path = session_path(id)?;
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    write_atomic(&path, &bytes, "gate session")
}
/// Gate sessions and calibration are files on the container's disk, and Cloudflare
/// Containers have ephemeral disk: the Durable Object checkpoint is the only durable
/// store, so its blob has to carry these files as well as the engine.
pub fn export_state() -> Result<Value, String> {
    export_state_at(&base_dir(), &calibration_path())
}
pub fn restore_state(state: &Value) -> Result<(), String> {
    restore_state_at(state, &base_dir(), &calibration_path())
}
fn export_state_at(dir: &std::path::Path, calibration: &std::path::Path) -> Result<Value, String> {
    let mut sessions = serde_json::Map::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".json"))
            else {
                continue;
            };
            if !valid_id(id) {
                continue;
            }
            if let Ok(raw) = fs::read_to_string(&path) {
                sessions.insert(id.to_string(), Value::String(raw));
            }
        }
    }
    Ok(json!({
        "sessions": sessions,
        "calibration": fs::read_to_string(calibration).ok(),
    }))
}
fn restore_state_at(
    state: &Value,
    dir: &std::path::Path,
    calibration: &std::path::Path,
) -> Result<(), String> {
    if let Some(sessions) = state.get("sessions").and_then(Value::as_object) {
        for (id, raw) in sessions {
            let Some(text) = raw.as_str() else {
                continue;
            };
            if !valid_id(id) {
                return Err("invalid gate_id in snapshot".to_string());
            }
            write_atomic(&dir.join(format!("{id}.json")), text.as_bytes(), "gate session")?;
        }
    }
    if let Some(text) = state.get("calibration").and_then(Value::as_str) {
        if !text.is_empty() {
            write_atomic(calibration, text.as_bytes(), "gate calibration")?;
        }
    }
    Ok(())
}
fn read_session(id: &str) -> Result<Value, String> {
    let path = session_path(id)?;
    let raw = fs::read_to_string(path).map_err(|_| format!("unknown gate_id: {id}"))?;
    serde_json::from_str(&raw).map_err(|_| format!("invalid session state for gate_id: {id}"))
}
fn new_id() -> String {
    format!(
        "gate-{:x}-{:x}",
        now(),
        NEXT_GATE.fetch_add(1, Ordering::Relaxed)
    )
}
fn refs(args: &Value) -> Result<Vec<String>, String> {
    match args.get("evidence_refs") {
        None => Ok(vec![]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "evidence_refs must be an array of strings".to_string())
            })
            .collect(),
        _ => Err("evidence_refs must be an array of strings".to_string()),
    }
}
fn parse_ref(reference: &str) -> Result<(String, String, Option<String>), String> {
    let parts: Vec<&str> = reference.split('|').map(str::trim).collect();
    if parts.len() != 3 || parts[0].is_empty() || parts[1].is_empty() {
        return Err("ref must be subject|relation|object".to_string());
    }
    Ok((
        parts[0].to_string(),
        parts[1].to_string(),
        (!parts[2].is_empty()).then(|| parts[2].to_string()),
    ))
}
fn literal(value: &str) -> String {
    serde_json::to_string(value).unwrap()
}
fn resolve_one(engine: &mut Engine, reference: &str) -> Value {
    let queried_at = now();
    let (subject, relation, object) = match parse_ref(reference) {
        Ok(parts) => parts,
        Err(reason) => {
            return json!({"ref":reference,"subject":null,"relation":null,"object":null,"status":"unresolved","rows":[],"goal":null,"queried_at":queried_at,"reason":reason})
        }
    };
    let goal = match &object {
        Some(object) => format!(
            "current({}, {}, {})",
            literal(&subject),
            literal(&relation),
            literal(object)
        ),
        None => format!(
            "current({}, {}, Obj)",
            literal(&subject),
            literal(&relation)
        ),
    };
    let result = engine
        .ask(&goal)
        .map_err(|e| e.to_string())
        .and_then(|rows| answer_text(&rows).ok_or_else(|| "no matching evidence row".to_string()));
    match result {
        Ok(text) => {
            json!({"ref":reference,"subject":subject,"relation":relation,"object":object,"status":"resolved","rows":text.lines().collect::<Vec<_>>(),"goal":goal,"queried_at":queried_at,"reason":"matching evidence row"})
        }
        Err(reason) => {
            json!({"ref":reference,"subject":subject,"relation":relation,"object":object,"status":"unresolved","rows":[],"goal":goal,"queried_at":queried_at,"reason":reason})
        }
    }
}
fn resolve_all(engine: &mut Engine, references: &[String]) -> Value {
    let records: Vec<Value> = references.iter().map(|r| resolve_one(engine, r)).collect();
    let mut grouped = HashMap::<&str, Vec<Value>>::new();
    for record in &records {
        grouped
            .entry(record["status"].as_str().unwrap_or("unresolved"))
            .or_default()
            .push(record.clone());
    }
    json!({"endpoint_ok":true,"resolved":grouped.remove("resolved").unwrap_or_default(),"unresolved":grouped.remove("unresolved").unwrap_or_default(),"stale":grouped.remove("stale").unwrap_or_default(),"records":records})
}
fn counts(resolution: &Value) -> Value {
    json!({"resolved":resolution["resolved"].as_array().map_or(0, Vec::len),"unresolved":resolution["unresolved"].as_array().map_or(0, Vec::len),"stale":resolution["stale"].as_array().map_or(0, Vec::len)})
}

pub fn open(engine: &mut Engine, args: &Value) -> Result<Value, String> {
    let case = args.get("case").cloned().ok_or("case is required")?;
    if !case.is_object() || !case.get("artifact").map_or(false, Value::is_object) {
        return Err("case must be an object with artifact object".to_string());
    }
    let evidence_refs = refs(args)?;
    let resolution = resolve_all(engine, &evidence_refs);
    let gate_id = new_id();
    let state = json!({"gate_id":gate_id,"case":case,"evidence_refs":evidence_refs,"resolution":resolution,"opened_at":now(),"decided":false});
    write_session(&state)?;
    let c = counts(&state["resolution"]);
    Ok(
        json!({"gate_id":state["gate_id"],"resolved":c["resolved"],"unresolved":c["unresolved"],"stale":c["stale"]}),
    )
}
pub fn ask(args: &Value) -> Result<Value, String> {
    let gate_id = args
        .get("gate_id")
        .and_then(Value::as_str)
        .ok_or("gate_id is required")?;
    let state = read_session(gate_id)?;
    let questions: Vec<Value> = state["resolution"]["records"].as_array().into_iter().flatten()
        .filter(|record| matches!(record["status"].as_str(), Some("unresolved") | Some("stale")))
        .map(|record| json!({"ref":record["ref"],"status":record["status"],"goal":record["goal"],"reason":record["reason"]})).collect();
    Ok(json!({"gate_id":gate_id,"questions":questions}))
}
pub fn answer(engine: &mut Engine, args: &Value) -> Result<Value, String> {
    let gate_id = args
        .get("gate_id")
        .and_then(Value::as_str)
        .ok_or("gate_id is required")?;
    let reference = args
        .get("ref")
        .and_then(Value::as_str)
        .ok_or("ref is required")?;
    let mut state = read_session(gate_id)?;
    if !state["evidence_refs"]
        .as_array()
        .map_or(false, |xs| xs.iter().any(|v| v.as_str() == Some(reference)))
    {
        return Err("ref is not part of this gate".to_string());
    }
    let fresh = resolve_one(engine, reference);
    let mut records: Vec<Value> = state["resolution"]["records"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r["ref"].as_str() != Some(reference))
        .collect();
    records.push(fresh.clone());
    let refs: Vec<String> = records
        .iter()
        .filter_map(|r| r["ref"].as_str().map(str::to_owned))
        .collect();
    state["resolution"] = resolve_all(engine, &refs);
    write_session(&state)?;
    Ok(json!({"gate_id":gate_id,"ref":reference,"status":fresh["status"],"record":fresh}))
}
fn answer_json(answer: &Answer) -> Value {
    match answer {
        Answer::Choice {
            choice,
            confidence,
            probabilities,
        } => {
            json!({"choice":choice,"confidence":confidence,"probabilities":probabilities.iter().map(|(k,v)|(k.clone(),json!(v))).collect::<serde_json::Map<String,Value>>() })
        }
        Answer::Score {
            score,
            confidence,
            probabilities,
        } => {
            json!({"score":score,"confidence":confidence,"probabilities":probabilities.iter().map(|(k,v)|(k.to_string(),json!(v))).collect::<serde_json::Map<String,Value>>() })
        }
        Answer::Noul { noul } => json!({"noul":noul}),
    }
}
fn choice(
    response: &mut JevClient,
    artifact: &Value,
    resolution: &Value,
    gate_id: &str,
) -> Result<Value, String> {
    let questions = vec![
        (
            "verdict",
            Question::Choice {
                instructions: "Judge only this artifact against the fixed rubric.".into(),
                criteria: vec![
                    ("pass".into(), "All dimensions sufficient.".into()),
                    ("rework".into(), "A bounded revision is needed.".into()),
                    ("abstain".into(), "Insufficient information.".into()),
                    (
                        "human_review".into(),
                        "A human authority decision is required.".into(),
                    ),
                ],
            },
        ),
        (
            "failed_dimension",
            Question::Choice {
                instructions: "Pick the one material gap, or none.".into(),
                criteria: RUBRIC
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .chain(std::iter::once(("none".into(), "No material gap.".into())))
                    .collect(),
            },
        ),
        (
            "next_action",
            Question::Choice {
                instructions: "Pick the smallest safe next action.".into(),
                criteria: vec![
                    ("implement".into(), "Artifact is ready.".into()),
                    (
                        "clarify_artifact".into(),
                        "Revise the failed dimension.".into(),
                    ),
                    ("gather_evidence".into(), "Read evidence first.".into()),
                    ("ask_human".into(), "Obtain authority.".into()),
                ],
            },
        ),
        (
            "scope_status",
            Question::Choice {
                instructions: "Assess scope only.".into(),
                criteria: vec![
                    (
                        "sufficient".into(),
                        "In/out boundaries are explicit.".into(),
                    ),
                    (
                        "needs_work".into(),
                        "Scope boundary is missing or contradictory.".into(),
                    ),
                ],
            },
        ),
        (
            "evidence_status",
            Question::Choice {
                instructions: "Assess evidence only.".into(),
                criteria: vec![
                    (
                        "sufficient".into(),
                        "Claims have enough supplied evidence.".into(),
                    ),
                    (
                        "needs_work".into(),
                        "A claim lacks supplied evidence or an investigation step.".into(),
                    ),
                ],
            },
        ),
        (
            "acceptance_status",
            Question::Choice {
                instructions: "Assess acceptance only.".into(),
                criteria: vec![
                    (
                        "sufficient".into(),
                        "Checks are observable and falsifiable.".into(),
                    ),
                    (
                        "needs_work".into(),
                        "Success criteria are not testable.".into(),
                    ),
                ],
            },
        ),
        (
            "safety_status",
            Question::Choice {
                instructions: "Assess safety only.".into(),
                criteria: vec![
                    (
                        "sufficient".into(),
                        "Authority and irreversible-action gates are explicit.".into(),
                    ),
                    ("needs_work".into(), "A safety gate is missing.".into()),
                ],
            },
        ),
    ];
    let state = json!({"artifact":artifact,"evidence_resolution":resolution,"rubric":RUBRIC});
    if serde_json::to_string(&state)
        .map_err(|e| e.to_string())?
        .len()
        > 18_000
    {
        return Err("gate state exceeds the JEV input budget; reduce the evidence set".to_string());
    }
    let question_cart = Value::Object(
        questions
            .iter()
            .map(|(id, question)| (id.to_string(), question.to_json()))
            .collect(),
    );
    eprintln!("lemmajevgaun: gate_id={gate_id} jev_questions={question_cart}");
    let answers = response.ask(&state, &questions)?.answers;
    let get = |key: &str| {
        answers
            .get(key)
            .map(answer_json)
            .ok_or_else(|| format!("JEV response omitted {key}"))
    };
    Ok(
        json!({"verdict":get("verdict")?,"failed_dimension":get("failed_dimension")?,"next_action":get("next_action")?,"dimensions":{"scope":get("scope_status")?,"evidence":get("evidence_status")?,"acceptance":get("acceptance_status")?,"safety":get("safety_status")?}}),
    )
}
fn loop_decision(review: &Value) -> Value {
    let verdict = &review["verdict"];
    let selected = verdict["choice"].as_str().unwrap_or("abstain");
    let probability = verdict["probabilities"]
        .get(selected)
        .and_then(Value::as_f64)
        .or_else(|| verdict["confidence"].as_f64())
        .unwrap_or(0.0);
    if probability < 0.7 {
        return json!({"choice":"abstain","reason":"low_jev_confidence","probability":probability});
    }
    if selected != "pass" {
        return json!({"choice":selected,"reason":"jev_verdict","probability":probability});
    }
    if review["dimensions"].as_object().map_or(false, |dims| {
        dims.values()
            .any(|v| v["choice"].as_str() == Some("needs_work"))
    }) {
        return json!({"choice":"rework","reason":"dimension_needs_work","probability":probability});
    }
    if review["failed_dimension"]["choice"].as_str() != Some("none")
        || review["next_action"]["choice"].as_str() != Some("implement")
    {
        return json!({"choice":"rework","reason":"pass_conflicts_with_followup","probability":probability});
    }
    json!({"choice":"pass","reason":"high_confidence_consistent_pass","probability":probability})
}
/// Approved builders: id -> shell command. Source of truth is the volume file
/// (default /data/builders.json, LEMMALOG_GAUNTLET_BUILDERS_PATH), so a
/// `docker compose up -d` by anyone keeps the registry; the env var is the
/// fallback for native runs without a volume. An empty registry is fail-loud:
/// gate_decide would reject every rework with "builder ... is not approved".
fn builders() -> &'static Value {
    static REGISTRY: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| {
        let path = std::env::var("LEMMALOG_GAUNTLET_BUILDERS_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/data/builders.json"));
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(map) = serde_json::from_str::<Value>(&raw) {
                if map.is_object() {
                    return map;
                }
                eprintln!("lemmajevgaun: {path:?} is not a JSON object of builders");
            } else {
                eprintln!("lemmajevgaun: {path:?} is not JSON; falling back to the env");
            }
        }
        std::env::var("LEMMALOG_GAUNTLET_BUILDERS_JSON")
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or_else(|| Value::Object(Default::default()))
    })
}

/// Read-only view of the approved builders for the MCP surface.
pub fn builders_list() -> Value {
    builders().clone()
}

/// Startup check: an empty registry blocks every rework cycle, and the
/// operator had no way to notice until a decide failed (bead lemmalog-src-05x).
pub fn warn_if_no_builders() {
    let n = builders().as_object().map_or(0, |m| m.len());
    if n == 0 {
        eprintln!(
            "GAUNTLET: no approved builder (empty builders file and LEMMALOG_GAUNTLET_BUILDERS_JSON); gate_decide will reject every rework with 'builder ... is not approved'"
        );
    }
}

fn builder_command(name: &str) -> Result<String, String> {
    builders()
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            let approved: Vec<&str> = builders().as_object().map_or_else(Vec::new, |m| {
                let mut v: Vec<&str> = m.keys().map(String::as_str).collect();
                v.sort_unstable();
                v
            });
            format!(
                "builder {name:?} is not approved; registry has [{}] — write the id into {} or LEMMALOG_GAUNTLET_BUILDERS_JSON",
                approved.join(", "),
                std::env::var("LEMMALOG_GAUNTLET_BUILDERS_PATH")
                    .unwrap_or_else(|_| "/data/builders.json".to_string())
            )
        })
}
fn build(name: &str, packet: &Value) -> Result<Value, String> {
    let command = builder_command(name)?;
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("builder spawn: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("builder stdin unavailable")?
        .write_all(serde_json::to_string(packet).unwrap().as_bytes())
        .map_err(|e| format!("builder stdin: {e}"))?;
    let output = child
        .wait_with_output()
        .map_err(|e| format!("builder wait: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "builder_failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(500)
                .collect::<String>()
        ));
    }
    let artifact: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| "builder_must_emit_one_json_object".to_string())?;
    if !artifact.is_object() {
        return Err("builder_must_emit_json_object".to_string());
    }
    Ok(artifact)
}
fn inject(case: &Value, resolution: &Value) -> Value {
    let mut enriched = case.clone();
    let Some(artifact) = enriched.get_mut("artifact").and_then(Value::as_object_mut) else {
        return enriched;
    };
    let evidence = artifact.entry("evidence").or_insert_with(|| json!([]));
    if !evidence.is_array() {
        *evidence = json!([]);
    }
    let lines = evidence.as_array_mut().unwrap();
    let header = "EVIDENCIA COMPROBADA POR MAQUINA contra el motor de hechos (no es una afirmacion del autor del artefacto):";
    if lines.iter().any(|line| line.as_str() == Some(header)) {
        return enriched; // the builder kept this exact, already-resolved evidence packet
    }
    lines.push(json!(header));
    for record in resolution["records"].as_array().into_iter().flatten() {
        let status = record["status"].as_str().unwrap_or("unresolved");
        let label = record["ref"].as_str().unwrap_or("");
        let rows = record["rows"]
            .as_array()
            .map(|x| {
                x.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        lines.push(json!(match status {
            "resolved" => format!("COMPROBADO {label} -> {rows}"),
            "stale" => format!("OBSOLETO {label} -> {rows}"),
            _ => format!("NO RESUELTO {label}: el motor no devolvio ese hecho"),
        }));
    }
    enriched
}
fn run(
    case: &Value,
    builder: &str,
    resolution: &Value,
    max_cycles: usize,
) -> Result<Value, String> {
    let effective = inject(case, resolution);
    let mut artifact = effective["artifact"].clone();
    let mut cycles = Vec::new();
    let base = std::env::var("LEMMALOG_JEV_BASE")
        .unwrap_or_else(|_| "https://api.typesafe.ai".to_string());
    let model = std::env::var("LEMMALOG_JEV_MODEL").unwrap_or_else(|_| "jev-latest".to_string());
    let mut client = JevClient::with_base(&base, &model);
    for cycle in 0..=max_cycles {
        // Builders may replace the artifact. Reinject the complete evidence packet
        // before every JEV call, rather than trusting a replacement to retain it.
        let judged_artifact = inject(&json!({"artifact":artifact}), resolution)["artifact"].clone();
        let review = choice(
            &mut client,
            &judged_artifact,
            resolution,
            effective["id"].as_str().unwrap_or_default(),
        )
        .map_err(|e| {
            eprintln!(
                "lemmajevgaun: gate_id={} cycle={cycle} jev_error={e}",
                effective["id"].as_str().unwrap_or_default()
            );
            e
        })?;
        eprintln!(
            "lemmajevgaun: gate_id={} cycle={cycle} jev_review={review}",
            effective["id"].as_str().unwrap_or_default()
        );
        let decision = loop_decision(&review);
        cycles.push(json!({"cycle":cycle,"review":review,"gate":decision,"artifact":judged_artifact}));
        if cycles.last().unwrap()["gate"]["choice"].as_str() != Some("rework")
            || cycle == max_cycles
        {
            break;
        }
        let replacement = build(
            builder,
            &json!({"case_id":effective["id"],"artifact":cycles.last().unwrap()["artifact"],"failed_dimension":cycles.last().unwrap()["review"]["failed_dimension"]["choice"],"next_action":cycles.last().unwrap()["review"]["next_action"]["choice"]}),
        )?;
        if replacement == cycles.last().unwrap()["artifact"] {
            cycles.last_mut().unwrap()["gate"] = json!({"choice":"rework","reason":"builder_noop","probability":cycles.last().unwrap()["gate"]["probability"]});
            break;
        }
        artifact = replacement;
    }
    Ok(
        json!({"schema":"gauntlet-report/v1","mode":"explicit_bounded_loop","result":{"id":effective["id"],"final":cycles.last().map(|c|c["gate"].clone()).unwrap_or_else(||json!({"choice":"abstain"})),"cycles":cycles}}),
    )
}
fn checked(report: &Value, resolution: &Value, gate_id: &str) -> Value {
    let original = report["result"]["final"]["choice"]
        .as_str()
        .unwrap_or("abstain");
    let mut final_verdict = original.to_string();
    let mut applied = Vec::<String>::new();
    let cap = |reason: &str, final_verdict: &mut String, applied: &mut Vec<String>| {
        applied.push(reason.to_string());
        if final_verdict == "pass" {
            *final_verdict = "human_review".to_string();
        }
    };
    if resolution["endpoint_ok"] != Value::Bool(true) {
        cap(
            "resolver endpoint_ok is not true",
            &mut final_verdict,
            &mut applied,
        );
    }
    if resolution["unresolved"]
        .as_array()
        .map_or(false, |x| !x.is_empty())
    {
        cap(
            "unresolved evidence reference(s)",
            &mut final_verdict,
            &mut applied,
        );
    }
    if resolution["stale"]
        .as_array()
        .map_or(false, |x| !x.is_empty())
    {
        cap(
            "stale evidence reference(s)",
            &mut final_verdict,
            &mut applied,
        );
    }
    if report["result"]["cycles"]
        .as_array()
        .and_then(|x| x.last())
        .and_then(|x| x["review"]["dimensions"].as_object())
        .map_or(false, |dims| {
            dims.values()
                .any(|v| v["choice"].as_str() == Some("needs_work"))
        })
    {
        cap(
            "a review dimension needs_work",
            &mut final_verdict,
            &mut applied,
        );
    }
    if resolution["records"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|r| {
            r["status"].as_str() == Some("resolved") && r["relation"].as_str() == Some("dead_end")
        })
    {
        cap(
            "resolved dead_end requires review",
            &mut final_verdict,
            &mut applied,
        );
    }
    json!({"original_verdict":original,"final_verdict":final_verdict,"applied_rules":applied,"evidence":resolution,"facts_to_assert":[{"subject":gate_id,"relation":"gate_verdict","object":final_verdict,"ts":now()}]})
}
fn append(record: &Value) -> Result<(), String> {
    let path = calibration_path();
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    writeln!(f, "{}", serde_json::to_string(record).unwrap()).map_err(|e| e.to_string())
}
pub fn decide(_engine: &mut Engine, args: &Value) -> Result<Value, String> {
    let id = args
        .get("gate_id")
        .and_then(Value::as_str)
        .ok_or("gate_id is required")?;
    let builder = args
        .get("builder")
        .and_then(Value::as_str)
        .ok_or("builder is required")?;
    let max = args.get("max_cycles").and_then(Value::as_u64).unwrap_or(2) as usize;
    if max < 1 {
        return Err("max_cycles must be a positive integer".to_string());
    };
    let mut state = read_session(id)?;
    let report = run(&state["case"], builder, &state["resolution"], max)?;
    let result = checked(&report, &state["resolution"], id);
    append(
        &json!({"type":"gate","gate_id":id,"verdict":result["final_verdict"],"probability":report["result"]["final"]["probability"],"dimensions":report["result"]["cycles"].as_array().and_then(|x|x.last()).map(|x|x["review"]["dimensions"].clone()),"evidence_refs":state["evidence_refs"],"ts":now()}),
    )?;
    state["decided"] = json!(true);
    state["builder"] = json!(builder);
    state["max_cycles"] = json!(max);
    state["report"] = report.clone();
    state["final_verdict"] = result["final_verdict"].clone();
    write_session(&state)?;
    Ok(
        json!({"gate_id":id,"final_verdict":result["final_verdict"],"applied_rules":result["applied_rules"],"report":report,"facts_to_assert":result["facts_to_assert"]}),
    )
}
pub fn outcome(args: &Value) -> Result<Value, String> {
    let id = args
        .get("gate_id")
        .and_then(Value::as_str)
        .ok_or("gate_id is required")?;
    let correct = args
        .get("was_correct")
        .and_then(Value::as_bool)
        .ok_or("was_correct must be a bool")?;
    let state = read_session(id)?;
    if state["decided"] != Value::Bool(true) {
        return Err("gate_decide must be called before gate_outcome".to_string());
    };
    append(
        &json!({"type":"outcome","gate_id":id,"was_correct":correct,"defect":args.get("defect").cloned().unwrap_or(Value::Null),"evidence":args.get("evidence").cloned().unwrap_or(Value::Null),"ts":now()}),
    )?;
    Ok(json!({"gate_id":id,"recorded":true}))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentMemory, MockExtractor};

    #[test]
    fn evidence_resolution_reads_the_engine_without_http() {
        let mut memory = AgentMemory::new(MockExtractor::new(0.9), "").unwrap();
        memory.observe_at("alice --works_at--> acme", 100);
        memory.maintain(100);
        let resolution = resolve_all(
            &mut memory.engine,
            &["alice|works_at|acme".to_string(), "alice|works_at|other".to_string()],
        );
        assert_eq!(resolution["resolved"].as_array().unwrap().len(), 1);
        assert_eq!(resolution["unresolved"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn hard_rules_only_lower_a_pass() {
        let report = json!({"result":{"final":{"choice":"pass"},"cycles":[{"review":{"dimensions":{"evidence":{"choice":"needs_work"}}}}]}});
        let resolution = json!({"endpoint_ok":true,"resolved":[],"unresolved":[],"stale":[],"records":[]});
        let result = checked(&report, &resolution, "gate-test");
        assert_eq!(result["final_verdict"], "human_review");
        let cautious = json!({"result":{"final":{"choice":"abstain"},"cycles":[]}});
        assert_eq!(checked(&cautious, &resolution, "gate-test")["final_verdict"], "abstain");
    }
}
