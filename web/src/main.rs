//! HGhub web server: the desktop Rust core (api/net/media/session/lines/
//! logging/network_policy/playback_log, included verbatim via `#[path]`)
//! behind an axum HTTP service with a password gate.
//!
//! * Frontend calls go to `POST /api/<command>` with a JSON args object.
//! * Media goes to `GET /hg/img|hls|seg?u=<url>` (replaces the `hg://` scheme).
//! * Everything except `/login`, `/api/web-login` and `/api/web-status`
//!   requires the gate cookie. No login → nothing is visible.
//!
//! Env:
//! * `HGHUB_WEB_PASSWORD_HASH` (required): sha256 hex of the gate password.
//! * `HGHUB_WEB_BIND` (default `127.0.0.1:8900`).
//! * `HGHUB_WEB_DATA` (default `./data`): session.json / line.json / logs.
//! * `HGHUB_WEB_DIST` (default `./dist`): the `VITE_HG_WEB=1 pnpm build` output.
//! * `HGHUB_WEB_PUBLIC_ORIGIN` (optional): 反代后的公网源站，如
//!   `https://8900-xxx.instabox.tech`（不带末尾 `/hg`）。设了就用它改写 m3u8 里的
//!   分片地址；不设则按请求头现算。反代会改写 Host 时必须设，否则改写出内网地址、手机播不了。

#[path = "../../src-tauri/src/net.rs"]
mod net;
#[path = "../../src-tauri/src/session.rs"]
mod session;
#[path = "../../src-tauri/src/lines.rs"]
mod lines;
#[path = "../../src-tauri/src/logging.rs"]
mod logging;
#[path = "../../src-tauri/src/network_policy.rs"]
mod network_policy;
// Web-local copies (tauri bits swapped out — see their headers): the
// desktop files would pull the Tauri/WRY/GTK tree (atk-sys) into this crate.
#[path = "playback_log_web.rs"]
mod playback_log;
#[path = "media_web.rs"]
mod media;

mod commands;

use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const COOKIE_NAME: &str = "hghub_web";
const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 3600);

#[derive(Clone)]
struct AppState {
    api: Arc<commands::Api>,
    data_dir: PathBuf,
    dist_dir: PathBuf,
    password_hash: [u8; 32],
    sessions: Arc<Mutex<HashMap<String, Instant>>>,
    /// 反代后的公网源站（如 https://8900-xxx.instabox.tech），用于 m3u8 改写。
    /// 未设时按请求头现算：优先 X-Forwarded-Host/Proto，再退回 Host（http）。
    public_origin: Option<String>,
}

fn sha256_hex(input: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hasher.finalize().into()
}

fn hex_decode_32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

fn new_session_token() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k == COOKIE_NAME).then(|| v.to_string())
    })
}

impl AppState {
    fn authed(&self, headers: &HeaderMap) -> bool {
        let Some(token) = cookie_token(headers) else {
            return false;
        };
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get(&token) {
            Some(&exp) if exp > Instant::now() => true,
            Some(_) => {
                sessions.remove(&token);
                false
            }
            None => false,
        }
    }
}

/// Gate: everything except the login page/endpoints needs a valid session.
/// API/media calls get 401, page loads redirect to /login.
async fn gate(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if path == "/login" || path == "/api/web-login" || path == "/api/web-status" {
        return next.run(req).await;
    }
    if state.authed(req.headers()) {
        return next.run(req).await;
    }
    if path.starts_with("/api/") || path.starts_with("/hg/") {
        return (StatusCode::UNAUTHORIZED, "请先登录").into_response();
    }
    Redirect::temporary("/login").into_response()
}

async fn web_status(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    Json(json!({ "authed": state.authed(&headers) }))
}

