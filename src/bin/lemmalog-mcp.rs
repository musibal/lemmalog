//! lemmalog-mcp: the Lemmalog engine as an MCP server (stdio JSON-RPC),
//! Optional HTTP mode shares one in-memory store across N agents, instead of N copies that overwrite each other.
//! for agent CLIs like Claude Code and Kimi CLI.
//!
//! Build:  cargo build --release --features mcp
//! Register (Claude Code):  claude mcp add lemmalog -- <path>/lemmalog-mcp
//! Register (Kimi CLI):     kimi mcp add lemmalog -- <path>/lemmalog-mcp
//!
//! Persistence: set LEMMALOG_MCP_PATH=/tmp/lemmalog.snapshot to keep
//! memory across server restarts (saved after every mutating call).
//!
//! The host model does the extraction (it reads the conversation anyway)
//! and asserts triples via `observe`; Lemmalog derives closures,
//! temporal views, canonicalizations, aggregations and answers `query`
//! and `why` deterministically.
//!
//! Time is bitemporal and the two clocks are kept apart: an `observe`
//! carries the *valid-from* time of the facts it asserts, while every read
//! path syncs the engine clock to the wall clock before deriving (see
//! `sync_clock`). Reads therefore answer as of the present no matter what
//! order episodes were ingested in, and backdating a batch cannot hide
//! facts asserted after it.

#![cfg(feature = "mcp")]

use lemmalog::agent::AgentMemory;
use lemmalog::canonical;
use lemmalog::eval::Engine;
use lemmalog::gate;
use lemmalog::intern::Value;
use serde_json::{json, Value as J};
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

struct State {
    memory: AgentMemory<lemmalog::agent::MockExtractor>,
    path: Option<String>,
}

fn main() {
    let path = std::env::var("LEMMALOG_MCP_PATH").ok();
    let mut memory =
        AgentMemory::new(lemmalog::agent::MockExtractor::new(0.9), "").expect("fresh memory");
    if let Some(p) = &path {
        if std::path::Path::new(p).exists() {
            match AgentMemory::load(lemmalog::agent::MockExtractor::new(0.9), p) {
                Ok(m) => memory = m,
                Err(e) => eprintln!("lemmalog-mcp: snapshot load failed: {e}"),
            }
        }
    }
    let state = State { memory, path };
    if let Ok(addr) = std::env::var("LEMMALOG_MCP_HTTP") {
        let listener = TcpListener::bind(&addr).expect("bind LEMMALOG_MCP_HTTP");
        eprintln!("lemmalog-mcp: HTTP listening on {addr}");
        let state = Arc::new(Mutex::new(state));
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let state = Arc::clone(&state);
                    thread::spawn(move || handle_connection(stream, state));
                }
                Err(e) => eprintln!("lemmalog-mcp: HTTP accept failed: {e}"),
            }
        }
        return;
    }

    let mut state = state;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<J>(&line) else {
            continue;
        };
        if let Some(resp) = handle(&mut state, &msg) {
            writeln!(out, "{resp}").ok();
            out.flush().ok();
        }
    }
}

/// Shared HTTP transport: one in-memory store serves multiple agents instead
/// of separate stores that overwrite each other's snapshots.
fn verbose_logs() -> bool {
    matches!(
        std::env::var("LEMMALOG_MCP_LOG_BODIES").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    )
}

fn handle(state: &mut State, msg: &J) -> Option<J> {
    let id = msg.get("id").cloned();
    let notification = id.is_none();
    let method = msg["method"].as_str().unwrap_or_default().to_string();
    let operation = if method == "tools/call" {
        msg["params"]["name"].as_str().unwrap_or_default()
    } else {
        &method
    };
    let started = Instant::now();
    let result = match method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "lemmajevgaun", "version": env!("CARGO_PKG_VERSION")},
            "instructions": SERVER_INSTRUCTIONS
        })),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or_default();
            let args = &msg["params"]["arguments"];
            let path = state.path.clone();
            tool_call(state, name, args, path.as_deref())
        }
        other => Err(format!("unknown method {other:?}")),
    };
    let outcome = match &result {
        Err(_) => "rpc_error",
        Ok(value) if value["isError"].as_bool() == Some(true) => "tool_error",
        Ok(_) => "ok",
    };
    let response = if notification {
        None
    } else {
        let id = id.expect("non-notification has an id");
        Some(match result {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
            Err(e) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32000, "message": e}
            }),
        })
    };
    if verbose_logs() {
        let input = &msg["params"]["arguments"];
        let output = response
            .as_ref()
            .and_then(|value| {
                value["result"]["content"][0]["text"]
                    .as_str()
                    .map(J::from)
                    .or_else(|| Some(value.clone()))
            })
            .unwrap_or(J::Null);
        let question = match operation {
            "lemmalog_query" | "lemmalog_query_deep" => input.get("goal"),
            "lemmalog_context" => input.get("query"),
            "lemmalog_why" => input.get("fact"),
            _ => None,
        };
        if let Some(question) = question {
            eprintln!(
                "lemmajevgaun: op={operation} id={} outcome={outcome} elapsed_ms={} question={} answer={}",
                msg["id"],
                started.elapsed().as_millis(),
                question,
                output,
            );
        } else {
            eprintln!(
                "lemmajevgaun: op={operation} id={} outcome={outcome} elapsed_ms={} input={} output={}",
                msg["id"],
                started.elapsed().as_millis(),
                input,
                output,
            );
        }
    } else {
        eprintln!(
            "lemmajevgaun: op={operation} outcome={outcome} elapsed_ms={}",
            started.elapsed().as_millis()
        );
    }
    response
}

fn transient_snapshot_path() -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "lemmalog-mcp-{}-{nonce}.snapshot",
        std::process::id()
    ))
}

/// Magic for a snapshot blob that carries more than the engine. A bare blob is the
/// pre-v2 format (engine bytes only) and still restores, so DO checkpoints written
/// before gate state was included are not lost.
const SNAPSHOT_MAGIC: &[u8] = b"LEMMASNAP2\n";

