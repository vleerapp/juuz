use serde_json::json;

use super::scrobble::load_song;
use super::{ApiResult, Ctx, empty, ok};
use crate::server::utils::db::users;
use crate::server::utils::error::ApiError;
use crate::server::utils::listenbrainz::{self, Client, Rejected, Track};
use crate::server::utils::state::AppState;

fn upstream(e: anyhow::Error) -> ApiError {
    match e.downcast_ref::<Rejected>() {
        Some(f) if f.is_auth() => ApiError::generic("listenbrainz: token rejected"),
        Some(f) => ApiError::generic(format!("listenbrainz: {}", f.message)),
        None => ApiError::generic(format!("listenbrainz request failed: {e}")),
    }
}

pub async fn status(ctx: Ctx) -> ApiResult {
    let uid = ctx.user().id.clone();
    let linked = ctx
        .st
        .db
        .run(move |c| Ok(users::listenbrainz_link(c, &uid)?))
        .await?;
    ok(json!({"juuzListenbrainz": {
        "linked": linked.is_some(),
        "url": linked.map(|l| l.url),
    }}))
}

pub async fn link(ctx: Ctx) -> ApiResult {
    let token = ctx.params.req("token")?.trim().to_string();
    if token.is_empty() {
        return Err(ApiError::missing("token"));
    }
    let url = listenbrainz::normalize_url(ctx.params.get("url").unwrap_or(""))
        .ok_or_else(|| ApiError::generic("url must start with http:// or https://"))?;
    link_account(&ctx.st, &ctx.user().id, &url, &token)
        .await
        .map_err(upstream)?;
    ok(json!({"juuzListenbrainz": {"linked": true, "url": url}}))
}

pub async fn link_account(
    st: &AppState,
    user_id: &str,
    url: &str,
    token: &str,
) -> anyhow::Result<()> {
    let client = Client::new(url, token);
    tokio::task::spawn_blocking(move || client.validate()).await??;
    let sealed = st.crypto.seal(token.as_bytes());
    let (uid, stored_url) = (user_id.to_string(), url.to_string());
    st.db
        .run(move |c| {
            Ok(users::set_listenbrainz(
                c,
                &uid,
                Some((&stored_url, &sealed)),
            )?)
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    Ok(())
}

pub async fn unlink(ctx: Ctx) -> ApiResult {
    let uid = ctx.user().id.clone();
    ctx.st
        .db
        .run(move |c| Ok(users::set_listenbrainz(c, &uid, None)?))
        .await?;
    empty()
}

pub fn forward(
    st: &AppState,
    user: &crate::server::utils::db::users::User,
    ids: Vec<String>,
    times: Vec<i64>,
    submission: bool,
) {
    let (db, crypto, uid, name) = (
        st.db.clone(),
        st.crypto.clone(),
        user.id.clone(),
        user.name.clone(),
    );
    tokio::task::spawn_blocking(move || {
        let Ok(conn) = db.conn() else { return };
        let Ok(Some(link)) = users::listenbrainz_link(&conn, &uid) else {
            return;
        };
        let Some(token) = crypto
            .open(&link.token)
            .and_then(|b| String::from_utf8(b).ok())
        else {
            return;
        };
        let client = Client::new(&link.url, &token);
        let songs: Vec<_> = ids
            .iter()
            .enumerate()
            .filter_map(|(i, id)| Some((i, load_song(&conn, id)?)))
            .collect();
        let result = if submission {
            let listens: Vec<_> = songs
                .iter()
                .filter(|(_, s)| s.duration_secs >= listenbrainz::MIN_SCROBBLE_SECS)
                .map(|(i, s)| {
                    let at = times
                        .get(*i)
                        .map(|t| t / 1000)
                        .filter(|t| *t > 0)
                        .unwrap_or_else(crate::server::utils::db::now);
                    (
                        Track {
                            artist: &s.artist,
                            title: &s.title,
                            album: s.album.as_deref(),
                            duration_secs: s.duration_secs,
                        },
                        at,
                    )
                })
                .collect();
            if listens.is_empty() {
                return;
            }
            client.scrobble(&listens)
        } else {
            let Some((_, s)) = songs.first() else { return };
            client.now_playing(&Track {
                artist: &s.artist,
                title: &s.title,
                album: s.album.as_deref(),
                duration_secs: s.duration_secs,
            })
        };
        if let Err(e) = result {
            if e.downcast_ref::<Rejected>().is_some_and(Rejected::is_auth) {
                tracing::warn!(
                    "listenbrainz token of {name} is no longer valid, unlinking; link it again"
                );
                let _ = users::set_listenbrainz(&conn, &uid, None);
            } else {
                tracing::warn!("listenbrainz scrobble for {name} failed: {e:#}");
            }
        }
    });
}
