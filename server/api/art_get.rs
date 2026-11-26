use crate::server::api::rest::media;
use crate::server::utils::http_util::{MediaQuery, denied, failed, session_user};
use crate::server::utils::state::AppState;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;

pub async fn art(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MediaQuery>,
) -> Response {
    let Some(user) = session_user(&st, &headers).await else {
        return denied();
    };
    let size = q.size.map(|s| s.clamp(16, 2400));
    match media::cover_response(&st, user.id, q.id, size).await {
        Ok(resp) => resp,
        Err(e) => failed(e),
    }
}