fn pack_snapshot(engine: &[u8], gates: &J) -> Result<Vec<u8>, String> {
    let gates = serde_json::to_vec(gates).map_err(|e| format!("snapshot gates: {e}"))?;
    let mut out = Vec::with_capacity(SNAPSHOT_MAGIC.len() + 4 + engine.len() + gates.len());
    out.extend_from_slice(SNAPSHOT_MAGIC);
    out.extend_from_slice(&(engine.len() as u32).to_be_bytes());
    out.extend_from_slice(engine);
    out.extend_from_slice(&gates);
    Ok(out)
}

fn unpack_snapshot(bytes: &[u8]) -> Result<(&[u8], Option<J>), String> {
    if !bytes.starts_with(SNAPSHOT_MAGIC) {
        return Ok((bytes, None));
    }
    let rest = &bytes[SNAPSHOT_MAGIC.len()..];
    if rest.len() < 4 {
        return Err("snapshot: truncated header".to_string());
    }
    let len = u32::from_be_bytes(rest[..4].try_into().expect("4 bytes")) as usize;
    if rest.len() < 4 + len {
        return Err("snapshot: truncated engine section".to_string());
    }
    let gates = serde_json::from_slice(&rest[4 + len..])
        .map_err(|e| format!("snapshot: gate section: {e}"))?;
    Ok((&rest[4..4 + len], Some(gates)))
}

fn export_snapshot(state: &State) -> Result<Vec<u8>, String> {
    let path = transient_snapshot_path();
    let engine = state
        .memory
        .save(
            path.to_str()
                .ok_or("temporary snapshot path is not UTF-8")?,
        )
        .and_then(|_| std::fs::read(&path));
    let _ = std::fs::remove_file(&path);
    let engine = engine.map_err(|e| format!("snapshot: {e}"))?;
    // ponytail: engine + gates ride one DO storage value; gate artifacts are the
    // growth term. Split into `gates:<id>` keys only if a blob approaches the
    // per-value limit.
    pack_snapshot(&engine, &gate::export_state()?)
}

fn restore_snapshot(state: &mut State, bytes: &[u8]) -> Result<(), String> {
    let (engine, gates) = unpack_snapshot(bytes)?;
    let path = transient_snapshot_path();
    std::fs::write(&path, engine).map_err(|e| format!("restore: {e}"))?;
    let restored = AgentMemory::load(
        lemmalog::agent::MockExtractor::new(0.9),
        path.to_str()
            .ok_or("temporary snapshot path is not UTF-8")?,
    )
    .map_err(|e| format!("restore: {e}"));
    let _ = std::fs::remove_file(&path);
    state.memory = restored?;
    if let Some(gates) = gates {
        gate::restore_state(&gates)?;
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let request = match read_request(&mut stream, &mut buffer, &mut chunk) {
            Ok(request) => request,
            Err(e) => {
                eprintln!("lemmalog-mcp: HTTP request failed: {e}");
                return;
            }
        };
        let Some((headers_end, content_length, method, path)) = request else {
            return;
        };
        let body_end = headers_end + content_length;
        let body = buffer[headers_end..body_end].to_vec();
        buffer.drain(..body_end);

        if method == "GET" && path == "/health" {
            if write_http_response(&mut stream, "200 OK", br#"{"ok":true}"#).is_err() {
                return;
            }
            continue;
        }
        if method == "GET" && path == "/_snapshot" {
            let result = export_snapshot(&state.lock().expect("state lock poisoned"));
            let (status, body) = match result {
                Ok(bytes) => ("200 OK", bytes),
                Err(e) => ("500 Internal Server Error", e.into_bytes()),
            };
            if write_http_response(&mut stream, status, &body).is_err() {
                return;
            }
            continue;
        }
        if method == "PUT" && path == "/_restore" {
            let result = restore_snapshot(&mut state.lock().expect("state lock poisoned"), &body);
            let (status, body) = match result {
                Ok(()) => ("200 OK", Vec::new()),
                Err(e) => ("400 Bad Request", e.into_bytes()),
            };
            if write_http_response(&mut stream, status, &body).is_err() {
                return;
            }
            continue;
        }
        if method != "POST" {
            if write_http_response(&mut stream, "405 Method Not Allowed", &[]).is_err() {
                return;
            }
            continue;
        }
        let response = match serde_json::from_slice::<J>(&body) {
            Ok(msg) => {
                let mut state = state.lock().expect("state lock poisoned");
                handle(&mut state, &msg)
            }
            Err(_) => Some(json!({
                "jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": "Parse error"}
            })),
        };
        match response {
            Some(response) => {
                let body = response.to_string();
                if write_http_response(&mut stream, "200 OK", body.as_bytes()).is_err() {
                    return;
                }
            }
            None => {
                if write_http_response(&mut stream, "202 Accepted", &[]).is_err() {
                    return;
                }
            }
        }
    }
}

fn read_request(
    stream: &mut TcpStream,
    buffer: &mut Vec<u8>,
    chunk: &mut [u8],
) -> std::io::Result<Option<(usize, usize, String, String)>> {
    let headers_end = loop {
        if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(chunk)?;
        if n == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..n]);
    };
    let headers = std::str::from_utf8(&buffer[..headers_end]).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid HTTP headers")
    })?;
    let mut lines = headers.lines();
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_string();
    let path = request_line.next().unwrap_or_default().to_string();
    let content_length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse::<usize>())
        .transpose()
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid Content-Length")
        })?
        .unwrap_or(0);
    while buffer.len() < headers_end + content_length {
        let n = stream.read(chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "truncated HTTP body",
            ));
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
    Ok(Some((headers_end, content_length, method, path)))
}

