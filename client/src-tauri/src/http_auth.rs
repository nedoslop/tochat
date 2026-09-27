/// Registers a new user via an HTTP POST to `<base_url>/register`.
pub async fn register(base_url: &str, username: &str, password: &str) -> Result<(), String> {
    let url = format!("{}/register", base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "username": username, "password": password }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(resp
            .text()
            .await
            .unwrap_or_else(|_| "registration failed".into()))
    }
}