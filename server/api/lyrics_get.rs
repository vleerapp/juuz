use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::server::utils::db::libraries::can_access_song;
use crate::server::utils::http_util::{denied, failed, session_user};
use crate::server::utils::lyrics;
use crate::server::utils::state::AppState;

#[derive(Deserialize)]
pub struct LyricsQuery {
    pub id: String,
}

pub async fn lyrics(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LyricsQuery>,
) -> Response {
    let Some(user) = session_user(&st, &headers).await else {
        return denied();
    };
    if !(user.stream || user.admin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let (uid, check) = (user.id.clone(), q.id.clone());
    match st
        .db
        .run(move |c| Ok(can_access_song(c, &uid, &check)?))
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return failed(e),
    }
    let (db, client, id) = (st.db.clone(), st.lyrics.clone(), q.id);
    let found = match tokio::task::spawn_blocking(move || lyrics::find(&db, &client, &id)).await {
        Ok(Ok(found)) => found,
        Ok(Err(e)) => return failed(e),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let Some(found) = found else {
        return Json(json!({"synced": false, "lines": []})).into_response();
    };
    let (synced, lines) = lyrics::parse_rich(&found.text);
    let lines: Vec<_> = lines
        .iter()
        .map(|l| {
            json!({
                "start": l.start,
                "end": l.end,
                "text": l.text,
                "secondary": l.secondary,
                "gap": l.gap,
                "words": l.words.iter().map(|w| json!({"start": w.start, "text": w.text})).collect::<Vec<_>>(),
            })
        })
        .collect();
    Json(json!({"synced": synced, "lines": lines})).into_response()
}
