use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use topcoat::session::Token;

use crate::api::media;
use crate::db::now;
use crate::db::users::{self, User};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct MediaQuery {
    id: String,
    size: Option<u32>,
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(k, v)| (k == name).then_some(v))
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = (bytes[i] == b'%' && i + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok())
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match decoded {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn session_user(st: &AppState, headers: &HeaderMap) -> Option<User> {
    let raw = cookie(headers, super::COOKIE)?;
    let token = Token::decode(&percent_decode(raw)).ok()?;
    let key = token.hash().to_vec();
    st.db
        .run(move |c| Ok(users::by_session(c, &key, now())?))
        .await
        .ok()
        .flatten()
}

fn denied() -> Response {
    StatusCode::UNAUTHORIZED.into_response()
}

fn failed(e: crate::error::ApiError) -> Response {
    let status = if e.code == 70 {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::BAD_REQUEST
    };
    status.into_response()
}

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

#[derive(Deserialize)]
pub struct ScrobbleQuery {
    id: String,
    submission: Option<bool>,
}

pub async fn scrobble(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ScrobbleQuery>,
) -> Response {
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v != "same-origin")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(user) = session_user(&st, &headers).await else {
        return denied();
    };
    if !(user.stream || user.admin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match crate::api::annotation::record(
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

#[derive(Deserialize)]
pub struct EventQuery {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    position: Option<i64>,
    playlist: Option<String>,
}

pub async fn event(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<EventQuery>,
) -> Response {
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v != "same-origin")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(user) = session_user(&st, &headers).await else {
        return denied();
    };
    if !(user.stream || user.admin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let kind = q.kind.to_uppercase();
    if !crate::db::events::KINDS.contains(&kind.as_str()) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let (uid, song, playlist, position) = (user.id.clone(), q.id.clone(), q.playlist, q.position);
    let stored = st
        .db
        .run({
            let kind = kind.clone();
            move |c| {
                if !crate::db::libraries::can_access_song(c, &uid, &song)? {
                    return Err(crate::error::ApiError::not_found("song"));
                }
                crate::db::events::insert(
                    c,
                    &uid,
                    &kind,
                    &song,
                    playlist.as_deref(),
                    position.map(|p| p.max(0)),
                    crate::db::now_ms(),
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
            let _ = crate::api::annotation::record(
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
