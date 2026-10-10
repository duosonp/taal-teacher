//! Taal Teacher server: serves the grammar-teacher page and answers its Claude requests.
//!
//! Visitors get a free daily token allowance (per browser and per address, plus a global
//! cap for the whole site). Beyond that, the page lets them use their own Anthropic API
//! key, which the browser sends straight to Anthropic, never through this server.
//!
//! Subcommands (no arguments = serve):
//!   validate            check data/lessons.json and the prompt block, exit 1 on errors
//!   stats               print lesson counts per language
//!   build [--out PATH]  write the complete page (default dist/index.html), e.g. to publish
//!                       it as a Claude artifact or open it straight from disk

mod anthropic;
mod lessons;
mod prompts;
mod quota;

use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use anthropic::{CallError, Claude};
use prompts::{Prompts, VarError};
use quota::{RateLimit, Store, today};

const PAGE: &str = include_str!("../web/index.html");
const LESSONS: &str = include_str!("../data/lessons.json");
const LESSONS_SLOT: &str = "__LESSONS__";

/// The page file has no document skeleton (Claude adds one when it's published as an artifact),
/// so the server wraps it, and fills in the curated lessons.
fn document() -> Result<String, String> {
    let lib = lessons::Library::parse(LESSONS)?;
    let report = lib.validate();
    if !report.errors.is_empty() {
        return Err(format!("data/lessons.json has errors:\n  {}", report.errors.join("\n  ")));
    }
    if PAGE.matches(LESSONS_SLOT).count() != 1 {
        return Err(format!("web/index.html must contain {LESSONS_SLOT} exactly once"));
    }
    let page = PAGE.replace(LESSONS_SLOT, &lessons::embeddable(LESSONS)?);
    Ok(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\
         <meta name=\"theme-color\" content=\"#f6f3ec\" media=\"(prefers-color-scheme: light)\">\
         <meta name=\"theme-color\" content=\"#17181c\" media=\"(prefers-color-scheme: dark)\">\
         <meta name=\"description\" content=\"A grammar teacher for any language: explains, compares and drills grammatical features.\">\
         </head><body>{page}</body></html>"
    ))
}

/// Runs a subcommand if one was given. Returns false when the server should start.
fn command(args: &[String]) -> bool {
    let Some(cmd) = args.first() else { return false };
    let fail = |msg: String| -> ! {
        eprintln!("error: {msg}");
        std::process::exit(1)
    };
    match cmd.as_str() {
        "validate" | "stats" => {
            let lib = lessons::Library::parse(LESSONS).unwrap_or_else(|e| fail(e));
            if cmd == "stats" {
                print!("{}", lib.stats());
                return true;
            }
            let r = lib.validate();
            for w in &r.warnings {
                eprintln!("warning: {w}");
            }
            for e in &r.errors {
                eprintln!("error: {e}");
            }
            if let Err(e) = prompts::Prompts::from_page(PAGE) {
                fail(e);
            }
            if !r.errors.is_empty() {
                std::process::exit(1);
            }
            println!("ok: {} lessons, {} warnings", lib.lessons.len(), r.warnings.len());
        }
        "build" => {
            let out = match args.get(1).map(String::as_str) {
                Some("--out") => args.get(2).cloned().unwrap_or_else(|| fail("--out needs a path".into())),
                Some(other) => fail(format!("unknown flag {other}")),
                None => "dist/index.html".into(),
            };
            let doc = document().unwrap_or_else(|e| fail(e));
            if let Some(dir) = std::path::Path::new(&out).parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).unwrap_or_else(|e| fail(e.to_string()));
            }
            std::fs::write(&out, doc).unwrap_or_else(|e| fail(e.to_string()));
            println!("wrote {out}");
        }
        other => fail(format!("unknown command {other}; use validate, stats or build")),
    }
    true
}

const COOKIE: &str = "teacher_id";

struct Limits {
    per_client: u64,
    per_ip: u64,
    global: u64,
}

struct App {
    document: String,
    prompts: Prompts,
    claude: Claude,
    store: Store,
    limits: Limits,
    rate: RateLimit,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}
fn env_num(name: &str, default: u64) -> u64 {
    env_or(name, &default.to_string()).replace('_', "").parse().unwrap_or(default)
}

