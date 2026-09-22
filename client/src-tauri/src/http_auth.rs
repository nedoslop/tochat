use serde_json::json;

pub async fn register(base_url: &str, username: &str, password: &str) -> Result<(), String> {
    let url = format!("{}/register", base_url.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&json!({ "username": username, "password": password }))
        .send()
        .await
        .map_err(|e| format!("http: {e}"))?;

    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!("server: {status} {body}"))
    }
}
