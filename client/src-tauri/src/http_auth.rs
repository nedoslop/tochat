use reqwest::Client;
use serde_json::json;

pub async fn register(base_url: &str, username: &str, password: &str) -> Result<(), String> {
    let url = format!("{}/register", base_url.trim_end_matches('/'));
    let client = Client::new();
    let res = client
        .post(&url)
        .json(&json!({ "username": username, "password": password }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if res.status().is_success() {
        Ok(())
    } else {
        let text = res.text().await.unwrap_or_default();
        Err(format!("register failed: {}", text))
    }
}