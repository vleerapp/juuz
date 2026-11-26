use crate::server::utils::http_util::{cross_site, denied, failed, session_user};
use crate::server::utils::state::AppState;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct ScrobbleQuery {
    pub id: String,
    pub submission: Option<bool>,
}

pub async fn scrobble(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ScrobbleQuery>,
) -> Response {
    if cross_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(user) = session_user(&st, &headers).await else {
        return denied();
    };
    if !(user.stream || user.admin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match crate::server::api::rest::annotation::record(
        &st,
        &user,
        vec![q.id],
        Vec::new(),
        q.submission.unwrap_or(true),
        "juuz web".to_string(),
        false,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => failed(e),
    }
}