async fn web_login(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Response, (StatusCode, String)> {
    let password = body
        .get("password")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // Constant-time compare of the two hashes.
    let got = sha256_hex(password);
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= got[i] ^ state.password_hash[i];
    }
    if diff != 0 {
        return Err((StatusCode::UNAUTHORIZED, "密码错误".to_string()));
    }
    let token = new_session_token();
    state
        .sessions
        .lock()
        .unwrap()
        .insert(token.clone(), Instant::now() + SESSION_TTL);
    let cookie = format!(
        "{COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={}",
        SESSION_TTL.as_secs()
    );
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "ok": true })),
    )
        .into_response())
}

async fn web_logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie_token(&headers) {
        state.sessions.lock().unwrap().remove(&token);
    }
    (
        [(
            header::SET_COOKIE,
            format!("{COOKIE_NAME}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0"),
        )],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

/* ───────────────────────── 参数读取 ───────────────────────── */

fn req_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("参数错误：缺少 {key}"))
}

fn req_bool(args: &Value, key: &str) -> Result<bool, String> {
    args.get(key)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| format!("参数错误：缺少 {key}"))
}

fn req_u32(args: &Value, key: &str) -> Result<u32, String> {
    args.get(key)
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("参数错误：缺少 {key}"))
}

fn req_i64(args: &Value, key: &str) -> Result<i64, String> {
    args.get(key)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("参数错误：缺少 {key}"))
}

fn opt_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_i64())
}

