use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;
use serde_json::json;

use crate::state::AppState;
use crate::util::hash_password;

#[derive(Debug, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

fn bad(status: StatusCode, err: &str, msg: &str) -> axum::response::Response {
    (status, Json(json!({ "error": err, "message": msg }))).into_response()
}

pub async fn register(
    State(state): State<AppState>,
    Json(creds): Json<Credentials>,
) -> impl IntoResponse {
    if creds.username.is_empty() || creds.password.is_empty() {
        return bad(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "missing credentials",
        );
    }
    let hash = hash_password(&creds.password);
    if state.db.create_user(&creds.username, &hash).await {
        (StatusCode::OK, Json(json!({ "status": "ok" }))).into_response()
    } else {
        bad(StatusCode::CONFLICT, "conflict", "username already taken")
    }
}

pub async fn login(
    State(state): State<AppState>,
    Json(creds): Json<Credentials>,
) -> impl IntoResponse {
    match state.db.get_user(&creds.username).await {
        Some((h, _)) if h == hash_password(&creds.password) => {
            (StatusCode::OK, Json(json!({ "status": "ok" }))).into_response()
        }
        _ => bad(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid credentials",
        ),
    }
}
