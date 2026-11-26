use crate::server::utils::http_util::{cross_site, denied, failed, session_user};
use crate::server::utils::state::AppState;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct EventQuery {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub position: Option<i64>,
    pub playlist: Option<String>,
}

pub async fn event(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<EventQuery>,
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
    let kind = q.kind.to_uppercase();
    if !crate::server::utils::db::events::KINDS.contains(&kind.as_str()) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let (uid, song, playlist, position) = (user.id.clone(), q.id.clone(), q.playlist, q.position);
    let stored = st
        .db
        .run({
            let kind = kind.clone();
            move |c| {
                if !crate::server::utils::db::libraries::can_access_song(c, &uid, &song)? {
                    return Err(crate::server::utils::error::ApiError::not_found("song"));
                }
                crate::server::utils::db::events::insert(
                    c,
                    &uid,
                    &kind,
                    &song,
                    playlist.as_deref(),
                    position.map(|p| p.max(0)),
                    crate::server::utils::db::now_ms(),
                )?;
                Ok(())
            }
        })
        .await;
    if let Err(e) = stored {
        return failed(e);
    }
    match kind.as_str() {
        "PLAY" => {
            let _ = crate::server::api::rest::annotation::record(
                &st,
                &user,
                vec![q.id],
                Vec::new(),
                false,
                "juuz web".to_string(),
                false,
            )
            .await;
        }
        "STOP" => {
            st.now_playing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&(user.id.clone(), "juuz web".to_string()));
            st.live.notify();
        }
        _ => {}
    }
    StatusCode::NO_CONTENT.into_response()
}
