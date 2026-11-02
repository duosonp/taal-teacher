# Taal Teacher

A grammar teacher for any language. Pick one or more languages and a grammatical feature
(demonstratives in Romanian, aspect in Polish and Russian, cases in Finnish and Hungarian…) and
get a lesson: paradigm tables, word-by-word glossed examples, typical mistakes, a side-by-side
comparison when there are several languages, and practice. Learners can have their own sentences
checked and ask the teacher follow-up questions.

It borrows from two sibling projects:

- **taal-writer**: the Rust server, the one-file page, the prompt block shared by page and server,
  the three ways of reaching Claude, daily token quotas, and forced-tool-call JSON answers.
- **word-trail**: a canonical, hand-checked data file validated at build time (a broken file fails
  the deploy), links between entries, a trail of what you visited, and a quick quiz.

## Layout

```
web/index.html          the page (no document skeleton; the server adds it)
data/lessons.json       curated, hand-checked lessons, embedded into the page at build time
src/main.rs             server + subcommands: validate, stats, build
src/lessons.rs          lesson schema and validation
src/prompts.rs          the prompt templates, read from the page's teacher-prompts block
src/anthropic.rs        Messages API client (forced "respond" tool call, retry when cut off)
src/quota.rs            daily token budgets and answer cache in SQLite, per-minute rate limit
```

## How Claude is reached

The page picks the first that is available:

1. **Inside Claude** (published as a Claude artifact): uses the viewer's own Claude plan.
2. **The visitor's own API key** (API key button): the browser calls Anthropic directly; the key
   never reaches this server.
3. **This server** (`/api/ask`): a free daily allowance, paid by your API key.

The server only sends the templates in the page with capped values, so it can't be used as a
general Claude proxy. A new lesson is made in small steps, so no single answer is long enough to be
cut off: `lesson_outline` (title, summary, mnemonic, related), then `lesson_section` once per
language, then `lesson_compare` (the side-by-side table, only with several languages, built from
the sections), then `lesson_drill`. The page shows the lesson after the first step and fills it in
as answers arrive; a language whose part fails gets a Try again button. `suggest`, `drill`,
`check` and `ask` are single requests. The `lesson_*` and `suggest` answers are cached and shared:
a lesson someone already asked for costs nothing the second time.

The curated library needs no Claude at all. Typing a feature and languages that match a library
lesson opens the library lesson instead of asking Claude.

## Limits

| Setting | Default | Meaning |
|---|---|---|
| `TEACHER_DAILY_PER_CLIENT` | 60000 | tokens per browser (random cookie id) per UTC day |
| `TEACHER_DAILY_PER_IP` | 150000 | per address (salted hash, never stored raw) |
| `TEACHER_DAILY_GLOBAL` | 1500000 | whole site; when reached, everyone is asked for a key |
| `TEACHER_REQUESTS_PER_MINUTE` | 20 | per address, in memory |
| `TEACHER_MODEL` | `claude-sonnet-5-5` | model used by the server |

A five-language comparison is the most expensive request (several thousand tokens). Set a monthly
spend limit in the Claude Console as the last line of defence.

## Run locally

```sh
cargo test
cargo run -- validate        # check data/lessons.json and the prompt block
cargo run -- stats
ANTHROPIC_API_KEY=sk-ant-... cargo run
# open http://localhost:8080
cargo run -- build           # dist/index.html: the complete page, e.g. to publish as an artifact
```

## Deploy to Fly.io

```sh
fly auth login
fly apps create taal-teacher            # pick another name if taken, and update fly.toml
fly volumes create teacher_data --region fra --size 1
fly secrets set ANTHROPIC_API_KEY=sk-ant-...
fly deploy --ha=false                   # one machine: the quota database lives on its volume
```

The Docker build runs the tests and `validate`, so a broken lesson file fails the deploy. The
machine stops when idle and starts on the next request. To deploy from GitHub on every push to
`main`, add a repo secret `FLY_API_TOKEN` (`fly tokens create deploy`); see `.github/workflows/ci.yml`.

## Adding curated lessons

Lessons in `data/lessons.json` have the same shape Claude returns for the `lesson` prompt, plus
`id`, `level`, `added` and practice items. `validate` checks:

- ids are unique and well-formed, dates are `YYYY-MM-DD`, levels are beginner/intermediate/advanced;
- every language is listed in `languages` and has a section, with the matching name;
- every table row has one cell per column; every example word has a gloss;
- a lesson with several languages has a comparison matrix with one column per language;
- `related` ids exist, and every lesson is linked to or from another one;
- multiple-choice answers are among their options.

As in word-trail, accuracy matters more than volume: only add forms and examples you are sure of.
