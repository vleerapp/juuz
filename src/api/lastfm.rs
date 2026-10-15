use serde_json::json;

use super::{ApiResult, Ctx, empty, ok};
use crate::db::users;
use crate::error::ApiError;
use crate::lastfm::{self, ApiFailure, Track};
use crate::state::AppState;

fn client(ctx: &Ctx) -> Result<std::sync::Arc<lastfm::Client>, ApiError> {
    ctx.st
        .lastfm
        .clone()
        .ok_or_else(|| ApiError::generic("last.fm is not configured on this server"))
}

fn upstream(e: anyhow::Error) -> ApiError {
    match e.downcast_ref::<ApiFailure>() {
        Some(f) => ApiError::generic(format!("last.fm: {}", f.message)),
        None => ApiError::generic(format!("last.fm request failed: {e}")),
    }
}

pub async fn status(ctx: Ctx) -> ApiResult {
    let uid = ctx.user().id.clone();
    let linked = ctx
        .st
        .db
        .run(move |c| Ok(users::lastfm_session(c, &uid)?))
        .await?;
    ok(json!({"juuzLastfm": {
        "configured": ctx.st.lastfm.is_some(),
        "linked": linked.is_some(),
        "username": linked.map(|(_, name)| name),
    }}))
}

pub async fn auth_url(ctx: Ctx) -> ApiResult {
    let client = client(&ctx)?;
    let (token, url) = tokio::task::spawn_blocking(move || {
        client.auth_token().map(|t| {
            let url = client.auth_link(&t);
            (t, url)
        })
    })
    .await
    .map_err(|_| ApiError::generic("internal server error"))?
    .map_err(upstream)?;
    ok(json!({"juuzLastfm": {"token": token, "url": url}}))
}

pub async fn link(ctx: Ctx) -> ApiResult {
    let client = client(&ctx)?;
    let token = ctx.params.req("token")?.to_string();
    let (key, name) = tokio::task::spawn_blocking(move || client.session(&token))
        .await
        .map_err(|_| ApiError::generic("internal server error"))?
        .map_err(upstream)?;
    let sealed = ctx.st.crypto.seal(key.as_bytes());
    let (uid, stored_name) = (ctx.user().id.clone(), name.clone());
    ctx.st
        .db
        .run(move |c| Ok(users::set_lastfm(c, &uid, Some((&sealed, &stored_name)))?))
        .await?;
    ok(json!({"juuzLastfm": {"linked": true, "username": name}}))
}

pub async fn unlink(ctx: Ctx) -> ApiResult {
    let uid = ctx.user().id.clone();
    ctx.st
        .db
        .run(move |c| Ok(users::set_lastfm(c, &uid, None)?))
        .await?;
    empty()
}

struct Song {
    artist: String,
    title: String,
    album: Option<String>,
    duration_secs: u32,
}

fn load_song(conn: &rusqlite::Connection, id: &str) -> Option<Song> {
    conn.query_row(
        "SELECT s.title, s.display_artist, al.title, s.duration_ms,
                (SELECT a.name FROM song_artists sa JOIN artists a ON a.id = sa.artist_id
                   WHERE sa.song_id = s.id ORDER BY sa.position LIMIT 1)
         FROM songs s JOIN albums al ON al.id = s.album_id WHERE s.id = ?1",
        [id],
        |r| {
            let album: String = r.get(2)?;
            let first: Option<String> = r.get(4)?;
            Ok(Song {
                title: r.get(0)?,
                artist: first.unwrap_or(r.get(1)?),
                album: (!album.starts_with('[')).then_some(album),
                duration_secs: (r.get::<_, i64>(3)? / 1000).max(0) as u32,
            })
        },
    )
    .ok()
}

pub fn forward(
    st: &AppState,
    user: &crate::db::users::User,
    ids: Vec<String>,
    times: Vec<i64>,
    submission: bool,
) {
    let Some(client) = st.lastfm.clone() else {
        return;
    };
    if !user.scrobbling {
        return;
    }
    let (db, crypto, uid, name) = (
        st.db.clone(),
        st.crypto.clone(),
        user.id.clone(),
        user.name.clone(),
    );
    tokio::task::spawn_blocking(move || {
        let Ok(conn) = db.conn() else { return };
        let Ok(Some((sealed, _))) = users::lastfm_session(&conn, &uid) else {
            return;
        };
        let Some(session) = crypto.open(&sealed).and_then(|b| String::from_utf8(b).ok()) else {
            return;
        };
        for (i, id) in ids.iter().enumerate() {
            let Some(song) = load_song(&conn, id) else {
                continue;
            };
            let track = Track {
                artist: &song.artist,
                title: &song.title,
                album: song.album.as_deref(),
                duration_secs: song.duration_secs,
            };
            let result = if submission {
                if song.duration_secs < lastfm::MIN_SCROBBLE_SECS {
                    continue;
                }
                let at = times
                    .get(i)
                    .map(|t| t / 1000)
                    .filter(|t| *t > 0)
                    .unwrap_or_else(crate::db::now);
                client.scrobble(&session, &track, at)
            } else {
                client.now_playing(&session, &track)
            };
            match result {
                Ok(()) => {}
                Err(e)
                    if e.downcast_ref::<ApiFailure>()
                        .is_some_and(|f| f.code == lastfm::INVALID_SESSION) =>
                {
                    tracing::warn!(
                        "last.fm session of {name} is no longer valid, unlinking; link it again"
                    );
                    let _ = users::set_lastfm(&conn, &uid, None);
                    return;
                }
                Err(e) => tracing::warn!("last.fm scrobble for {name} failed: {e:#}"),
            }
            if !submission {
                break;
            }
        }
    });
}
