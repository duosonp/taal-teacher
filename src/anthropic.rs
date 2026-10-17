//! Minimal client for the Anthropic Messages API.
//!
//! Answers come back through a tool call ("respond", requested via the system prompt), so the API hands over a parsed
//! JSON object instead of free text: no code fences, no stray sentences, no broken quoting.
//! An answer cut off by the length limit is asked for once more with twice the room.

use std::time::Duration;

use serde_json::{Value, json};

pub struct Claude {
    http: reqwest::Client,
    key: String,
    model: String,
    base: String,
}

pub struct Answer {
    pub data: Value,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub enum CallError {
    /// Anthropic is rate limiting or overloaded.
    Busy,
    /// Claude answered but not with usable JSON. Tokens were still spent.
    InvalidJson { input_tokens: u64, output_tokens: u64 },
    Upstream(String),
}

/// The most room any single answer may get, even after a retry.
const MAX_TOKENS_CEILING: u32 = 8000;

impl Claude {
    pub fn new(key: String, model: String, base: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(90))
            .build()
            .expect("HTTP client");
        Self { http, key, model, base: base.trim_end_matches('/').to_string() }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub async fn ask_json(&self, prompt: &str, max_tokens: u32) -> Result<Answer, CallError> {
        let (mut input_tokens, mut output_tokens) = (0, 0);
        let mut room = max_tokens;
        for attempt in 0..2 {
            let reply = self.call(prompt, room).await?;
            input_tokens += reply.input_tokens;
            output_tokens += reply.output_tokens;
            match reply.data {
                Some(data) if !reply.cut_off => return Ok(Answer { data, input_tokens, output_tokens }),
                // A cut-off answer is retried once with twice the room; if there is no more
                // room to give, whatever parsed is better than nothing.
                Some(data) if attempt == 1 || room >= MAX_TOKENS_CEILING => {
                    return Ok(Answer { data, input_tokens, output_tokens });
                }
                _ => {}
            }
            if !reply.cut_off || room >= MAX_TOKENS_CEILING || attempt == 1 {
                tracing::warn!(stop = reply.stop_reason, start = reply.snippet, room, "answer was not usable JSON");
                break;
            }
            tracing::info!(room, "answer was cut off; asking again with more room");
            room = (room * 2).min(MAX_TOKENS_CEILING);
        }
        Err(CallError::InvalidJson { input_tokens, output_tokens })
    }

    async fn call(&self, prompt: &str, max_tokens: u32) -> Result<Reply, CallError> {
        let body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "tools": [{
                "name": "respond",
                "description": "Give your answer: a JSON object exactly as the instructions describe.",
                "input_schema": { "type": "object", "additionalProperties": true },
            }],
            // Sonnet 5.5 and newer reject forced tool use ("tool"/"any"), so let Claude choose and
            // ask for the tool in the system prompt; JSON written as text is still accepted below.
            "tool_choice": { "type": "auto" },
            "system": "Always answer by calling the respond tool exactly once, with the complete JSON object as its input.",
            "messages": [{ "role": "user", "content": prompt }],
        });
        let res = self
            .http
            .post(format!("{}/v1/messages", self.base))
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| CallError::Upstream(e.to_string()))?;

        let status = res.status();
        let v: Value = res.json().await.map_err(|e| CallError::Upstream(e.to_string()))?;
        if status.as_u16() == 429 || status.as_u16() == 529 {
            return Err(CallError::Busy);
        }
        if !status.is_success() {
            let msg = v["error"]["message"].as_str().unwrap_or("unknown error");
            return Err(CallError::Upstream(format!("{status}: {msg}")));
        }
        Ok(Reply::from_response(&v))
    }
}

struct Reply {
    data: Option<Value>,
    cut_off: bool,
    stop_reason: String,
    snippet: String,
    input_tokens: u64,
    output_tokens: u64,
}

impl Reply {
    fn from_response(v: &Value) -> Self {
        let parts = v["content"].as_array().cloned().unwrap_or_default();
        // Preferred: the forced tool call's input, already parsed by the API.
        let from_tool = parts
            .iter()
            .find(|p| p["type"] == "tool_use" && p["name"] == "respond")
            .map(|p| p["input"].clone())
            .filter(|input| input.as_object().is_some_and(|o| !o.is_empty()));
        // Fallback: JSON written as text.
        let text: String = parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect();
        let data = from_tool.or_else(|| extract_json(&text));
        let stop_reason = v["stop_reason"].as_str().unwrap_or("").to_string();
        Self {
            cut_off: stop_reason == "max_tokens",
            snippet: text.chars().take(200).collect(),
            data,
            stop_reason,
            input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
        }
    }
}

/// Finds the JSON object or array in Claude's answer, tolerating code fences or a stray
/// sentence around it.
pub fn extract_json(text: &str) -> Option<Value> {
    let start = text.find(['{', '['])?;
    let close = if text.as_bytes()[start] == b'{' { '}' } else { ']' };
    let end = text.rfind(close)?;
    (end > start).then(|| serde_json::from_str(&text[start..=end]).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fenced_json() {
        let v = extract_json("```json\n{\"a\": [1, 2]}\n```").unwrap();
        assert_eq!(v["a"][1], 2);
        assert_eq!(extract_json("Here: [1,2,3] done").unwrap()[2], 3);
        assert!(extract_json("no json here").is_none());
        assert!(extract_json("{broken").is_none());
    }

    #[test]
    fn reads_the_tool_call() {
        let r = Reply::from_response(&json!({
            "stop_reason": "tool_use",
            "content": [{ "type": "tool_use", "name": "respond", "input": { "reply": "„Ja“, sagte sie." } }],
            "usage": { "input_tokens": 10, "output_tokens": 5 },
        }));
        assert_eq!(r.data.unwrap()["reply"], "„Ja“, sagte sie.");
        assert!(!r.cut_off);
    }

    #[test]
    fn notices_a_cut_off_answer() {
        let r = Reply::from_response(&json!({
            "stop_reason": "max_tokens",
            "content": [{ "type": "tool_use", "name": "respond", "input": {} }],
            "usage": { "input_tokens": 10, "output_tokens": 4000 },
        }));
        assert!(r.cut_off && r.data.is_none());
    }
}
