//! TypeSafe System One (`jev`) client behind the `jev` feature: typed
//! questions in, calibrated answers out. No text generation, no parsing of
//! prose — the model returns a probability distribution constrained to the
//! options you supplied.
//!
//! The transport is a function from request JSON to response JSON, so tests
//! run without a server; [`JevClient::new`] installs the HTTP transport.
//!
//! What jev is for here: the calibrated number the engine's semiring wants.
//! What it is NOT for: counting, arithmetic, date ordering, or multi-hop
//! reasoning — the engine does those natively and jev is documented-weak at
//! all four.

use std::collections::HashMap;
use std::time::Duration;

pub type Transport = Box<dyn Fn(&str) -> Result<String, String> + Send>;

/// One typed question. IDs are chosen by the caller and never sent to the
/// model, so the instructions must be self-contained.
#[derive(Debug, Clone)]
pub enum Question {
    /// Is this true? Answer is the probability of yes, in [0,1].
    Noul { instructions: String },
    /// Which level? `criteria` is an ordered list, one entry per level
    /// starting at zero. The answer can fall between two levels.
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    /// Which option? `criteria` maps label to description.
    Choice {
        instructions: String,
        criteria: Vec<(String, String)>,
    },
}

impl Question {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        match self {
            Question::Noul { instructions } => serde_json::json!({
                "type": "noul", "instructions": instructions
            }),
            Question::Score {
                instructions,
                criteria,
            } => serde_json::json!({
                "type": "score", "instructions": instructions, "criteria": criteria
            }),
            Question::Choice {
                instructions,
                criteria,
            } => {
                let map: serde_json::Map<String, serde_json::Value> = criteria
                    .iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect();
                serde_json::json!({
                    "type": "choice", "instructions": instructions, "criteria": map
                })
            }
        }
    }
}

/// A typed answer. Exactly one of the shapes is populated, matching the
/// question that produced it.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// Probability that the answer is yes. Noul carries no separate
    /// confidence — the value IS the signal.
    Noul { noul: f64 },
    /// Expected position along the levels, the distribution it came from,
    /// and how peaked that distribution is.
    Score {
        score: f64,
        confidence: f64,
        probabilities: Vec<(i64, f64)>,
    },
    /// Selected label, the distribution across options, and confidence.
    Choice {
        choice: String,
        confidence: f64,
        probabilities: Vec<(String, f64)>,
    },
}

impl Answer {
    /// Confidence where the shape has one; a Noul reports `None` by design.
    pub fn confidence(&self) -> Option<f64> {
        match self {
            Answer::Noul { .. } => None,
            Answer::Score { confidence, .. } | Answer::Choice { confidence, .. } => {
                Some(*confidence)
            }
        }
    }
}

/// Token counts the API reports for one request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// What one `system_one` call returned.
#[derive(Debug, Clone)]
pub struct Response {
    pub answers: HashMap<String, Answer>,
    pub usage: Usage,
    /// The exact model version that served the request (e.g. `jev-1.13.0`),
    /// which is what makes a run reproducible.
    pub model: String,
}

pub struct JevClient {
    pub base_url: String,
    pub model: String,
    transport: Transport,
    pub calls: usize,
    pub failures: usize,
}

impl JevClient {
    /// HTTP transport against the TypeSafe API. Reads `TYPESAFE_API_KEY`.
    pub fn new(model: &str) -> Self {
        Self::with_base("https://api.typesafe.ai", model)
    }

    /// Same, against an explicit base URL (staging, a proxy, a fake).
    pub fn with_base(base_url: &str, model: &str) -> Self {
        let base = base_url.trim_end_matches('/').to_string();
        let url = format!("{base}/v1/systemone");
        let api_key = std::env::var("TYPESAFE_API_KEY").ok();
        JevClient {
            base_url: base,
            model: model.to_string(),
            calls: 0,
            failures: 0,
            transport: Box::new(move |body: &str| {
                let mut req = ureq::post(&url)
                    .set("Content-Type", "application/json")
                    .timeout(Duration::from_secs(180));
                if let Some(key) = &api_key {
                    req = req.set("Authorization", &format!("Bearer {key}"));
                }
                let saved = req.clone();
                match req.send_string(body) {
                    Ok(resp) => resp.into_string().map_err(|e| format!("read: {e}")),
                    // rate limited: back off once, then retry the saved request
                    Err(ureq::Error::Status(429, resp)) => {
                        let _ = resp.into_string();
                        std::thread::sleep(Duration::from_secs(20));
                        saved
                            .send_string(body)
                            .map_err(|e| format!("http retry: {e}"))?
                            .into_string()
                            .map_err(|e| format!("read: {e}"))
                    }
                    Err(ureq::Error::Status(code, resp)) => {
                        let body = resp
                            .into_string()
                            .unwrap_or_else(|_| String::from("<no body>"));
                        let hint: String = body.chars().take(300).collect();
                        Err(format!("http {code}: {hint}"))
                    }
                    Err(e) => Err(format!("http: {e}")),
                }
            }),
        }
    }

