use crate::server::api::rest::media;
use crate::server::utils::http_util::{MediaQuery, denied, failed, session_user};
use crate::server::utils::state::AppState;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

pub async fn play(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MediaQuery>,
) -> Response {
    let Some(user) = session_user(&st, &headers).await else {
        return denied();
    };
    if !(user.stream || user.admin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match media::serve_song_as(&st, &user.id, &q.id, &headers).await {
        Ok(resp) => resp,
        Err(e) => failed(e),
    }
}