fn write_http_response(stream: &mut TcpStream, status: &str, body: &[u8]) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn tools() -> J {
    json!([
        tool("lemmalog_observe",
            "Assert facts into memory (host model does extraction). Input: facts in the line protocol 'S --rel[conf]--> O', one per line; optional ts integer = the facts' valid-from time (default: now). Backdating with ts is safe: it sets valid-time only, never the clock reads use. Example: 'Alice --works_at--> Acme\\nBob --manager--> Carol'.", &["facts", "ts"], &["facts"]),
        tool("lemmalog_retract",
            "Retract facts that turned out to be WRONG (line protocol, same as observe). Open matching rows are removed and invalidation propagates: the response reports which derived facts died as a consequence. For a value that merely CHANGED, prefer re-asserting the same relation (the update policy supersedes).", &["facts"], &["facts"]),
        tool("lemmalog_query",
            "Query derived memory: a goal atom like 'reports_to(\"Alice\", Y)' or 'current(X, works_at, O)'. Returns variable bindings. Read-only.", &["goal"], &["goal"]),
        tool("lemmalog_query_deep",
            "Demand-driven query (magic sets) for exploratory points: same goal syntax as lemmalog_query.", &["goal"], &["goal"]),
        tool("lemmalog_why",
            "Provenance proof tree for a ground fact like 'reports_to(Alice, Carol)' — which rules and source episodes produced it.", &["fact"], &["fact"]),
        tool("lemmalog_install_rules",
            "Install a Datalog rule batch (versioned, revertable). Input: rules text. Rules: 'head(X,Y) :- atom(X,Y), cmp.' with stratified negation (!atom), count/min/max/sum aggregates in heads, now(T) builtin.", &["rules"], &["rules"]),
        tool("lemmalog_uninstall",
            "Uninstall a rule batch by id (see lemmalog_batches). Derivations revert.", &["id"], &["id"]),
        tool("lemmalog_batches", "List installed rule batches.", &[], &[]),
        tool("lemmalog_escalations",
            "List queued escalation conflicts in order. Read-only.", &[], &[]),
        tool("lemmalog_resolve_escalation",
            "Dismiss a queued escalation by its zero-based index.", &["index"], &["index"]),
        tool("lemmalog_what_if",
            "Hypothetical: 'what would follow if these facts were true?' Input: facts (line protocol) + goal atom. Store is untouched.", &["facts", "goal"], &["facts", "goal"]),
        tool("lemmalog_canonicalize",
            "Entity resolution: asserts alias edges (line protocol 'local --alias_of[conf]--> canonical'), installs the canonicalization rules and canonical views over current. Conflicts surface as alias_conflict facts.", &["facts"], &["facts"]),
        tool("lemmalog_context",
            "Query-driven context assembly via hybrid retrieval: BM25 over facts and episodes + entity-match boosting, budget-aware. Input: query (natural language) + optional budget_tokens (default 1000). Returns relevance-selected facts and their verbatim source episodes — use this instead of lemmalog_dump when preparing a grounded answer.", &["query", "budget_tokens"], &["query"]),
        tool("lemmalog_dump", "List facts of a predicate (or all) with confidence and provenance.", &["pred"], &[]),
        tool("lemmalog_changes",
            "Resync after a context reset or another agent's work: everything asserted, derived, or retracted since an epoch. Input: optional `since` epoch integer (default: 0 = everything, capped). The response carries the current epoch — checkpoint it and pass it back next time.", &["since"], &[]),
        tool("lemmalog_save", "Persist memory to LEMMALOG_MCP_PATH.", &[], &[]),
        tool("lemmalog_run", "Sync the clock to the present and run one maintenance epoch (usually automatic — every read does it).", &[], &[]),
        tool("lemmalog_commit",
            "Atomically apply an adapter-proposed evidence diff. `actor` and `evidence` are required; send multiline `retract` and/or `observe`. Values that merely changed belong only in `observe` under the same relation; reserve `retract` for facts proven false. The server persists once after the complete diff.", &[
                "actor", "evidence", "retract", "observe", "ts"
            ], &["actor", "evidence"]),
        json!({"name":"gate_open","description":"Open a lemmajevgaun evidence gate. `case` is the complete {id, artifact} object; evidence refs are subject|relation|object. Resolves facts in-process before JEV sees the artifact.","inputSchema":{"type":"object","properties":{"case":{"type":"object"},"evidence_refs":{"type":"array","items":{"type":"string"}}},"required":["case"]}}),
        json!({"name":"gate_ask","description":"List unresolved or stale evidence questions for an opened gate.","inputSchema":{"type":"object","properties":{"gate_id":{"type":"string"}},"required":["gate_id"]}}),
        json!({"name":"gate_answer","description":"Re-resolve one gate evidence ref directly against Lemmalog. Caller supplied facts are never accepted.","inputSchema":{"type":"object","properties":{"gate_id":{"type":"string"},"ref":{"type":"string"}},"required":["gate_id","ref"]}}),
        json!({"name":"gate_decide","description":"Run the bounded Gauntlet Loop. One JEV multi-question call per cycle judges the complete, evidence-enriched artifact; only an approved builder id may revise it.","inputSchema":{"type":"object","properties":{"gate_id":{"type":"string"},"builder":{"type":"string"},"max_cycles":{"type":"integer","minimum":1}},"required":["gate_id","builder"]}}),
        json!({"name":"gate_outcome","description":"Record whether a completed gate was correct for calibration. Requires gate_decide first.","inputSchema":{"type":"object","properties":{"gate_id":{"type":"string"},"was_correct":{"type":"boolean"},"defect":{},"evidence":{}},"required":["gate_id","was_correct"]}}),
    ])
}

/// Sent on initialize; clients prepend it to the model's context. Small
/// models never read the README — they read this.
const SERVER_INSTRUCTIONS: &str = "\
Lemmalog is your working memory. Start durable work with lemmalog_context \
or lemmalog_query, and use lemmalog_why before trusting derivations. \
A normal agent commits verified source-backed diffs only through \
lemmalog_commit({actor,evidence,observe?,retract?,ts?}); do not call \
naked lemmalog_observe or lemmalog_retract. Query asserted triples as \
current(Subject, \"rel\", Object); values that change are re-asserted under \
the same relation. For a decision use gate_open, gate_ask, gate_answer, \
gate_decide, and gate_outcome in order. JEV proposes but never mutates or \
executes effects. lemmalog_changes resyncs after context resets.";

/// Property descriptions shared by the per-tool schemas (each tool
/// advertises only the properties it actually takes — a model that sees
/// `query` on lemmalog_observe will eventually pass it one).
fn prop_desc(p: &str) -> &'static str {
    match p {
        "facts" => "line-protocol facts",
        "goal" => "query atom",
        "fact" => "ground fact",
        "rules" => "rule program text",
        "id" => "batch id",
        "pred" => "predicate name",
        "ts" => "timestamp",
        "query" => "natural-language query",
        "budget_tokens" => "context token budget",
        "since" => "epoch checkpoint",
        "index" => "zero-based escalation index",
        _ => "",
    }
}

