//! `DecisionEngine` backed by any OpenAI-compatible chat-completions
//! endpoint (Gemini's `v1beta/openai`, OpenAI itself, a local
//! `llama.cpp` server, …). The model never touches policy: it only
//! picks among the *already-generated* candidates or takes a route —
//! and anything it can't express honestly decays to `Abstain`, never
//! to an invented action.

use std::io::Read;
use std::time::Duration;

use serde_json::{json, Value};

use crate::{
    CandidateAction, Decision, DecisionContext, DecisionEngine, DecisionError, EngineHealth, Route,
};

/// Cap on candidates sent to the model — keeps prompt tokens (and
/// spend) bounded; the generator already ranks best-first.
const MAX_CANDIDATES: usize = 8;
/// Cap on the world digest included in the prompt.
const MAX_DIGEST_CHARS: usize = 1500;
/// Hard ceiling on the completion — the contract is a tiny JSON
/// object, so anything longer is waste.
const MAX_COMPLETION_TOKENS: u64 = 150;

/// OpenAI-compatible chat-completions decision engine.
///
/// Config: `base_url` (e.g.
/// `https://generativelanguage.googleapis.com/v1beta/openai`),
/// `model`, and `api_key_env` — the *name* of the env var holding the
/// key, read at request time and never logged.
pub struct OpenAiProvider {
    base_url: String,
    model: String,
    api_key_env: String,
    display_name: String,
    agent: ureq::Agent,
}

impl OpenAiProvider {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key_env: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        let model = model.into();
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .build()
            .new_agent();
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            display_name: format!("openai:{model}"),
            model,
            api_key_env: api_key_env.into(),
            agent,
        }
    }

    fn render_prompt(ctx: &DecisionContext) -> Vec<Value> {
        let mut digest = ctx.state_digest.clone();
        if digest.len() > MAX_DIGEST_CHARS {
            let mut cut = MAX_DIGEST_CHARS;
            while !digest.is_char_boundary(cut) {
                cut -= 1;
            }
            digest.truncate(cut);
            digest.push('…');
        }
        let cands: Vec<Value> = ctx
            .candidates
            .iter()
            .take(MAX_CANDIDATES)
            .enumerate()
            .map(|(i, c)| {
                json!({
                    "index": i,
                    "action": c.action,
                    "prior": c.prior,
                })
            })
            .collect();
        let system = "You are a decision engine inside a computer-use \
                      runtime. You are given a goal, a digest of the \
                      screen, and a ranked list of candidate actions. \
                      Reply with ONLY a JSON object, no prose: either \
                      {\"act\": <candidate index>} to run that \
                      candidate, or {\"route\": \"abstain\" | \
                      \"retry\" | \"reobserve\" | {\"wait\": <ms>}} \
                      when no candidate moves toward the goal or the \
                      world needs another look. Optionally add \
                      \"why\": <one short sentence>.";
        let user = json!({
            "goal": ctx.goal,
            "step": ctx.step,
            "last_error": ctx.last_error,
            "screen": digest,
            "candidates": cands,
        });
        vec![
            json!({"role": "system", "content": system}),
            json!({"role": "user", "content": user.to_string()}),
        ]
    }

    /// Map the model's JSON reply into a `Decision`. Anything
    /// incoherent — bad JSON, an out-of-range index, an unknown route —
    /// decays to `Route::Abstain`: the model produced an answer we
    /// can't act on, and abstaining is the honest fail-closed move.
    fn interpret(content: &str, candidates: &[CandidateAction]) -> Decision {
        let trimmed = content
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        // Some models wrap the object in prose — salvage the first
        // balanced {...} block if the whole body doesn't parse.
        let parsed: Option<Value> = serde_json::from_str(trimmed).ok().or_else(|| {
            let start = trimmed.find('{')?;
            let mut depth = 0i32;
            let mut end = None;
            for (i, ch) in trimmed.char_indices() {
                if i < start {
                    continue;
                }
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i + ch.len_utf8());
                            break;
                        }
                    }
                    _ => {}
                }
            }
            serde_json::from_str(trimmed.get(start..end?).unwrap_or("")).ok()
        });
        let parsed = match parsed {
            Some(v) => v,
            None => return abstain("unparseable model reply".into()),
        };

        let why = parsed["why"].as_str().unwrap_or("model route").to_string();
        if let Some(idx) = parsed["act"].as_u64() {
            let idx = idx as usize;
            return match candidates.get(idx) {
                Some(c) => Decision::Act {
                    action: c.action.clone(),
                    candidate_index: Some(idx),
                    rationale: why,
                },
                // Pointing at a candidate that doesn't exist is an
                // incoherent answer — abstain, never invent an action.
                None => abstain(format!("invalid candidate index {idx}")),
            };
        }
        let route = match &parsed["route"] {
            Value::String(s) if s == "abstain" => Route::Abstain,
            Value::String(s) if s == "retry" => Route::Retry,
            Value::String(s) if s == "reobserve" => Route::Reobserve,
            Value::Object(o) => match o["wait"].as_u64() {
                Some(ms) if ms <= 30_000 => Route::Wait { millis: ms },
                _ => return abstain("invalid wait route".into()),
            },
            _ => return abstain("unknown route".into()),
        };
        Decision::Route {
            route,
            rationale: why,
        }
    }
}

