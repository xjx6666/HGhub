//! Plain-async mirrors of the desktop Tauri commands in `src-tauri/src/api.rs`.
//!
//! The bodies are the same logic, only the plumbing differs: no `tauri::State`,
//! no `tauri::AppHandle`. `Api` here is the same 3-method core (get/post/authed
//! over `net::edge`) as the desktop `Api`; keep the two in sync when the
//! upstream logic changes.

use crate::net::{base, edge};
use crate::session::SessionStore;
use serde_json::{json, Value};

pub struct Api {
    pub session: SessionStore,
}

impl Api {
    pub fn new() -> Self {
        Self {
            session: SessionStore::new(),
        }
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        let origin = base();
        let token = self.session.token(&origin);
        edge(&origin, "GET", path, None, token.as_deref()).await
    }

    /// POST（游客也可调用，有登录态时带上 token）：新播放接口需要。
    async fn post(&self, path: &str, body: Option<Value>) -> Result<Value, String> {
        let origin = base();
        let token = self.session.token(&origin);
        edge(&origin, "POST", path, body, token.as_deref()).await
    }

    async fn authed(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let origin = base();
        let token = self
            .session
            .token(&origin)
            .ok_or_else(|| "请先登录".to_string())?;
        edge(&origin, method, path, body, Some(&token)).await
    }
}

const REPO: &str = "Minireel/HGhub";

fn ver_parts(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches(['v', 'V'])
        .split(['.', '-', '+'])
        .map(|s| s.trim().parse::<u64>().unwrap_or(0))
        .collect()
}

