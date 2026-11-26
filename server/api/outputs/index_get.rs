use crate::server::utils::remote::{caller, outputs_json};
use crate::server::utils::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

pub async fn outputs(State(st): State<AppState>, headers: HeaderMap) -> Response {
    match caller(&st, &headers, false).await {
        Ok(user) => Json(outputs_json(&st, &user).await).into_response(),
        Err(status) => status.into_response(),
    }
}