#[cfg(test)]
mod tests {
    #[test]
    fn page_is_served_whole_with_lessons_filled_in() {
        let doc = super::document().unwrap();
        assert!(doc.starts_with("<!doctype html>"));
        assert!(doc.contains("width=device-width"));
        assert!(doc.contains("id=\"teacher-prompts\""));
        assert!(!doc.contains(super::LESSONS_SLOT));
        assert!(doc.contains("\"ro-demonstratives\""));
        assert!(doc.trim_end().ends_with("</body></html>"));
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if command(&args) {
        return;
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let key = std::env::var("ANTHROPIC_API_KEY").expect("set ANTHROPIC_API_KEY");
    let app = Arc::new(App {
        document: document().expect("page"),
        prompts: Prompts::from_page(PAGE).expect("prompt templates"),
        claude: Claude::new(
            key,
            env_or("TEACHER_MODEL", "claude-sonnet-5-5"),
            env_or("ANTHROPIC_BASE_URL", "https://api.anthropic.com"),
        ),
        store: Store::open(&env_or("TEACHER_DB", "teacher.db")).expect("open quota database"),
        limits: Limits {
            per_client: env_num("TEACHER_DAILY_PER_CLIENT", 60_000),
            per_ip: env_num("TEACHER_DAILY_PER_IP", 150_000),
            global: env_num("TEACHER_DAILY_GLOBAL", 2_000_000),
        },
        rate: RateLimit::new(env_num("TEACHER_REQUESTS_PER_MINUTE", 20) as usize, Duration::from_secs(60)),
    });
    tracing::info!(
        model = app.claude.model(),
        kinds = ?app.prompts.kinds(),
        per_client = app.limits.per_client,
        per_ip = app.limits.per_ip,
        global = app.limits.global,
        "taal teacher ready"
    );

    let router = Router::new()
        .route("/", get(page))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/config", get(config))
        .route("/api/ask", post(ask))
        .layer(DefaultBodyLimit::max(48 * 1024))
        .with_state(app);

    let port = env_or("PORT", "8080");
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await.expect("bind");
    tracing::info!("listening on :{port}");
    axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server");
}

/* ---------- visitor identity ---------- */

struct Visitor {
    id: String,
    is_new: bool,
    ip: String,
    https: bool,
}

fn visitor(headers: &HeaderMap, addr: SocketAddr) -> Visitor {
    let cookie = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().strip_prefix(COOKIE)?.strip_prefix('='))
        .find(|v| v.len() == 32 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_string);
    let header_str = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim);
    // Fly's edge sets Fly-Client-IP; fall back to the first forwarded address, then the socket.
    let ip = header_str("fly-client-ip")
        .or_else(|| header_str("x-forwarded-for").and_then(|v| v.split(',').next()).map(str::trim))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| addr.ip().to_string());
    let https = header_str("x-forwarded-proto") == Some("https");
    match cookie {
        Some(id) => Visitor { id, is_new: false, ip, https },
        None => Visitor { id: format!("{:032x}", rand::random::<u128>()), is_new: true, ip, https },
    }
}

fn with_cookie(mut res: Response, v: &Visitor) -> Response {
    if v.is_new {
        let secure = if v.https { "; Secure" } else { "" };
        let c = format!("{COOKIE}={}; Path=/; Max-Age=31536000; HttpOnly; SameSite=Lax{secure}", v.id);
        if let Ok(h) = HeaderValue::from_str(&c) {
            res.headers_mut().append(header::SET_COOKIE, h);
        }
    }
    res
}

fn quota_json(app: &App, v: &Visitor) -> Value {
    let (_, resets_in) = today();
    json!({
        "used": app.store.used(&format!("c:{}", v.id)),
        "limit": app.limits.per_client,
        "resetsIn": resets_in,
    })
}

fn error(status: StatusCode, code: &str, message: &str, quota: Option<Value>) -> Response {
    (status, Json(json!({ "code": code, "message": message, "quota": quota }))).into_response()
}

/* ---------- routes ---------- */

async fn page(State(app): State<Arc<App>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    let v = visitor(&headers, addr);
    let mut res = app.document.clone().into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
             font-src https://fonts.gstatic.com; connect-src 'self' https://api.anthropic.com; img-src 'self' data:; \
             base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ),
    );
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    with_cookie(res, &v)
}

