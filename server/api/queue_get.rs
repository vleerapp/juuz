use crate::server::utils::remote::{caller, failed, state_json};
use crate::server::utils::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

pub async fn state(State(st): State<AppState>, headers: HeaderMap) -> Response {
    let user = match caller(&st, &headers, false).await {
        Ok(u) => u,
        Err(status) => return status.into_response(),
    };
    match state_json(&st, &user).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => failed(e),
    }
}
