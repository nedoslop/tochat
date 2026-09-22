use reqwest::StatusCode;
use serde_json::json;

/// POST /login, falling back to POST /register on 401.
///
/// The UI hint says "if login does not exist, it will be created", so we
/// try login first; a `409 Conflict` on register then means "user exists
/// with a different password".
pub async fn login_or_register(
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<(), String> {
    let base = base_url.trim_end_matches('/');
    let client = reqwest::Client::new();
    let body = json!({ "username": username, "password": password });

    let resp = client
        .post(format!("{base}/login"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("login request failed: {e}"))?;

    match resp.status() {
        StatusCode::OK => return Ok(()),
        StatusCode::UNAUTHORIZED => {} // try to register
        other => return Err(format!("unexpected login status {other}")),
    }

    let resp = client
        .post(format!("{base}/register"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("register request failed: {e}"))?;

    match resp.status() {
        StatusCode::OK => Ok(()),
        StatusCode::CONFLICT => Err("invalid credentials".into()),
        other => Err(format!("unexpected register status {other}")),
    }
}

/// Derive the websocket URL from the HTTP base URL:
///   http://host:port  -> ws://host:port/ws
///   https://host:port -> wss://host:port/ws
pub fn to_ws_url(base_url: &str) -> Result<String, String> {
    let base = base_url.trim_end_matches('/');
    if let Some(rest) = base.strip_prefix("http://") {
        Ok(format!("ws://{rest}/ws"))
    } else if let Some(rest) = base.strip_prefix("https://") {
        Ok(format!("wss://{rest}/ws"))
    } else {
        Err("base URL must start with http:// or https://".into())
    }
}