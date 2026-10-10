//! Prompt templates.
//!
//! The templates live in one JSON block inside `web/index.html`, so the page (when it runs
//! inside Claude or with a visitor's own key) and this server fill exactly the same text.
//! The server only ever sends these templates to Claude: a client names a template and
//! supplies the values for its `{{placeholders}}`, nothing else.

use std::collections::HashMap;

use serde::Deserialize;

const OPEN_TAG: &str = r#"<script type="application/json" id="teacher-prompts">"#;

#[derive(Deserialize)]
struct Def {
    #[serde(rename = "maxTokens")]
    max_tokens: u32,
    text: Vec<String>,
    /// Answers depend only on the filled prompt and help everyone (lessons, topic suggestions),
    /// so the server caches them and serves repeats for free.
    #[serde(default)]
    shared: bool,
}

pub struct Template {
    pub max_tokens: u32,
    pub shared: bool,
    text: String,
    vars: Vec<String>,
}

pub struct Prompts(HashMap<String, Template>);

/// Why a request's values were refused.
#[derive(Debug, PartialEq)]
pub enum VarError {
    Unknown(String),
    TooLong(String),
    TooLarge,
}

/// Longest value accepted per placeholder, in characters. Generous for real use, small
/// enough that the endpoint is useless as a general-purpose Claude proxy.
fn var_cap(name: &str) -> usize {
    match name {
        "lesson_ctx" => 3500,
        "hist_block" => 3000,
        "avoid" => 1500,
        "focus_block" => 400,
        "question" => 500,
        "sentence" => 400,
        "langs" => 300,
        "feature" => 120,
        _ => 60,
    }
}
const TOTAL_CAP: usize = 9000;

impl Prompts {
    pub fn from_page(page: &str) -> Result<Self, String> {
        let start = page.find(OPEN_TAG).ok_or("prompt block not found in page")? + OPEN_TAG.len();
        let end = start + page[start..].find("</script>").ok_or("prompt block is not closed")?;
        let defs: HashMap<String, Def> =
            serde_json::from_str(&page[start..end]).map_err(|e| format!("prompt block: {e}"))?;
        Ok(Self(
            defs.into_iter()
                .map(|(kind, d)| {
                    let text = d.text.join("\n");
                    let vars = placeholders(&text);
                    (kind, Template { max_tokens: d.max_tokens, shared: d.shared, text, vars })
                })
                .collect(),
        ))
    }

    pub fn get(&self, kind: &str) -> Option<&Template> {
        self.0.get(kind)
    }

    pub fn kinds(&self) -> Vec<&str> {
        let mut k: Vec<&str> = self.0.keys().map(String::as_str).collect();
        k.sort_unstable();
        k
    }
}

impl Template {
    /// Checks the values and fills the template. Missing values become empty strings.
    pub fn fill(&self, vars: &HashMap<String, String>) -> Result<String, VarError> {
        let mut total = 0;
        for (name, value) in vars {
            if !self.vars.contains(name) {
                return Err(VarError::Unknown(name.clone()));
            }
            let n = value.chars().count();
            if n > var_cap(name) {
                return Err(VarError::TooLong(name.clone()));
            }
            total += n;
        }
        if total > TOTAL_CAP {
            return Err(VarError::TooLarge);
        }
        Ok(render(&self.text, |name| vars.get(name).map(String::as_str).unwrap_or("")))
    }
}

/// A placeholder is `{{name}}` with name made of ASCII letters, digits and `_`.
fn placeholder_at(s: &str) -> Option<(&str, usize)> {
    let rest = s.strip_prefix("{{")?;
    let close = rest.find("}}")?;
    let name = &rest[..close];
    (!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .then_some((name, close + 4))
}

/// Single pass, so a value that itself contains `{{x}}` is never expanded.
fn render<'a>(text: &str, value: impl Fn(&str) -> &'a str) -> String {
    let mut out = String::with_capacity(text.len() + 512);
    let mut rest = text;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        match placeholder_at(&rest[i..]) {
            Some((name, len)) => {
                out.push_str(value(name));
                rest = &rest[i + len..];
            }
            None => {
                out.push_str("{{");
                rest = &rest[i + 2..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn placeholders(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("{{") {
        match placeholder_at(&rest[i..]) {
            Some((name, len)) => {
                if !found.iter().any(|f| f == name) {
                    found.push(name.to_string());
                }
                rest = &rest[i + len..];
            }
            None => rest = &rest[i + 2..],
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn reads_the_real_page() {
        let p = Prompts::from_page(include_str!("../web/index.html")).unwrap();
        assert_eq!(p.kinds(), ["ask", "check", "drill", "lesson", "suggest"]);
        assert!(p.get("lesson").unwrap().shared && p.get("suggest").unwrap().shared);
        assert!(!p.get("ask").unwrap().shared && !p.get("drill").unwrap().shared);
        let out = p
            .get("lesson")
            .unwrap()
            .fill(&vars(&[("langs", "Romanian (ro)"), ("feature", "demonstratives"), ("level", "beginner"), ("base", "English")]))
            .unwrap();
        assert!(out.contains("demonstratives") && out.contains("Romanian (ro)"));
        assert!(!out.contains("{{"));
    }

    #[test]
    fn values_are_not_expanded_again() {
        let t = Template { max_tokens: 10, shared: false, text: "a {{x}} b".into(), vars: vec!["x".into()] };
        assert_eq!(t.fill(&vars(&[("x", "{{x}}")])).unwrap(), "a {{x}} b");
    }

    #[test]
    fn rejects_unknown_and_oversized_values() {
        let t = Template { max_tokens: 10, shared: false, text: "{{question}}".into(), vars: vec!["question".into()] };
        assert_eq!(t.fill(&vars(&[("nope", "1")])), Err(VarError::Unknown("nope".into())));
        assert_eq!(t.fill(&vars(&[("question", &"x".repeat(501))])), Err(VarError::TooLong("question".into())));
    }

    #[test]
    fn json_braces_are_left_alone() {
        assert_eq!(placeholders(r#"{"a":{"b":1}} {{ok}} {{not ok}}"#), ["ok"]);
    }
}