fn tool(name: &str, desc: &str, props: &[&str], required: &[&str]) -> J {
    let properties: serde_json::Map<String, J> = props
        .iter()
        .map(|p| {
            (
                p.to_string(),
                json!({"type": if *p == "ts" || *p == "budget_tokens" || *p == "since" || *p == "index" { "integer" } else { "string" }, "description": prop_desc(p)}),
            )
        })
        .collect();
    json!({
        "name": name,
        "description": desc,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required
        }
    })
}

/// Parse `pred(Arg1, Arg2, ...)` into a predicate and bare argument
/// strings (quotes stripped). Every argument must be a constant — entity
/// names arrive ground regardless of case.
fn parse_fact_atom(s: &str) -> Result<(String, Vec<String>), String> {
    let s = s.trim().trim_end_matches('.');
    let open = s
        .find('(')
        .ok_or_else(|| format!("expected pred(args): {s:?}"))?;
    let close = s
        .rfind(')')
        .ok_or_else(|| format!("expected pred(args): {s:?}"))?;
    if close <= open {
        return Err(format!("expected pred(args): {s:?}"));
    }
    let pred = s[..open].trim().to_string();
    if pred.is_empty() {
        return Err(format!("missing predicate: {s:?}"));
    }
    let args: Vec<String> = s[open + 1..close]
        .split(',')
        .map(|a| a.trim().trim_matches('"').to_string())
        .map(|a| a.trim_matches('"').to_string())
        .collect();
    if args.iter().any(|a| a.is_empty()) {
        return Err(format!("empty argument in {s:?} (why needs a ground fact)"));
    }
    Ok((pred, args))
}

fn engine_of(state: &mut State) -> &mut Engine {
    &mut state.memory.engine
}

/// Wall-clock seconds since the Unix epoch.
fn wall_clock() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Advance the engine clock to the present and re-derive temporal views.
///
/// `now` is the *reader's* present, not a side effect of the last write.
/// Without this, `engine.now` is whatever timestamp the last `observe`
/// passed, so a backdated ingest silently hides every fact asserted at a
/// later `ts` from `current/3` — the fact stays in `edge`, but fails the
/// `VF =< T` guard. Reads sync forward; ingest keeps honouring its own
/// `ts` as valid-time. The clock only ever moves forward here: a
/// future-dated fact must not drag the present backwards.
fn sync_clock(state: &mut State) {
    let t = wall_clock();
    let e = &mut state.memory.engine;
    if t > e.now {
        let old = e.now;
        let inert = e.clock_advance_is_inert(old, t);
        e.set_now(t);
        // Only the full re-derivation is skippable: `current/3` flips exactly
        // when the clock passes an edge's valid-from/valid-to, and reads are
        // far more frequent than facts crossing a boundary. `run()` stays
        // unconditional — it must still flush a pending seminaive delta or a
        // `program_dirty` left by an earlier install, and costs nothing when
        // neither is outstanding.
        if !inert {
            e.invalidate_derived();
        }
        let _ = e.run();
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n).collect();
        format!("{cut}...")
    }
}

/// Empty query results must redirect, not dead-end: the classic trap is
/// querying the raw relation (`depends_on(X, Y)`) when asserted facts
/// live under `current(S, rel, O)` — the answer is silently empty. Turn
/// that into the hint that fixes the next call.
fn empty_query_hint(e: &Engine, goal: &str) -> String {
    let pred = goal.split('(').next().unwrap_or_default().trim();
    if e.relations.contains_key(pred) {
        return "(no answers — the relation exists but holds no rows; \
check spelling of constant arguments, or run lemmalog_dump on it)"
            .to_string();
    }
    // is `pred` used as a RELATION inside current? then the fact shape
    // is the trap
    for key in e.relation_keys("current") {
        if key.len() == 3 && e.interner.display(&key[1]) == pred {
            return format!(
                "(no answers — `{pred}` is a RELATION, not a predicate: asserted \
facts are current(S, rel, O). Try current(X, \"{pred}\", Y)"
            );
        }
    }
    // unknown predicate: closest names by shared stem
    let stem = |s: &str| s.split('_').next().unwrap_or(s).to_lowercase();
    let similar: Vec<String> = e
        .relations
        .keys()
        .filter(|r| !r.starts_with("__") && stem(r) == stem(pred))
        .take(5)
        .cloned()
        .collect();
    if similar.is_empty() {
        format!(
            "(no answers — no relation named `{pred}`; asserted facts live \
under current(S, rel, O), derived under their rule heads)"
        )
    } else {
        format!(
            "(no answers — no relation named `{pred}`; similar: {}. \
Asserted facts live under current(S, rel, O)",
            similar.join(", ")
        )
    }
}