/// 只比较数字段：`v1.2.0` > `1.1.9`；解析不出的段按 0 处理。
fn is_newer(latest: &str, current: &str) -> bool {
    let a = ver_parts(latest);
    let b = ver_parts(current);
    for i in 0..a.len().max(b.len()) {
        let x = *a.get(i).unwrap_or(&0);
        let y = *b.get(i).unwrap_or(&0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 新版播放接口返回 `streams.{main,hevc,preview}`，旧接口的 `video_url` 现在恒为空串。
/// 这里把新字段回填到 `video_url` / `video_url_hevc`，前端和自检继续按原字段取流。
fn normalize_play_info(info: &mut Value) {
    fn non_empty(v: Option<&Value>) -> Option<String> {
        v.and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }
    let main = non_empty(info.get("streams").and_then(|s| s.get("main")));
    let hevc = non_empty(info.get("streams").and_then(|s| s.get("hevc")));
    let Some(obj) = info.as_object_mut() else { return };
    for (field, stream) in [("video_url", main), ("video_url_hevc", hevc)] {
        if let Some(url) = stream {
            if non_empty(obj.get(field)).is_none() {
                obj.insert(field.into(), json!(url));
            }
        }
    }
}

fn has_play_url(info: &Value) -> bool {
    info.get("video_url")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

pub async fn debug_log(data_dir: &std::path::Path, msg: String) -> Result<Value, String> {
    crate::logging::append_debug(data_dir, &msg)?;
    Ok(Value::Null)
}

/// 网络自检：系统代理 / DNS / 连通性快照，用于远程定位「打不开」。
pub async fn net_diag() -> Result<Value, String> {
    Ok(crate::net::diagnose().await)
}

/* ───────────────────────── 线路（数据源） ───────────────────────── */

pub async fn line_state() -> Result<Value, String> {
    Ok(json!({
        "base": base(),
        "default": crate::net::DEFAULT_BASE,
        "fixed": crate::lines::fixed(),
    }))
}

pub async fn line_set(api: &Api, url: String) -> Result<Value, String> {
    let previous = base();
    let next = crate::net::set_base(&url)?;
    if previous != next {
        api.session.clear();
    }
    Ok(json!(next))
}

pub async fn line_mirrors() -> Result<Value, String> {
    crate::net::refresh_client();
    crate::lines::mirrors().await
}

pub async fn line_probe(urls: Vec<String>) -> Result<Value, String> {
    crate::net::refresh_client();
    Ok(crate::lines::probe(urls).await)
}

/* ───────────────────────── 更新检查 ───────────────────────── */

pub async fn check_update() -> Result<Value, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = crate::net::get(
        &url,
        std::time::Duration::from_secs(12),
        Some("application/vnd.github+json"),
        None,
    )
    .await?;

    let status = resp.status();
    if status.as_u16() == 404 {
        // 还没发过 release
        return Ok(json!({
            "current": current, "latest": "", "has_update": false,
            "url": format!("https://github.com/{REPO}/releases"),
        }));
    }
    if !status.is_success() {
        return Err(format!("GitHub 返回 HTTP {status}"));
    }

    let v: Value = resp.json().await.map_err(|e| crate::net::err_text(&e))?;
    let tag = v
        .get("tag_name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let html = v
        .get("html_url")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("https://github.com/{REPO}/releases"));
    let notes = v.get("body").and_then(|x| x.as_str()).unwrap_or("");

    Ok(json!({
        "current": current,
        "latest": tag.trim_start_matches(['v', 'V']).to_string(),
        "tag": tag,
        "has_update": !tag.is_empty() && is_newer(&tag, &current),
        "url": html,
        "name": v.get("name").and_then(|x| x.as_str()).unwrap_or(""),
        "published_at": v.get("published_at").and_then(|x| x.as_str()).unwrap_or(""),
        "notes": notes.chars().take(800).collect::<String>(),
    }))
}

/// 用系统默认浏览器打开链接（只允许 http/https）。
/// 网页版没有系统 opener：返回校验过的地址，前端直接 window.open。
pub async fn open_url(url: String) -> Result<Value, String> {
    let target = crate::network_policy::parse_target(&url)?;
    Ok(json!(target.as_str()))
}

/// 切换移动端屏幕方向：网页版无原生旋转能力，直接成功，前端用 Screen Orientation API 自行处理。
pub async fn set_orientation() -> Result<Value, String> {
    Ok(Value::Null)
}

/* ───────────────────────── 会话 ───────────────────────── */

pub async fn app_boot(api: &Api) -> Result<Value, String> {
    let origin = base();
    Ok(json!({
        "user": api.session.user(&origin),
        "logged_in": api.session.token(&origin).is_some(),
        "base": origin,
        "strict_security": crate::network_policy::strict_security(),
        "cli_relaxed": crate::network_policy::cli_strict_override() == Some(false),
    }))
}

pub async fn security_set(strict: bool) -> Result<Value, String> {
    crate::network_policy::set_strict_security(strict)?;
    crate::net::refresh_client();
    crate::playback_log::record(
        "security.mode_changed",
        json!({"strict": crate::network_policy::strict_security()}),
    );
    Ok(json!(crate::network_policy::strict_security()))
}

pub async fn api_login(api: &Api, username: String, password: String) -> Result<Value, String> {
    let origin = base();
    let data = edge(
        &origin,
        "POST",
        "/api/account/loginByPassword",
        Some(json!({ "username": username, "password": password })),
        None,
    )
    .await?;
    let token = data
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "登录返回缺少 token".to_string())?
        .to_string();
    let user = data.get("user").cloned().unwrap_or(Value::Null);
    if base() != origin {
        return Err("登录期间线路已变化，请重新登录".into());
    }
    api.session.set(&origin, token, user.clone());
    Ok(json!({ "user": user }))
}

pub async fn api_register(
    api: &Api,
    username: String,
    password: String,
    email: String,
) -> Result<Value, String> {
    let origin = base();
    let mut body = json!({
        "username": username,
        "password": password,
        "confirm_password": password,
    });
    if !email.trim().is_empty() {
        body["email"] = json!(email);
    }
    let data = edge(
        &origin,
        "POST",
        "/api/account/registerByPassword",
        Some(body),
        None,
    )
    .await?;
    let token = data
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "注册返回缺少 token".to_string())?
        .to_string();
    let user = data.get("user").cloned().unwrap_or(Value::Null);
    if base() != origin {
        return Err("注册期间线路已变化，请重新登录".into());
    }
    api.session.set(&origin, token, user.clone());
    Ok(json!({ "user": user }))
}

pub async fn api_logout(api: &Api) -> Result<Value, String> {
    let origin = base();
    let token = api.session.token(&origin);
    let _ = edge(
        &origin,
        "POST",
        "/api/account/logout",
        None,
        token.as_deref(),
    )
    .await;
    api.session.clear();
    Ok(json!({ "ok": true }))
}

pub async fn api_me(api: &Api) -> Result<Value, String> {
    let origin = base();
    let token = api.session.token(&origin);
    let me = edge(&origin, "GET", "/api/account/me", None, token.as_deref()).await?;
    if base() != origin {
        return Err("读取账号期间线路已变化".into());
    }
    if let Some(token) = api.session.token(&origin) {
        api.session.set(&origin, token, me.clone());
    }
    Ok(me)
}

/* ───────────────────────── 列表 ───────────────────────── */

pub async fn api_feed(api: &Api, page: u32, size: u32) -> Result<Value, String> {
    api.get(&format!("/api/videos?page={page}&size={size}")).await
}

pub async fn api_category(api: &Api, cate: String, page: u32, size: u32) -> Result<Value, String> {
    api.get(&format!(
        "/api/videos/category/{cate}?page={page}&size={size}"
    ))
    .await
}

pub async fn api_rank(api: &Api, kind: String) -> Result<Value, String> {
    let kind = match kind.as_str() {
        "recommend" | "potential" => kind,
        _ => "hot".to_string(),
    };
    api.get(&format!("/api/ranks/{kind}")).await
}

pub async fn api_search(api: &Api, q: String, page: u32, size: u32) -> Result<Value, String> {
    let encoded: String = url::form_urlencoded::byte_serialize(q.as_bytes()).collect();
    api.get(&format!(
        "/api/search?q={encoded}&page={page}&size={size}"
    ))
    .await
}

pub async fn api_posts(api: &Api, page: u32, size: u32) -> Result<Value, String> {
    api.get(&format!("/api/posts?page={page}&size={size}")).await
}

pub async fn api_post(api: &Api, id: i64) -> Result<Value, String> {
    api.get(&format!("/api/posts/{id}")).await
}

/* ───────────────────────── 详情 / 播放 ───────────────────────── */

pub async fn api_video(api: &Api, id: i64) -> Result<Value, String> {
    api.get(&format!("/api/videos/{id}")).await
}

pub async fn api_play(api: &Api, id: i64, ep: u32) -> Result<Value, String> {
    // 优先走新接口；失败或没有可用地址时再退回旧接口，两者都不行就返回新接口的结果/错误。
    let primary = api
        .post(&format!("/api/videos/{id}/episodes/{ep}/playback"), Some(json!({})))
        .await
        .map(|mut info| {
            normalize_play_info(&mut info);
            info
        });
    if let Ok(info) = &primary {
        if has_play_url(info) {
            return primary;
        }
    }
    match api
        .get(&format!("/api/videos/{id}/play?ep={ep}&page_texts=0"))
        .await
    {
        Ok(mut legacy) => {
            normalize_play_info(&mut legacy);
            if has_play_url(&legacy) || primary.is_err() {
                Ok(legacy)
            } else {
                primary
            }
        }
        Err(_) => primary,
    }
}

/* ───────────────────────── 评论 / 互动 ───────────────────────── */

pub async fn api_comments(api: &Api, id: i64, ep: u32, page: u32, size: u32) -> Result<Value, String> {
    api.get(&format!(
        "/api/videos/{id}/comments?ep={ep}&page={page}&size={size}"
    ))
    .await
}

pub async fn api_comment_send(
    api: &Api,
    id: i64,
    ep: u32,
    text: String,
    reply_id: Option<i64>,
) -> Result<Value, String> {
    let mut body = json!({ "text": text });
    let path = match reply_id {
        Some(rid) if rid > 0 => {
            body["reply_id"] = json!(rid);
            format!("/api/videos/{id}/comments")
        }
        _ => format!("/api/videos/{id}/comments?ep={ep}"),
    };
    api.authed("POST", &path, Some(body)).await
}

pub async fn api_like(api: &Api, id: i64, ep: u32, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    api.authed(method, &format!("/api/videos/{id}/like?ep={ep}"), None)
        .await
}

pub async fn api_collect(api: &Api, id: i64, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    api.authed(method, &format!("/api/videos/{id}/collect"), None)
        .await
}

pub async fn api_follow(api: &Api, uid: i64, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    api.authed(method, &format!("/api/users/{uid}/follow"), None)
        .await
}

/* ───────────────────────── 本地缓存 ───────────────────────── */

pub async fn cache_clear() -> Result<Value, String> {
    crate::media::clear_caches();
    Ok(Value::Null)
}

/* ───────────────────────── 吃瓜帖子互动 ───────────────────────── */

pub async fn api_post_like(api: &Api, id: i64, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    api.authed(method, &format!("/api/posts/{id}/like"), None)
        .await
}

pub async fn api_post_collect(api: &Api, id: i64, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    api.authed(method, &format!("/api/posts/{id}/collect"), None)
        .await
}

pub async fn api_post_comments(api: &Api, id: i64, page: u32, size: u32) -> Result<Value, String> {
    api.get(&format!("/api/posts/{id}/comments?page={page}&size={size}"))
        .await
}