    /// Test transport: no network, canned response.
    pub fn with_transport(transport: Transport, model: &str) -> Self {
        JevClient {
            base_url: String::new(),
            model: model.to_string(),
            transport,
            calls: 0,
            failures: 0,
        }
    }

    /// One request: every question is evaluated in parallel and in isolation
    /// against the same state.
    ///
    /// Keep `state` to what the questions need. Accuracy falls as the state
    /// grows with unrelated content, and questions that each carry their own
    /// private state must NOT be batched into one call — measured on
    /// relation-alignment pairs, batching moved scores by up to 0.73 and
    /// changed the routing. Batch only when the questions share one state.
    pub fn ask(
        &mut self,
        state: &serde_json::Value,
        questions: &[(&str, Question)],
    ) -> Result<Response, String> {
        let qs: serde_json::Map<String, serde_json::Value> = questions
            .iter()
            .map(|(id, q)| (id.to_string(), q.to_json()))
            .collect();
        let body = serde_json::json!({
            "state": state,
            "model": self.model,
            "questions": qs,
        })
        .to_string();
        self.calls += 1;
        let raw = (self.transport)(&body).inspect_err(|_| {
            self.failures += 1;
        })?;
        parse_response(&raw).inspect_err(|_| {
            self.failures += 1;
        })
    }
}

/// Parse the wire form. Kept separate from the transport so the contract is
/// testable without a network.
pub fn parse_response(raw: &str) -> Result<Response, String> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("bad json: {e}"))?;
    let obj = v
        .get("answers")
        .and_then(|a| a.as_object())
        .ok_or_else(|| format!("no answers in response: {raw}"))?;
    let mut answers = HashMap::new();
    for (id, a) in obj {
        let kind = a.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let ans = match kind {
            "noul" => {
                let value = num(a, "noul")?;
                if !(0.0..=1.0).contains(&value) {
                    return Err(format!("noul out of range in answer `{id}`: {value}"));
                }
                Answer::Noul { noul: value }
            }
            "score" => {
                let score = num(a, "score")?;
                let confidence = num(a, "confidence")?;
                if !(0.0..=1.0).contains(&confidence) {
                    return Err(format!(
                        "confidence out of range in answer `{id}`: {confidence}"
                    ));
                }
                let probabilities = int_map(a, "probabilities")?;
                Answer::Score { score, confidence, probabilities }
            }
            "choice" => {
                let confidence = num(a, "confidence")?;
                if !(0.0..=1.0).contains(&confidence) {
                    return Err(format!(
                        "confidence out of range in answer `{id}`: {confidence}"
                    ));
                }
                let choice = a
                    .get("choice")
                    .and_then(|c| c.as_str())
                    .ok_or_else(|| format!("missing choice in answer `{id}`"))?
                    .to_string();
                Answer::Choice {
                    choice,
                    confidence,
                    probabilities: str_map(a, "probabilities")?,
                }
            },
            // an answer kind a future API adds is skipped, not fatal
            _ => continue,
        };
        answers.insert(id.clone(), ans);
    }
    let usage = v.get("usage");
    Ok(Response {
        answers,
        usage: Usage {
            input_tokens: usage
                .and_then(|u| u.get("input_tokens"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0),
            output_tokens: usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0),
        },
        model: v
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

fn num(v: &serde_json::Value, key: &str) -> Result<f64, String> {
    let n = v
        .get(key)
        .and_then(|n| n.as_f64())
        .ok_or_else(|| format!("missing numeric `{key}` in answer: {v}"))?;
    if n.is_finite() {
        Ok(n)
    } else {
        Err(format!("non-finite numeric `{key}` in answer: {v}"))
    }
}

fn int_map(v: &serde_json::Value, key: &str) -> Result<Vec<(i64, f64)>, String> {
    let mut out: Vec<(i64, f64)> = v
        .get(key)
        .and_then(|m| m.as_object())
        .ok_or_else(|| format!("missing probability map `{key}` in answer: {v}"))?
        .iter()
        .map(|(k, p)| {
            let level = k
                .parse::<i64>()
                .map_err(|_| format!("invalid probability level `{k}`"))?;
            let probability = p
                .as_f64()
                .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
                .ok_or_else(|| format!("invalid probability for level `{k}`"))?;
            Ok((level, probability))
        })
        .collect::<Result<_, String>>()?;
    if out.is_empty() {
        return Err(format!("empty probability map `{key}` in answer: {v}"));
    }
    out.sort_by_key(|(k, _)| *k);
    Ok(out)
}

fn str_map(v: &serde_json::Value, key: &str) -> Result<Vec<(String, f64)>, String> {
    let mut out: Vec<(String, f64)> = v
        .get(key)
        .and_then(|m| m.as_object())
        .ok_or_else(|| format!("missing probability map `{key}` in answer: {v}"))?
        .iter()
        .map(|(k, p)| {
            let probability = p
                .as_f64()
                .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
                .ok_or_else(|| format!("invalid probability for option `{k}`"))?;
            Ok((k.clone(), probability))
        })
        .collect::<Result<_, String>>()?;
    if out.is_empty() {
        return Err(format!("empty probability map `{key}` in answer: {v}"));
    }
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}