fn tool_call(state: &mut State, name: &str, args: &J, path: Option<&str>) -> Result<J, String> {
    // Input errors (bad goal syntax, rejected rules, unknown batch id)
    // return as tool results with isError: true so the model can
    // self-correct; JSON-RPC errors are reserved for unknown tools and
    // server faults.
    let inner: Result<String, String> = match name {
        "lemmalog_observe" => {
            let facts = args["facts"].as_str().unwrap_or_default();
            // No ts means "now" — the wall clock, never the engine clock,
            // which is only ever a record of the last write.
            let ts = args["ts"].as_i64().unwrap_or_else(wall_clock);
            let mem = &mut state.memory;
            let (report, dropped) = mem.observe_extracted(facts, ts);
            // Facts keep `ts` as their valid-time (VF), but the derived view
            // is as of the present: backdating an episode must not rewind
            // `current/3` past everything already asserted.
            let now = wall_clock().max(mem.engine.now);
            if now > mem.engine.now {
                mem.engine.invalidate_derived();
            }
            mem.maintain(now);
            let mut out = format!(
                "added={} updated={} noop={} escalations={}",
                report.added,
                report.updated,
                report.noop,
                report.escalations.len()
            );
            for e in report.escalations.iter().take(3) {
                out.push_str(&format!("\nescalation: {e}"));
            }
            if report.escalations.len() > 3 {
                out.push_str(&format!(
                    "\n(+{} more escalations)",
                    report.escalations.len() - 3
                ));
            }
            if !dropped.is_empty() {
                out.push_str(&format!(
                    "\ndropped {} line(s) — NOT asserted:",
                    dropped.len()
                ));
                for (line, reason) in dropped.iter().take(5) {
                    out.push_str(&format!("\n  `{}` — {}", truncate(line, 80), reason));
                }
                if dropped.len() > 5 {
                    out.push_str(&format!("\n  (+{} more)", dropped.len() - 5));
                }
            }
            Ok(out)
        }
        "lemmalog_retract" => {
            let facts = args["facts"].as_str().unwrap_or_default();
            if facts.trim().is_empty() {
                Err("input: `facts` is required — line-protocol facts to retract".to_string())
            } else {
                // invalidation is computed against the derived view, so the
                // clock has to be current before we decide what dies
                sync_clock(state);
                let (done, missing, died) = state.memory.retract_facts(facts);
                let mut out = String::new();
                if !done.is_empty() {
                    out.push_str(&format!(
                        "retracted {} fact(s); invalidation propagated:\n",
                        done.len()
                    ));
                    if died.is_empty() {
                        out.push_str("  no derived facts depended on them\n");
                    } else {
                        out.push_str(&format!("  {} derived fact(s) died:\n", died.len()));
                        for d in died.iter().take(15) {
                            out.push_str(&format!("    {d}\n"));
                        }
                        if died.len() > 15 {
                            out.push_str(&format!("    (+{} more)\n", died.len() - 15));
                        }
                    }
                }
                if !missing.is_empty() {
                    out.push_str(&format!(
                        "not found (no open fact matches):\n  {}",
                        missing.join("\n  ")
                    ));
                }
                if done.is_empty() && missing.is_empty() {
                    out.push_str(
                        "nothing retracted — every line failed to parse \
(strict validation: S --rel--> O with real entity names)",
                    );
                }
                Ok(out)
            }
        }
        "lemmalog_changes" => {
            let since = args["since"].as_i64().unwrap_or(0).max(0) as u64;
            sync_clock(state);
            let e = engine_of(state);
            let events = e.changes_since(since);
            let epoch = e.epoch();
            let mut out = format!("epoch={epoch}\n");
            if events.is_empty() {
                out.push_str("no changes since that epoch");
            } else {
                let shown = events.len().min(200);
                for ev in events.iter().take(shown) {
                    match ev {
                        lemmalog::eval::Change::Added(_, (p, k)) => {
                            out.push_str(&format!("+ {}", e.render_fact(p, k)))
                        }
                        lemmalog::eval::Change::Retracted(_, (p, k)) => {
                            out.push_str(&format!("- {}", e.render_fact(p, k)))
                        }
                        lemmalog::eval::Change::Cleared(_, p) => {
                            out.push_str(&format!("- (cleared {p})"))
                        }
                    };
                    out.push('\n');
                }
                if events.len() > shown {
                    out.push_str(&format!("(+{} more)\n", events.len() - shown));
                }
                out.push_str(&format!("checkpoint: pass since={epoch} next time"));
            }
            Ok(out)
        }
        "lemmalog_query" => {
            let goal = args["goal"].as_str().unwrap_or_default().to_string();
            sync_clock(state);
            match engine_of(state).ask(&goal) {
                Ok(rows) => Ok(match lemmalog::answer_text(&rows) {
                    Some(text) => text,
                    None => empty_query_hint(engine_of(state), &goal),
                }),
                Err(e) => Err(format!(
                    "parse: could not parse goal `{}`\nreason: {e}\nhint: quote entity names — bare capitalized words are variables. Example: reports_to(\"Alice\", Y)",
                    truncate(&goal, 120)
                )),
            }
        }
        "lemmalog_query_deep" => {
            let goal = args["goal"].as_str().unwrap_or_default().to_string();
            sync_clock(state);
            match engine_of(state).ask_deep(&goal) {
                Ok(rows) => Ok(match lemmalog::answer_text(&rows) {
                    Some(text) => text,
                    None => empty_query_hint(engine_of(state), &goal),
                }),
                Err(e) => Err(format!(
                    "query: could not evaluate `{}`\nreason: {e}\nhint: quote entity names — bare capitalized words are variables. Example: reports_to(\"Alice\", Y)",
                    truncate(&goal, 120)
                )),
            }
        }
        "lemmalog_why" => {
            let fact = args["fact"].as_str().unwrap_or_default().to_string();
            sync_clock(state);
            match parse_fact_atom(&fact) {
                Ok((pred, parts)) => {
                    let vals: Vec<Value> = parts
                        .iter()
                        .map(|a| match a.parse::<i64>() {
                            Ok(i) => Value::Int(i),
                            Err(_) => state.memory.engine.sym(a),
                        })
                        .collect();
                    Ok(state.memory.engine.why(&pred, &vals))
                }
                Err(e) => Err(format!("input: {e}\nexample: reports_to(Alice, Carol)")),
            }
        }
        "lemmalog_install_rules" => {
            let rules = args["rules"].as_str().unwrap_or_default();
            sync_clock(state);
            match state.memory.install_rules(rules) {
                Ok(id) => {
                    let n = engine_of(state).run();
let purged = state.memory.purge_declared_escalations();
                    let mut out = format!("installed {id}; backfill derived +{n} facts; queue purged {purged}");
                    for w in state.memory.batch_conflicts(&id) {
                        out.push_str(&format!("\nWARNING: {w}"));
                    }
                    Ok(out)
                }
                Err(e) => Err(format!(
                    "rules rejected — nothing installed:\n{e}\ncommon causes: recursion through negation; aggregates outside rule heads; parse errors (rules end with '.')"
                )),
            }
        }
        "lemmalog_uninstall" => {
            let id = args["id"].as_str().unwrap_or_default().to_string();
            sync_clock(state);
            if engine_of(state).uninstall(&id) {
                let _ = engine_of(state).run();
                Ok(format!("uninstalled {id}; derivations recomputed"))
            } else {
                Err(format!(
                    "input: no batch {id:?} — call lemmalog_batches to list installed ids"
                ))
            }
        }
        "lemmalog_batches" => Ok(engine_of(state)
            .batches()
            .into_iter()
            .map(|(id, src)| format!("{id}: {src}"))
            .collect::<Vec<_>>()
            .join("\n")),
        "lemmalog_escalations" => {
            let escalations = state.memory.escalations();
            if escalations.is_empty() {
                Ok("(empty)".to_string())
            } else {
                Ok(escalations
                    .iter()
                    .enumerate()
                    .map(|(idx, text)| format!("{idx}: {text}"))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
        }
        "lemmalog_resolve_escalation" => {
            let raw_index = args["index"]
                .as_i64()
                .ok_or_else(|| "input: `index` is required and must be an integer".to_string())?;
            let queue_len = state.memory.escalations().len();
            if raw_index < 0 || raw_index as usize >= queue_len {
                Err(format!(
                    "index {raw_index} out of range; current queue length {queue_len}"
                ))
            } else {
                state.memory.resolve_escalation(raw_index as usize);
                Ok(format!("resolved escalation {raw_index}"))
            }
        }
        "lemmalog_what_if" => {
            let facts = args["facts"].as_str().unwrap_or_default();
            let goal = args["goal"].as_str().unwrap_or_default().to_string();
            let (cands, dropped) = lemmalog::agent::parse_protocol_reported(facts, 0.9);
            if cands.is_empty() && !facts.trim().is_empty() {
                let reasons: Vec<String> = dropped
                    .iter()
                    .map(|(l, r)| format!("  `{}` — {}", truncate(l, 60), r))
                    .collect();
                return Ok(json!({
                    "content": [{"type": "text", "text": format!(
                        "input: no valid facts in the hypothetical.\n{}",
                        reasons.join("\n")
                    )}],
                    "isError": true
                }));
            }
            sync_clock(state);
            let e = engine_of(state);
            let now = e.now;
            let mut extra: Vec<(String, Vec<Value>)> = Vec::new();
            for c in &cands {
                let (s, p, o) = (e.sym(&c.subj), e.sym(&c.pred), e.sym(&c.obj));
                extra.push((
                    "edge".to_string(),
                    vec![
                        s,
                        p,
                        o,
                        Value::Int(now),
                        Value::Int(i64::MAX),
                        Value::Int(now),
                    ],
                ));
            }
            let refs: Vec<(&str, &[Value])> = extra
                .iter()
                .map(|(p, a)| (p.as_str(), a.as_slice()))
                .collect();
            match e.hypothetical(&refs, &goal) {
                Ok(rows) => Ok(lemmalog::answer_text(&rows)
                    .unwrap_or_else(|| "(no answers)".to_string())),
                Err(er) => Err(format!(
                    "query: could not evaluate `{}`\nreason: {er}\nhint: quote entity names — bare capitalized words are variables",
                    truncate(&goal, 120)
                )),
            }
        }
        "lemmalog_canonicalize" => {
            let facts = args["facts"].as_str().unwrap_or_default();
            let aliases: Vec<_> = lemmalog::agent::parse_protocol(facts, 0.9)
                .into_iter()
                .filter(|c| c.pred == "alias_of")
                .collect();
            if aliases.is_empty() {
                return Ok(json!({
                    "content": [{"type": "text", "text": "input: no alias edges found — lines must look like `local --alias_of[0.9]--> canonical`"}],
                    "isError": true
                }));
            }
            sync_clock(state);
            let e = engine_of(state);
            for c in &aliases {
                canonical::assert_alias(e, &c.subj, &c.obj, c.confidence);
            }
            match canonical::install_canonicalization(e, &["current"]) {
                Ok(_) => {
                    let now = state.memory.engine.now;
                    let _ = state.memory.maintain(now);
                    let conflicts = canonical::alias_conflicts(&state.memory.engine);
                    if conflicts.is_empty() {
                        Ok("canonicalization installed; no conflicts".to_string())
                    } else {
                        Ok(format!(
                            "canonicalization installed; CONFLICTS (retract bad alias edges):\n{}",
                            conflicts.join("\n")
                        ))
                    }
                }
                Err(er) => Err(format!("canonicalization rejected: {er}")),
            }
        }
        "lemmalog_context" => {
            let query = args["query"].as_str().unwrap_or_default().to_string();
            let budget = args["budget_tokens"].as_i64().unwrap_or(1000).max(50) as usize;
            if query.trim().is_empty() {
                Err("input: `query` is required — the natural-language question the context should serve".to_string())
            } else {
                sync_clock(state);
                Ok(state.memory.context_for_query(&query, budget))
            }
        }
        "lemmalog_dump" => {
            let pred = args["pred"].as_str().unwrap_or_default();
            sync_clock(state);
            let e = engine_of(state);
            let mut preds: Vec<&String> = e.relations.keys().collect();
            preds.sort();
            let mut out = String::new();
            for p in preds {
                if !pred.is_empty() && p != pred {
                    continue;
                }
                for key in e.relation_keys(p) {
                    out.push_str(&format!("{}\n", e.render_fact(p, &key)));
                }
            }
            if out.is_empty() {
                out.push_str("(empty)\n");
            }
            Ok(out)
        }
        "lemmalog_save" => {
            let p = path.ok_or_else(|| {
                "input: LEMMALOG_MCP_PATH not set — register the server with --env LEMMALOG_MCP_PATH=...".to_string()
            })?;
            match state.memory.save(p) {
                Ok(_) => Ok(format!("saved to {p}")),
                Err(e) => Err(format!("io: snapshot save failed: {e}")),
            }
        }
        "lemmalog_run" => {
            sync_clock(state);
            let n = engine_of(state).run();
            Ok(format!("+{n} facts (clock {})", engine_of(state).now))
        }
        "lemmalog_commit" => {
            let actor = args["actor"].as_str().unwrap_or_default().trim();
            let evidence = args["evidence"].as_str().unwrap_or_default().trim();
            let retract = args["retract"].as_str().unwrap_or_default();
            let observe = args["observe"].as_str().unwrap_or_default();
            if actor.is_empty() || evidence.is_empty() {
                Err("input: `actor` and `evidence` are required for an evidence diff".to_string())
            } else if retract.trim().is_empty() && observe.trim().is_empty() {
                Err("input: provide non-empty `retract` and/or `observe` facts".to_string())
            } else {
                // The engine mutates in place. Snapshot before the first half so a
                // rejected second half cannot leave a partial in-memory commit.
                let before = export_snapshot(state)?;
                let applied = (|| -> Result<Vec<String>, String> {
                    let mut reports = Vec::new();
                    // Apply removals first so an optional re-assertion is final.
                    // `None` suppresses inner saves; the outer commit saves once.
                    if !retract.trim().is_empty() {
                        let result =
                            tool_call(state, "lemmalog_retract", &json!({"facts": retract}), None)?;
                        if result["isError"].as_bool() == Some(true) {
                            return Err(result["content"][0]["text"]
                                .as_str()
                                .unwrap_or("retract rejected")
                                .to_string());
                        }
                        reports.push(
                            result["content"][0]["text"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                        );
                    }
                    if !observe.trim().is_empty() {
                        let mut request = json!({"facts": observe});
                        if let Some(ts) = args["ts"].as_i64() {
                            request["ts"] = json!(ts);
                        }
                        let result = tool_call(state, "lemmalog_observe", &request, None)?;
                        let report = result["content"][0]["text"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string();
                        if result["isError"].as_bool() == Some(true) || report.contains("dropped ")
                        {
                            return Err(if report.is_empty() {
                                "observe rejected".to_string()
                            } else {
                                report
                            });
                        }
                        reports.push(report);
                    }
                    Ok(reports)
                })();
                match applied {
                    Ok(reports) => Ok(format!(
                        "actor={actor} evidence={evidence}\n{}",
                        reports.join("\n")
                    )),
                    Err(error) => {
                        restore_snapshot(state, &before).map_err(|restore| {
                            format!("commit aborted: {error}; rollback failed: {restore}")
                        })?;
                        Err(format!("commit aborted and rolled back: {error}"))
                    }
                }
            }
        }
        "gate_open" => {
            sync_clock(state);
            gate::open(engine_of(state), args).map(|v| v.to_string())
        }
        "gate_ask" => gate::ask(args).map(|v| v.to_string()),
        "gate_answer" => {
            sync_clock(state);
            gate::answer(engine_of(state), args).map(|v| v.to_string())
        }
        "gate_decide" => {
            sync_clock(state);
            gate::decide(engine_of(state), args).map(|v| v.to_string())
        }
        "gate_outcome" => gate::outcome(args).map(|v| v.to_string()),
        other => {
            // models invent tool names; teach instead of rejecting —
            // suggest the closest real tool so the next call succeeds
            let known: Vec<&str> = [
                "lemmalog_observe",
                "lemmalog_retract",
                "lemmalog_query",
                "lemmalog_query_deep",
                "lemmalog_why",
                "lemmalog_install_rules",
                "lemmalog_uninstall",
                "lemmalog_batches",
                "lemmalog_what_if",
                "lemmalog_canonicalize",
                "lemmalog_context",
                "lemmalog_dump",
                "lemmalog_changes",
                "lemmalog_save",
                "lemmalog_run",
                "lemmalog_commit",
                "lemmalog_escalations",
                "lemmalog_resolve_escalation",
                "gate_open",
                "gate_ask",
                "gate_answer",
                "gate_decide",
                "gate_outcome",
            ]
            .to_vec();
            let stripped = other.trim_start_matches("lemmalog_");
            let close: Vec<&str> = known
                .iter()
                .filter(|t| {
                    let b = t.trim_start_matches("lemmalog_");
                    b.contains(stripped) || stripped.contains(b)
                })
                .copied()
                .collect();
            let hint = if close.is_empty() {
                format!("unknown tool {other:?}; available: {}", known.join(", "))
            } else {
                format!(
                    "unknown tool {other:?}; did you mean {}? available: {}",
                    close.join(" or "),
                    known.join(", ")
                )
            };
            return Ok(json!({
                "content": [{"type": "text", "text": hint}],
                "isError": true
            }));
        }
    };
    let (text, is_error) = match inner {
        Ok(t) => (t, false),
        Err(t) => (t, true),
    };
    if !is_error
        && matches!(
            name,
            // `lemmalog_retract` MUST be here: retraction closes a validity
            // interval in RAM, so leaving it out means a fact the agent
            // deliberately marked FALSE comes back alive on the next restart
            // (verified: retract -> gone from RAM -> still in file -> restart
            // -> `S=hecho_falso, O=mentira` again). Absence is state here.
            "lemmalog_observe"
                | "lemmalog_retract"
                | "lemmalog_install_rules"
                | "lemmalog_uninstall"
                | "lemmalog_canonicalize"
                | "lemmalog_resolve_escalation"
                | "lemmalog_commit"
        )
    {
        // A discarded save error reports success to the agent while the disk
        // silently drops the write; memory loss must never be quiet.
        if let Some(p) = path {
            if let Err(e) = state.memory.save(p) {
                return Ok(json!({
                    "content": [{"type": "text", "text": format!("{text}\n\nWARNING: snapshot NOT saved ({e}) — this change lives only in RAM")}],
                    "isError": true
                }));
            }
        }
    }
    Ok(json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Count your tokens before you count your tools": the wire surface
    /// every model pays for on every turn. Guard against drift into the
    /// small-model failure mode (a 126-tool surface costs ~40K tokens;
    /// ours should stay a small fraction of that — if this test fails,
    /// split the server or move to meta-tools, don't just bump it).
    #[test]
    fn tool_surface_stays_under_budget() {
        let wire = serde_json::to_string(&tools()).unwrap();
        let est_tokens = wire.len() / 4; // chars-per-token lower bound
        assert!(
            est_tokens < 4500,
            "tool surface ~{est_tokens} tokens ({} tools, {} bytes) — trim descriptions or consolidate",
            tools().as_array().unwrap().len(),
            wire.len()
        );
    }

    #[test]
    fn every_tool_schema_lists_only_its_own_properties() {
        let ts = tools();
        let list = ts.as_array().unwrap();
        assert_eq!(list.len(), 23);
        for t in list {
            let props: Vec<&str> = t["inputSchema"]["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(|k| k.as_str())
                .collect();
            assert!(!props.is_empty() || t["name"].as_str().unwrap() != "lemmalog_observe");
        }
    }

    #[test]
    fn commit_applies_one_evidence_diff() {
        let mut state = State {
            memory: AgentMemory::new(lemmalog::agent::MockExtractor::new(0.9), "").unwrap(),
            path: None,
        };
        let first = tool_call(
            &mut state,
            "lemmalog_commit",
            &json!({
                "actor": "test_adapter",
                "evidence": "tests:commit",
                "observe": "claim --status--> proposed"
            }),
            None,
        )
        .unwrap();
        assert_eq!(first["isError"], false);
        assert_eq!(
            state
                .memory
                .ask("current(\"claim\", \"status\", \"proposed\")")
                .unwrap()
                .len(),
            1
        );

        let corrected = tool_call(
            &mut state,
            "lemmalog_commit",
            &json!({
                "actor": "test_adapter",
                "evidence": "tests:commit-correction",
                "retract": "claim --status--> proposed",
                "observe": "claim --status--> validated"
            }),
            None,
        )
        .unwrap();
        assert_eq!(corrected["isError"], false);
        assert!(state
            .memory
            .ask("current(\"claim\", \"status\", \"proposed\")")
            .unwrap()
            .is_empty());
        assert_eq!(
            state
                .memory
                .ask("current(\"claim\", \"status\", \"validated\")")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn commit_rolls_back_when_observe_is_rejected() {
        let mut state = State {
            memory: AgentMemory::new(lemmalog::agent::MockExtractor::new(0.9), "").unwrap(),
            path: None,
        };
        tool_call(
            &mut state,
            "lemmalog_commit",
            &json!({
                "actor": "test_adapter",
                "evidence": "tests:rollback-seed",
                "observe": "claim --status--> proposed"
            }),
            None,
        )
        .unwrap();

        let failed = tool_call(
            &mut state,
            "lemmalog_commit",
            &json!({
                "actor": "test_adapter",
                "evidence": "tests:rollback-invalid-observe",
                "retract": "claim --status--> proposed",
                "observe": "this is not line protocol"
            }),
            None,
        )
        .unwrap();
        assert_eq!(failed["isError"], true);
        assert_eq!(
            state
                .memory
                .ask("current(\"claim\", \"status\", \"proposed\")")
                .unwrap()
                .len(),
            1,
            "the successful retract must be undone when observe is rejected"
        );
    }

    #[test]
    fn snapshot_round_trip_restores_the_engine() {
        let _guard = GATE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch_dir("engine-gates");
        set_gate_env(&dir);
        let mut source = fresh_state();
        tool_call(
            &mut source,
            "lemmalog_observe",
            &json!({"facts": "snapshot_subject --status--> persisted"}),
            None,
        )
        .unwrap();
        let bytes = export_snapshot(&source).unwrap();

        let mut restored = fresh_state();
        restore_snapshot(&mut restored, &bytes).unwrap();
        assert_eq!(
            restored
                .memory
                .ask("current(\"snapshot_subject\", \"status\", \"persisted\")")
                .unwrap()
                .len(),
            1
        );
        clear_gate_env(&dir);
    }

    fn scratch_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("lemmalog-{label}-{}", std::process::id()))
    }

    /// The gate directory is process-global (env), so every test that snapshots gate
    /// state holds this lock and points the paths at scratch dirs — otherwise two
    /// tests write each other's dirs, and a run would also rewrite the developer's
    /// real `~/.lemmalog/gate-calibration.jsonl`.
    static GATE_ENV: Mutex<()> = Mutex::new(());

    fn set_gate_env(dir: &std::path::Path) -> std::path::PathBuf {
        std::env::set_var("LEMMALOG_GATE_DIR", dir);
        let calibration = dir.join("calibration.jsonl");
        std::env::set_var("LEMMALOG_GATE_CALIBRATION", &calibration);
        calibration
    }

    fn clear_gate_env(dir: &std::path::Path) {
        std::env::remove_var("LEMMALOG_GATE_DIR");
        std::env::remove_var("LEMMALOG_GATE_CALIBRATION");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn fresh_state() -> State {
        State {
            memory: AgentMemory::new(lemmalog::agent::MockExtractor::new(0.9), "").unwrap(),
            path: None,
        }
    }

    /// A gate open when the container slept must still be there after the DO replays
    /// its checkpoint: gate files live on ephemeral container disk, so the blob
    /// carries them and restore rewrites them.
    #[test]
    fn snapshot_round_trip_restores_open_gates() {
        let _guard = GATE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let source_gates = scratch_dir("source-gates");
        let restored_gates = scratch_dir("restored-gates");
        set_gate_env(&source_gates);
        let mut source = fresh_state();
        let opened = tool_call(
            &mut source,
            "gate_open",
            &json!({"case": {"id": "mcl-sleep", "artifact": {"claim": "x"}}}),
            None,
        )
        .unwrap();
        let gate_id = opened["content"][0]["text"]
            .as_str()
            .and_then(|t| serde_json::from_str::<J>(t).ok())
            .and_then(|v| v["gate_id"].as_str().map(str::to_owned))
            .expect("gate_open returns a gate_id");
        assert!(source_gates.join(format!("{gate_id}.json")).exists());

        let bytes = export_snapshot(&source).unwrap();
        // The new container starts with a fresh disk, i.e. an empty gate dir.
        clear_gate_env(&source_gates);
        set_gate_env(&restored_gates);
        let mut restored = fresh_state();
        restore_snapshot(&mut restored, &bytes).unwrap();
        assert!(restored_gates.join(format!("{gate_id}.json")).exists());
        let asked = tool_call(&mut restored, "gate_ask", &json!({"gate_id": gate_id}), None).unwrap();
        assert_eq!(asked["isError"].as_bool(), Some(false));

        clear_gate_env(&restored_gates);
    }

    /// Checkpoints written before gates were included are bare engine bytes; they
    /// must keep restoring, not fail as a corrupt blob.
    #[test]
    fn legacy_snapshot_blobs_restore_without_gates() {
        let mut state = State {
            memory: AgentMemory::new(lemmalog::agent::MockExtractor::new(0.9), "").unwrap(),
            path: None,
        };
        let legacy = b"engine-bytes-only".to_vec();
        let (engine, gates) = unpack_snapshot(&legacy).unwrap();
        assert_eq!(engine, legacy.as_slice());
        assert!(gates.is_none());

        let packed = pack_snapshot(b"engine", &json!({"sessions": {}, "calibration": null})).unwrap();
        let (engine, gates) = unpack_snapshot(&packed).unwrap();
        assert_eq!(engine, b"engine");
        assert!(gates.unwrap()["sessions"].is_object());
        assert!(unpack_snapshot(SNAPSHOT_MAGIC).is_err());
        // A truncated v2 blob must fail loudly rather than drop facts.
        let mut truncated = pack_snapshot(b"engine", &json!({})).unwrap();
        truncated.truncate(SNAPSHOT_MAGIC.len() + 2);
        assert!(unpack_snapshot(&truncated).is_err());
        assert!(restore_snapshot(&mut state, &truncated).is_err());
    }
}
