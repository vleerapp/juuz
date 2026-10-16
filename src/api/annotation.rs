use serde_json::json;

use super::model::kind_of;
use super::{ApiResult, Ctx, empty, ok};
use crate::db::now;
use crate::error::ApiError;
use crate::state::NowPlaying;
use crate::util::iso;

fn item_ids(ctx: &Ctx) -> Vec<String> {
    ["id", "albumId", "artistId"]
        .iter()
        .flat_map(|k| ctx.params.all(k))
        .map(str::to_string)
        .collect()
}

async fn set_starred(ctx: Ctx, starred: bool) -> ApiResult {
    let ids = item_ids(&ctx);
    if ids.is_empty() {
        return Err(ApiError::missing("id"));
    }
    let uid = ctx.user().id.clone();
    ctx.st
        .db
        .run(move |c| {
            let tx = c.transaction()?;
            for id in &ids {
                if kind_of(&tx, id)?.is_none() {
                    return Err(ApiError::not_found("item"));
                }
                if starred {
                    tx.execute(
                        "INSERT OR IGNORE INTO starred(user_id, item_id, starred_at) VALUES (?1, ?2, ?3)",
                        rusqlite::params![uid, id, now()],
                    )?;
                } else {
                    tx.execute(
                        "DELETE FROM starred WHERE user_id = ?1 AND item_id = ?2",
                        rusqlite::params![uid, id],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    empty()
}

pub async fn star(ctx: Ctx) -> ApiResult {
    set_starred(ctx, true).await
}

pub async fn unstar(ctx: Ctx) -> ApiResult {
    set_starred(ctx, false).await
}

pub async fn set_rating(ctx: Ctx) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let rating = ctx
        .params
        .int("rating")?
        .ok_or_else(|| ApiError::missing("rating"))?;
    if !(0..=5).contains(&rating) {
        return Err(ApiError::bad_param("rating"));
    }
    let uid = ctx.user().id.clone();
    ctx.st
        .db
        .run(move |c| {
            if kind_of(c, &id)?.is_none() {
                return Err(ApiError::not_found("item"));
            }
            if rating == 0 {
                c.execute(
                    "DELETE FROM ratings WHERE user_id = ?1 AND item_id = ?2",
                    rusqlite::params![uid, id],
                )?;
            } else {
                c.execute(
                    "INSERT INTO ratings(user_id, item_id, rating) VALUES (?1, ?2, ?3)
                     ON CONFLICT(user_id, item_id) DO UPDATE SET rating = excluded.rating",
                    rusqlite::params![uid, id, rating],
                )?;
            }
            Ok(())
        })
        .await?;
    empty()
}

pub fn record_play(
    conn: &rusqlite::Connection,
    user_id: &str,
    song_id: &str,
    at: i64,
) -> rusqlite::Result<bool> {
    let duration_secs: i64 = conn
        .query_row(
            "SELECT duration_ms / 1000 FROM songs WHERE id = ?1",
            [song_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let window = (duration_secs * 3 / 4).max(30);
    let duplicate: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM scrobbles
         WHERE user_id = ?1 AND song_id = ?2 AND ABS(played_at - ?3) < ?4)",
        rusqlite::params![user_id, song_id, at, window],
        |r| r.get(0),
    )?;
    if duplicate {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO play_stats(user_id, song_id, play_count, last_played) VALUES (?1, ?2, 1, ?3)
         ON CONFLICT(user_id, song_id) DO UPDATE SET play_count = play_count + 1,
             last_played = MAX(last_played, excluded.last_played)",
        rusqlite::params![user_id, song_id, at],
    )?;
    conn.execute(
        "INSERT INTO scrobbles(user_id, song_id, played_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![user_id, song_id, at],
    )?;
    Ok(true)
}

pub async fn scrobble(ctx: Ctx) -> ApiResult {
    let ids: Vec<String> = ctx
        .params
        .all("id")
        .into_iter()
        .map(str::to_string)
        .collect();
    if ids.is_empty() {
        return Err(ApiError::missing("id"));
    }
    let times = ctx.params.ints("time")?;
    let submission = ctx.params.bool("submission").unwrap_or(true);
    let user = ctx.user().clone();
    let player = ctx.params.get("c").unwrap_or("unknown").to_string();
    tracing::debug!(
        "scrobble from {player} submission={submission} songs={}",
        ids.len()
    );

    if !submission {
        let id = ids[0].clone();
        let exists = ctx
            .st
            .db
            .run(move |c| Ok(kind_of(c, &id)? == Some(super::model::ItemKind::Song)))
            .await?;
        if !exists {
            return Err(ApiError::not_found("song"));
        }
        super::lastfm::forward(&ctx.st, &user, vec![ids[0].clone()], Vec::new(), false);
        ctx.st
            .now_playing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                (user.id.clone(), player.clone()),
                NowPlaying {
                    user_id: user.id.clone(),
                    song_id: ids[0].clone(),
                    username: user.name.clone(),
                    player,
                    started: now(),
                    until: now() + crate::state::NOW_PLAYING_TTL,
                },
            );
        ctx.st.live.notify();
        return empty();
    }

    let uid = user.id.clone();
    let record = user.scrobbling;
    let fresh = ctx
        .st
        .db
        .run(move |c| {
            let tx = c.transaction()?;
            let mut fresh = Vec::new();
            for (i, id) in ids.iter().enumerate() {
                if kind_of(&tx, id)? != Some(super::model::ItemKind::Song) {
                    return Err(ApiError::not_found("song"));
                }
                if !record {
                    continue;
                }
                let at = times
                    .get(i)
                    .map(|t| t / 1000)
                    .filter(|t| *t > 0)
                    .unwrap_or_else(now);
                if record_play(&tx, &uid, id, at)? {
                    fresh.push((id.clone(), at * 1000));
                }
            }
            tx.commit()?;
            Ok(fresh)
        })
        .await?;
    let (forward_ids, forward_times) = fresh.into_iter().unzip();
    super::lastfm::forward(&ctx.st, &user, forward_ids, forward_times, true);
    ctx.st
        .now_playing
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(user.id.clone(), player));
    ctx.st.live.notify();
    empty()
}

pub async fn start_scan(ctx: Ctx) -> ApiResult {
    ctx.require_admin()?;
    let full = ctx.params.bool("full").unwrap_or(false);
    ctx.st.scanner.start(full);
    scan_status(ctx).await
}

pub async fn scan_status(ctx: Ctx) -> ApiResult {
    let status = ctx.st.scanner.status()?;
    let mut body = json!({"scanning": status.scanning, "count": status.count, "folderCount": status.folder_count});
    if let Some(t) = status.last_scan {
        body["lastScan"] = json!(iso(t));
    }
    ok(json!({"scanStatus": body}))
}