fn opt_string(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/* ───────────────────────── /api/<cmd> ───────────────────────── */

async fn api_dispatch(
    State(state): State<AppState>,
    Path(cmd): Path<String>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let args = body.map(|Json(v)| v).unwrap_or(Value::Null);
    let api = &state.api;
    let err = |e: String| (StatusCode::BAD_REQUEST, e);
    let out = match cmd.as_str() {
        "app_boot" => commands::app_boot(api).await,
        "security_set" => commands::security_set(req_bool(&args, "strict").map_err(err)?).await,
        "debug_log" => {
            commands::debug_log(&state.data_dir, req_str(&args, "msg").map_err(err)?).await
        }
        "net_diag" => commands::net_diag().await,
        "line_state" => commands::line_state().await,
        "line_set" => commands::line_set(api, req_str(&args, "url").map_err(err)?).await,
        "line_mirrors" => commands::line_mirrors().await,
        "line_probe" => {
            let urls = args
                .get("urls")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "参数错误：缺少 urls".to_string()))?;
            commands::line_probe(urls).await
        }
        "check_update" => commands::check_update().await,
        "open_url" => commands::open_url(req_str(&args, "url").map_err(err)?).await,
        "set_orientation" => commands::set_orientation().await,
        "api_login" => {
            commands::api_login(
                api,
                req_str(&args, "username").map_err(err)?,
                req_str(&args, "password").map_err(err)?,
            )
            .await
        }
        "api_register" => {
            commands::api_register(
                api,
                req_str(&args, "username").map_err(err)?,
                req_str(&args, "password").map_err(err)?,
                opt_string(&args, "email"),
            )
            .await
        }
        "api_logout" => commands::api_logout(api).await,
        "api_me" => commands::api_me(api).await,
        "api_feed" => {
            commands::api_feed(
                api,
                req_u32(&args, "page").map_err(err)?,
                req_u32(&args, "size").map_err(err)?,
            )
            .await
        }
        "api_category" => {
            commands::api_category(
                api,
                req_str(&args, "cate").map_err(err)?,
                req_u32(&args, "page").map_err(err)?,
                req_u32(&args, "size").map_err(err)?,
            )
            .await
        }
        "api_rank" => commands::api_rank(api, req_str(&args, "kind").map_err(err)?).await,
        "api_search" => {
            commands::api_search(
                api,
                req_str(&args, "q").map_err(err)?,
                req_u32(&args, "page").map_err(err)?,
                req_u32(&args, "size").map_err(err)?,
            )
            .await
        }
        "api_posts" => {
            commands::api_posts(
                api,
                req_u32(&args, "page").map_err(err)?,
                req_u32(&args, "size").map_err(err)?,
            )
            .await
        }
        "api_post" => commands::api_post(api, req_i64(&args, "id").map_err(err)?).await,
        "api_video" => commands::api_video(api, req_i64(&args, "id").map_err(err)?).await,
        "api_play" => {
            commands::api_play(
                api,
                req_i64(&args, "id").map_err(err)?,
                req_u32(&args, "ep").map_err(err)?,
            )
            .await
        }
        "api_comments" => {
            commands::api_comments(
                api,
                req_i64(&args, "id").map_err(err)?,
                req_u32(&args, "ep").map_err(err)?,
                req_u32(&args, "page").map_err(err)?,
                req_u32(&args, "size").map_err(err)?,
            )
            .await
        }
        "api_comment_send" => {
            commands::api_comment_send(
                api,
                req_i64(&args, "id").map_err(err)?,
                req_u32(&args, "ep").map_err(err)?,
                req_str(&args, "text").map_err(err)?,
                opt_i64(&args, "reply_id"),
            )
            .await
        }
        "api_like" => {
            commands::api_like(
                api,
                req_i64(&args, "id").map_err(err)?,
                req_u32(&args, "ep").map_err(err)?,
                req_bool(&args, "on").map_err(err)?,
            )
            .await
        }
        "api_collect" => {
            commands::api_collect(
                api,
                req_i64(&args, "id").map_err(err)?,
                req_bool(&args, "on").map_err(err)?,
            )
            .await
        }
        "api_follow" => {
            commands::api_follow(
                api,
                req_i64(&args, "uid").map_err(err)?,
                req_bool(&args, "on").map_err(err)?,
            )
            .await
        }
        "cache_clear" => commands::cache_clear().await,
        "api_post_like" => {
            commands::api_post_like(
                api,
                req_i64(&args, "id").map_err(err)?,
                req_bool(&args, "on").map_err(err)?,
            )
            .await
        }
        "api_post_collect" => {
            commands::api_post_collect(
                api,
                req_i64(&args, "id").map_err(err)?,
                req_bool(&args, "on").map_err(err)?,
            )
            .await
        }
        "api_post_comments" => {
            commands::api_post_comments(
                api,
                req_i64(&args, "id").map_err(err)?,
                req_u32(&args, "page").map_err(err)?,
                req_u32(&args, "size").map_err(err)?,
            )
            .await
        }
        "playback_log_status" => {
            playback_log::playback_log_status().map(|s| serde_json::to_value(s).unwrap())
        }
        "playback_log_set" => {
            playback_log::playback_log_set(req_bool(&args, "enabled").map_err(err)?)
                .map(|s| serde_json::to_value(s).unwrap())
        }
        "playback_log_write" => {
            let entries: Vec<playback_log::Entry> = serde_json::from_value(
                args.get("entries").cloned().unwrap_or(Value::Null),
            )
            .map_err(|_| (StatusCode::BAD_REQUEST, "参数错误：entries".to_string()))?;
            playback_log::playback_log_write(entries).map(|_| Value::Null)
        }
        "playback_log_read" => playback_log::playback_log_read().map(|s| json!(s)),
        _ => return Err((StatusCode::NOT_FOUND, format!("未知命令：{cmd}"))),
    };
    out.map(Json).map_err(err)
}

/* ───────────────────────── /hg/ 媒体代理 ───────────────────────── */

