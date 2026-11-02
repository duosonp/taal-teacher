//! Curated lessons: the canonical, hand-checked part of Taal Teacher.
//!
//! `data/lessons.json` holds lessons in exactly the shape the page assembles from Claude's
//! `lesson_*` prompts, plus an id, the date added, practice items and links to other lessons (the
//! word-trail idea, applied to grammar features). They are embedded in the page at build
//! time, so the library works with no Claude connection at all. `validate` refuses a deploy
//! with broken data, the same way word-trail's tool guards its graph.

// Every field is deserialized (with deny_unknown_fields) so the schema is checked, even the
// ones only the page reads.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Library {
    pub version: u32,
    pub updated: String,
    pub languages: HashMap<String, String>,
    pub lessons: Vec<Lesson>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lesson {
    pub id: String,
    pub feature: String,
    pub langs: Vec<String>,
    pub level: String,
    pub added: String,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub matrix: Option<Table>,
    pub sections: Vec<Section>,
    #[serde(default)]
    pub contrasts: Vec<Contrast>,
    #[serde(default)]
    pub mnemonic: String,
    #[serde(default)]
    pub related: Vec<Related>,
    #[serde(default)]
    pub drill: Vec<Drill>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub lang: String,
    pub name: String,
    pub overview: String,
    #[serde(default)]
    pub tables: Vec<Table>,
    #[serde(default)]
    pub examples: Vec<Example>,
    #[serde(default)]
    pub pitfalls: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    pub title: String,
    pub cols: Vec<String>,
    pub rows: Vec<Row>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub label: String,
    pub cells: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Example {
    pub words: Vec<Gloss>,
    pub translation: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gloss {
    pub w: String,
    pub g: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contrast {
    pub point: String,
    pub detail: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Related {
    #[serde(default)]
    pub id: Option<String>,
    pub feature: String,
    #[serde(default)]
    pub langs: Vec<String>,
    pub why: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Drill {
    pub q: String,
    pub lang: String,
    #[serde(default)]
    pub options: Vec<String>,
    pub answer: String,
    #[serde(default)]
    pub accept: Vec<String>,
    pub explain: String,
}

pub struct Report {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

const LEVELS: [&str; 3] = ["beginner", "intermediate", "advanced"];

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn is_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn check_table(t: &Table, at: &str, errors: &mut Vec<String>) {
    if t.cols.is_empty() || t.rows.is_empty() {
        errors.push(format!("{at}: table \"{}\" has no columns or no rows", t.title));
    }
    for r in &t.rows {
        if r.cells.len() != t.cols.len() {
            errors.push(format!(
                "{at}: table \"{}\" row \"{}\" has {} cells for {} columns",
                t.title,
                r.label,
                r.cells.len(),
                t.cols.len()
            ));
        }
    }
}

impl Library {
    pub fn parse(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("data/lessons.json: {e}"))
    }

    pub fn validate(&self) -> Report {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        if !is_date(&self.updated) {
            errors.push(format!("top-level updated \"{}\" is not YYYY-MM-DD", self.updated));
        }
        let ids: HashSet<&str> = self.lessons.iter().map(|l| l.id.as_str()).collect();
        if ids.len() != self.lessons.len() {
            let mut seen = HashSet::new();
            for l in &self.lessons {
                if !seen.insert(&l.id) {
                    errors.push(format!("duplicate lesson id {}", l.id));
                }
            }
        }
        let mut linked: HashSet<&str> = HashSet::new();
        for l in &self.lessons {
            let at = l.id.as_str();
            if !is_id(at) {
                errors.push(format!("{at}: id must be lowercase letters, digits and dashes"));
            }
            if !is_date(&l.added) {
                errors.push(format!("{at}: added \"{}\" is not YYYY-MM-DD", l.added));
            }
            if !LEVELS.contains(&l.level.as_str()) {
                errors.push(format!("{at}: level \"{}\" is not one of {LEVELS:?}", l.level));
            }
            if l.title.trim().is_empty() || l.summary.trim().is_empty() || l.feature.trim().is_empty() {
                errors.push(format!("{at}: title, summary and feature are required"));
            }
            if l.langs.is_empty() {
                errors.push(format!("{at}: no languages"));
            }
            for code in &l.langs {
                if !self.languages.contains_key(code) {
                    errors.push(format!("{at}: unknown language {code}"));
                }
            }
            let section_langs: Vec<&str> = l.sections.iter().map(|s| s.lang.as_str()).collect();
            for code in &l.langs {
                if !section_langs.contains(&code.as_str()) {
                    errors.push(format!("{at}: no section for {code}"));
                }
            }
            for s in &l.sections {
                if !l.langs.contains(&s.lang) {
                    errors.push(format!("{at}: section for {} which is not in langs", s.lang));
                }
                if let Some(name) = self.languages.get(&s.lang)
                    && name != &s.name
                {
                    errors.push(format!("{at}: section name \"{}\" should be \"{name}\"", s.name));
                }
                for t in &s.tables {
                    check_table(t, at, &mut errors);
                }
                for e in &s.examples {
                    if e.words.is_empty() || e.translation.trim().is_empty() {
                        errors.push(format!("{at}: an example in {} has no words or no translation", s.lang));
                    }
                    if e.words.iter().any(|g| g.w.trim().is_empty() || g.g.trim().is_empty()) {
                        errors.push(format!("{at}: an example in {} has a word without its gloss", s.lang));
                    }
                }
                if s.examples.is_empty() {
                    warnings.push(format!("{at}: section {} has no examples", s.lang));
                }
            }
            match (&l.matrix, l.langs.len() > 1) {
                (Some(m), true) => {
                    check_table(m, at, &mut errors);
                    if m.cols.len() != l.langs.len() {
                        errors.push(format!("{at}: comparison matrix needs one column per language"));
                    }
                }
                (None, true) => errors.push(format!("{at}: a comparison lesson needs a matrix")),
                (Some(_), false) => warnings.push(format!("{at}: a one-language lesson has a matrix")),
                (None, false) => {}
            }
            for r in &l.related {
                match &r.id {
                    Some(id) if !ids.contains(id.as_str()) => {
                        errors.push(format!("{at}: related lesson {id} does not exist"))
                    }
                    Some(id) if id == &l.id => errors.push(format!("{at}: links to itself")),
                    Some(id) => {
                        linked.insert(at);
                        linked.insert(id.as_str());
                    }
                    None => {}
                }
            }
            for d in &l.drill {
                if !l.langs.contains(&d.lang) {
                    errors.push(format!("{at}: drill item in {} which is not in langs", d.lang));
                }
                if !d.options.is_empty() && !d.options.contains(&d.answer) {
                    errors.push(format!("{at}: drill answer \"{}\" is not among its options", d.answer));
                }
                if d.q.trim().is_empty() || d.answer.trim().is_empty() || d.explain.trim().is_empty() {
                    errors.push(format!("{at}: drill items need q, answer and explain"));
                }
            }
            if l.drill.len() < 3 {
                warnings.push(format!("{at}: fewer than 3 practice items"));
            }
        }
        for l in &self.lessons {
            if !linked.contains(l.id.as_str()) {
                errors.push(format!("{}: not linked to or from any other lesson", l.id));
            }
        }
        Report { errors, warnings }
    }

    pub fn stats(&self) -> String {
        let mut by_lang: Vec<(String, usize)> = self
            .languages
            .iter()
            .map(|(code, name)| {
                (format!("{name} ({code})"), self.lessons.iter().filter(|l| l.langs.contains(code)).count())
            })
            .collect();
        by_lang.sort();
        let drills: usize = self.lessons.iter().map(|l| l.drill.len()).sum();
        let examples: usize = self.lessons.iter().flat_map(|l| &l.sections).map(|s| s.examples.len()).sum();
        let mut out = format!(
            "{} lessons, {} examples, {} practice items (updated {})\n",
            self.lessons.len(),
            examples,
            drills,
            self.updated
        );
        for (name, n) in by_lang {
            out.push_str(&format!("  {name}: {n}\n"));
        }
        out
    }
}

/// The library as compact JSON that is safe inside a `<script>` element.
pub fn embeddable(json: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(v.to_string().replace("</", "<\\/").replace("<!--", "<\\!--"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_library_is_valid() {
        let lib = Library::parse(include_str!("../data/lessons.json")).unwrap();
        let r = lib.validate();
        assert!(r.errors.is_empty(), "{:#?}", r.errors);
        assert!(lib.lessons.len() >= 5);
    }

    #[test]
    fn catches_broken_tables_and_links() {
        let json = r#"{"version":1,"updated":"2026-10-10","languages":{"ro":"Romanian"},"lessons":[
          {"id":"a","feature":"x","langs":["ro"],"level":"beginner","added":"2026-10-10","title":"t","summary":"s",
           "sections":[{"lang":"ro","name":"Romanian","overview":"o",
             "tables":[{"title":"T","cols":["sg","pl"],"rows":[{"label":"m","cells":["acest"]}]}],
             "examples":[{"words":[{"w":"acest","g":""}],"translation":"this"}]}],
           "related":[{"id":"nope","feature":"y","why":"z"}],
           "drill":[{"q":"?","lang":"ro","options":["a","b"],"answer":"c","explain":"e"}]}]}"#;
        let r = Library::parse(json).unwrap().validate();
        let all = r.errors.join("\n");
        assert!(all.contains("has 1 cells for 2 columns"));
        assert!(all.contains("word without its gloss"));
        assert!(all.contains("related lesson nope does not exist"));
        assert!(all.contains("not among its options"));
    }

    #[test]
    fn embedding_cannot_close_the_script() {
        let s = embeddable(r#"{"a":"</script><!--"}"#).unwrap();
        assert!(!s.contains("</script>") && !s.contains("<!--"));
    }
}
