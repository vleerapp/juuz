use crate::server::utils::remote::{caller, failed, outputs_json};
use crate::server::utils::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

pub async fn scan(State(st): State<AppState>, headers: HeaderMap) -> Response {
    let user = match caller(&st, &headers, true).await {
        Ok(u) => u,
        Err(status) => return status.into_response(),
    };
    match st.jukebox.scan().await {
        Ok(_) => Json(outputs_json(&st, &user).await).into_response(),
        Err(e) => failed(e),
    }
}