fn abstain(why: String) -> Decision {
    Decision::Route {
        route: Route::Abstain,
        rationale: why,
    }
}

impl DecisionEngine for OpenAiProvider {
    fn name(&self) -> &str {
        &self.display_name
    }

    fn decide(&self, ctx: &DecisionContext) -> Result<Decision, DecisionError> {
        let key = std::env::var(&self.api_key_env).map_err(|_| DecisionError::Engine {
            engine: self.display_name.clone(),
            message: format!("api key env var '{}' is not set", self.api_key_env),
        })?;
        let body = json!({
            "model": self.model,
            "messages": Self::render_prompt(ctx),
            "temperature": 0,
            "max_tokens": MAX_COMPLETION_TOKENS,
        });
        let resp = self
            .agent
            .post(format!("{}/chat/completions", self.base_url))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {key}"))
            .send(serde_json::to_vec(&body).unwrap_or_default())
            .map_err(|e| DecisionError::Engine {
                engine: self.display_name.clone(),
                message: format!("transport: {e}"),
            })?;
        let status = resp.status().as_u16();
        let mut text = String::new();
        resp.into_body()
            .as_reader()
            .read_to_string(&mut text)
            .map_err(|e| DecisionError::Engine {
                engine: self.display_name.clone(),
                message: format!("response read: {e}"),
            })?;
        let body: Value = serde_json::from_str(&text).map_err(|e| DecisionError::Engine {
            engine: self.display_name.clone(),
            message: format!("response not json: {e}"),
        })?;
        if !(200..300).contains(&status) {
            let msg = body["error"]["message"]
                .as_str()
                .unwrap_or("unknown api error");
            return Err(DecisionError::Engine {
                engine: self.display_name.clone(),
                message: format!("HTTP {status}: {msg}"),
            });
        }
        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");
        if content.is_empty() {
            return Err(DecisionError::Engine {
                engine: self.display_name.clone(),
                message: format!("no completion in response: {body}"),
            });
        }
        Ok(Self::interpret(content, &ctx.candidates))
    }

    fn health(&self) -> EngineHealth {
        // Cheap liveness: a live probe would burn tokens; the env var
        // check catches the only deploy-time misconfig that matters.
        if std::env::var(&self.api_key_env).is_ok() {
            EngineHealth::Ready
        } else {
            EngineHealth::Down(format!("api key env var '{}' is not set", self.api_key_env))
        }
    }
}