async fn config(State(app): State<Arc<App>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    let v = visitor(&headers, addr);
    let body = json!({ "mode": "hosted", "model": app.claude.model(), "quota": quota_json(&app, &v) });
    with_cookie(Json(body).into_response(), &v)
}

#[derive(Deserialize)]
struct AskBody {
    kind: String,
    #[serde(default)]
    vars: HashMap<String, String>,
}

async fn ask(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<AskBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let v = visitor(&headers, addr);
    let res = answer(&app, &v, &headers, body).await;
    with_cookie(res, &v)
}

async fn answer(
    app: &App,
    v: &Visitor,
    headers: &HeaderMap,
    body: Result<Json<AskBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    // Only the page sends this header. Cross-site forms can't, and we never allow CORS.
    if headers.get("x-teacher").and_then(|h| h.to_str().ok()) != Some("1") {
        return error(StatusCode::FORBIDDEN, "forbidden", "Requests must come from the Taal Teacher page.", None);
    }
    let Ok(Json(body)) = body else {
        return error(StatusCode::BAD_REQUEST, "bad_request", "Malformed request.", None);
    };
    let Some(template) = app.prompts.get(&body.kind) else {
        return error(StatusCode::BAD_REQUEST, "bad_request", "Unknown request kind.", None);
    };
    let prompt = match template.fill(&body.vars) {
        Ok(p) => p,
        Err(VarError::Unknown(_)) => return error(StatusCode::BAD_REQUEST, "bad_request", "Unexpected field.", None),
        Err(VarError::TooLong(_) | VarError::TooLarge) => {
            return error(StatusCode::PAYLOAD_TOO_LARGE, "prompt_too_large", "The text is too long to send.", None);
        }
    };

    let ip_key = app.store.ip_key(&v.ip);
    if !app.rate.allow(&ip_key) {
        return error(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Too many requests. Wait a moment.", None);
    }

    let client_key = format!("c:{}", v.id);
    let quota = || Some(quota_json(app, v));

    // Shared answers (lessons, topic suggestions) are served from the cache for free.
    let answer_key = template.shared.then(|| Store::answer_key(&body.kind, &prompt));
    if let Some(data) = answer_key.as_deref().and_then(|k| app.store.cached_answer(k))
        && let Ok(data) = serde_json::from_str::<Value>(&data)
    {
        return Json(json!({ "data": data, "usage": { "input": 0, "output": 0 }, "quota": quota_json(app, v), "cached": true }))
            .into_response();
    }
    if app.store.used("global") >= app.limits.global {
        return error(StatusCode::SERVICE_UNAVAILABLE, "global_quota", "The free service has reached today's limit.", quota());
    }
    if app.store.used(&client_key) >= app.limits.per_client || app.store.used(&ip_key) >= app.limits.per_ip {
        return error(StatusCode::TOO_MANY_REQUESTS, "quota", "Today's free allowance is used up.", quota());
    }

    let keys = [client_key.as_str(), ip_key.as_str(), "global"];
    match app.claude.ask_json(&prompt, template.max_tokens).await {
        Ok(a) => {
            app.store.add(&keys, a.input_tokens + a.output_tokens);
            if let Some(k) = &answer_key {
                app.store.store_answer(k, &a.data.to_string());
            }
            tracing::info!(kind = body.kind, input = a.input_tokens, output = a.output_tokens, "answered");
            Json(json!({
                "data": a.data,
                "usage": { "input": a.input_tokens, "output": a.output_tokens },
                "quota": quota_json(app, v),
            }))
            .into_response()
        }
        Err(CallError::InvalidJson { input_tokens, output_tokens }) => {
            app.store.add(&keys, input_tokens + output_tokens);
            error(StatusCode::BAD_GATEWAY, "invalid_json", "Claude's answer came back garbled.", quota())
        }
        Err(CallError::Busy) => error(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Claude is busy. Try again shortly.", quota()),
        Err(CallError::Upstream(msg)) => {
            tracing::warn!(kind = body.kind, "upstream error: {msg}");
            error(StatusCode::BAD_GATEWAY, "upstream", "Claude had a problem answering.", quota())
        }
    }
}
