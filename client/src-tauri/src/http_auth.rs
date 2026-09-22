use serde::Serialize;

#[derive(Serialize)]
struct Creds<'a> {
    username: &'a str,
    password: &'a str,
}

pub async fn register(base_url: &str, username: &str, password: &str) -> Result<(), String> {
    let url = format!("{}/register", base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&Creds { username, password })
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("register failed: {text}"));
    }
    Ok(())
}