async fn media_proxy(State(state): State<AppState>, req: Request) -> Response {
    let uri = req.uri().clone();
    let path = uri.path().to_string(); // /hg/img | /hg/hls | /hg/seg
    let route = path.strip_prefix("/hg").unwrap_or(&path).to_string();
    let query = uri.query().unwrap_or("").to_string();
    // m3u8 改写用的代理基址。反代（如 InstaBox 公网端口）会把 Host 改写成本机地址，
    // 只按 Host 现算会得到 http://127.0.0.1:8900，手机上播不了；因此：
    // 1) HGHUB_WEB_PUBLIC_ORIGIN 已设时直接用它；2) 否则优先 X-Forwarded-Host/Proto；3) 再退回 Host。
    let headers = req.headers();
    let header_str = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let origin = match &state.public_origin {
        Some(o) => format!("{o}/hg"),
        None => {
            let host = header_str("x-forwarded-host")
                .or_else(|| header_str(header::HOST.as_str()))
                .unwrap_or("127.0.0.1:8900");
            let scheme = header_str("x-forwarded-proto").unwrap_or("http");
            format!("{scheme}://{host}/hg")
        }
    };
    let resp = media::handle(&route, &query, &origin).await;
    let status = StatusCode::from_u16(resp.status).unwrap_or(StatusCode::OK);
    let mut builder = Response::builder().status(status);
    for (k, v) in resp.headers {
        builder = builder.header(k, v);
    }
    builder.body(Body::from(resp.body)).unwrap()
}

/* ───────────────────────── 静态前端 ───────────────────────── */

fn content_type(path: &PathBuf) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("webmanifest") => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

async fn static_handler(State(state): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    if path == "/" {
        let file = state.dist_dir.join("index.html");
        return match tokio::fs::read(&file).await {
            Ok(bytes) => (
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                bytes,
            )
                .into_response(),
            Err(_) => (StatusCode::NOT_FOUND, "前端未构建：请先跑 VITE_HG_WEB=1 pnpm build").into_response(),
        };
    }
    let rel = path.trim_start_matches('/');
    if rel.contains("..") {
        return (StatusCode::BAD_REQUEST, "bad path").into_response();
    }
    let file = state.dist_dir.join(rel);
    match tokio::fs::read(&file).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, content_type(&file))], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn login_page() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../login.html"),
    )
        .into_response()
}

#[tokio::main]
async fn main() {
    let password_hash = std::env::var("HGHUB_WEB_PASSWORD_HASH")
        .ok()
        .and_then(|s| hex_decode_32(&s))
        .unwrap_or_else(|| {
            eprintln!("HGHUB_WEB_PASSWORD_HASH 未设置或不是 64 位十六进制：拒绝启动（密码门不许空着跑）。");
            std::process::exit(1);
        });
    let bind: std::net::SocketAddr = std::env::var("HGHUB_WEB_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8900".to_string())
        .parse()
        .unwrap_or_else(|_| {
            eprintln!("HGHUB_WEB_BIND 解析失败");
            std::process::exit(1);
        });
    let data_dir = PathBuf::from(std::env::var("HGHUB_WEB_DATA").unwrap_or_else(|_| "./data".into()));
    let dist_dir = PathBuf::from(std::env::var("HGHUB_WEB_DIST").unwrap_or_else(|_| "./dist".into()));
    let public_origin = std::env::var("HGHUB_WEB_PUBLIC_ORIGIN")
        .ok()
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty());
    std::fs::create_dir_all(&data_dir).expect("创建数据目录失败");

    net::bind_base(&data_dir);
    playback_log::bind(&data_dir);
    let api = Arc::new(commands::Api::new());
    api.session.bind(data_dir.clone());

    let state = AppState {
        api,
        data_dir: data_dir.clone(),
        dist_dir: dist_dir.clone(),
        password_hash,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        public_origin,
    };

    let app = Router::new()
        .route("/login", get(login_page))
        .route("/api/web-login", post(web_login))
        .route("/api/web-logout", post(web_logout))
        .route("/api/web-status", get(web_status))
        .route("/api/{cmd}", post(api_dispatch))
        .route("/hg/img", get(media_proxy))
        .route("/hg/hls", get(media_proxy))
        .route("/hg/seg", get(media_proxy))
        .fallback(get(static_handler))
        .layer(middleware::from_fn_with_state(state.clone(), gate))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind).await.unwrap_or_else(|e| {
        eprintln!("绑定 {bind} 失败：{e}");
        std::process::exit(1);
    });
    println!("hghub-web listening on {bind}");
    println!("data: {}", data_dir.display());
    println!("dist: {}", dist_dir.display());
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await
        .unwrap();
}
