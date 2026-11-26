use crate::server::utils::jukebox::group::BROWSER_PREFIX;
use crate::server::utils::remote::{OutputQuery, caller, failed, outputs_json, owns_seat};
use crate::server::utils::state::AppState;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

pub async fn select(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<OutputQuery>,
) -> Response {
    let user = match caller(&st, &headers, true).await {
        Ok(u) => u,
        Err(status) => return status.into_response(),
    };
    let id = q.id.as_deref().filter(|s| !s.is_empty());
    if id.is_some_and(|id| id.starts_with(BROWSER_PREFIX) && !owns_seat(&st, &user, id)) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match st.jukebox.select(id, None).await {
        Ok(()) => Json(outputs_json(&st, &user).await).into_response(),
        Err(e) => {
            tracing::warn!("selecting jukebox output: {e}");
            failed(e)
        }
    }
}
