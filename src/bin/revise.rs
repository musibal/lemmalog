//! Deterministic gauntlet builder: repairs only the `evidence` dimension by
//! removing machine-verified UNRESOLVED/OBSOLETE lines and recording the
//! retraction. Any other gap fails loudly (exit 1) — a script cannot reason.
use serde_json::{json, Value};
use std::io::{self, Read};

fn main() {
    if let Err(e) = run() {
        eprintln!("revise: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut raw = String::new();
    io::stdin()
        .read_to_string(&mut raw)
        .map_err(|e| format!("stdin: {e}"))?;
    let packet: Value =
        serde_json::from_str(raw.trim()).map_err(|e| format!("packet not JSON: {e}"))?;
    let revised = transform(packet)?;
    println!(
        "{}",
        serde_json::to_string(&revised).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn transform(packet: Value) -> Result<Value, String> {
    if packet["failed_dimension"].as_str() != Some("evidence") {
        return Err(format!(
            "no deterministic repair for failed_dimension={}; edita el artefacto a mano",
            packet["failed_dimension"]
        ));
    }
    let artifact = packet["artifact"]
        .as_object()
        .ok_or("packet.artifact is not an object")?
        .clone();
    let evidence = artifact["evidence"].as_array().cloned().unwrap_or_default();
    let bad: Vec<&Value> = evidence
        .iter()
        .filter(|l| {
            let s = l.as_str().unwrap_or("");
            s.starts_with("NO RESUELTO ") || s.starts_with("OBSOLETO ")
        })
        .collect();
    if bad.is_empty() {
        return Err("no unverified evidence left to retract; needs a human edit".to_string());
    }
    let mut retractions = Vec::new();
    let mut kept = Vec::new();
    for line in &evidence {
        let s = line.as_str().unwrap_or_default();
        if s.starts_with("NO RESUELTO ") || s.starts_with("OBSOLETO ") {
            let rest = s
                .strip_prefix("NO RESUELTO ")
                .or_else(|| s.strip_prefix("OBSOLETO "))
                .unwrap_or(s);
            let label = rest.split(':').next().unwrap_or(rest).trim();
            retractions.push(json!(format!(
                "{label} — se retira: no verificable en el motor de hechos"
            )));
        } else {
            kept.push(line.clone());
        }
    }
    let mut revised = artifact.clone();
    revised.insert("evidence".to_string(), Value::Array(kept));
    revised.insert("retractions".to_string(), json!(retractions));
    Ok(Value::Object(revised))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str =
        "EVIDENCIA COMPROBADA POR MAQUINA contra el motor de hechos (no es una afirmacion del autor del artefacto):";

    fn packet(artifact: Value, dim: &str) -> Value {
        json!({"case_id":"g1","failed_dimension":dim,"next_action":"clarify_artifact","artifact":artifact})
    }

    #[test]
    fn retracts_unverified_lines_and_keeps_verified() {
        let out = transform(packet(
            json!({"plan":"X","evidence":[HEADER,"COMPROBADO current(a,b,c) -> filas","NO RESUELTO current(x,y,z): el motor no devolvio ese hecho","OBSOLETO current(p,q,r) -> filas viejas"]}),
            "evidence",
        ))
        .unwrap();
        let ev = out["evidence"].as_array().unwrap();
        assert_eq!(ev.len(), 2, "header + verified line only");
        assert_eq!(ev[0].as_str().unwrap(), HEADER);
        let retractions: Vec<&str> = out["retractions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        assert!(retractions[0].starts_with("current(x,y,z)"), "label must drop the NO RESUELTO prefix");
        assert!(retractions[1].starts_with("current(p,q,r)"));
    }

    #[test]
    fn nothing_to_repair_fails_loudly() {
        assert!(transform(packet(json!({"evidence":["COMPROBADO a"]}), "evidence")).is_err());
    }

    #[test]
    fn non_evidence_dimension_fails_loudly() {
        assert!(transform(packet(json!({}), "safety")).is_err());
    }
}