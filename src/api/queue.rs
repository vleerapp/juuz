use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use super::model::*;
use super::{ApiResult, Ctx, empty, ok};
use crate::db::now;
use crate::error::ApiError;
use crate::util::iso;

type QueueHead = (Option<String>, Option<i64>, i64, i64, String);

async fn save(ctx: Ctx, by_index: bool) -> ApiResult {
    let ids: Vec<String> = ctx
        .params
        .all("id")
        .into_iter()
        .map(str::to_string)
        .collect();
    let uid = ctx.user().id.clone();
    let client = ctx.params.get("c").unwrap_or("").to_string();
    let position = ctx.params.int_or("position", 0)?.max(0);
    let current_id = ctx.params.get("current").map(str::to_string);
    let current_index = ctx.params.int("currentIndex")?;
    ctx.st
        .db
        .run(move |c| {
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            for id in &ids {
                if kind_of_for(&tx, &uid, id)? != Some(ItemKind::Song) {
                    return Err(ApiError::not_found("song"));
                }
            }
            let (cur_id, cur_idx) = if by_index {
                let idx = current_index.filter(|i| *i >= 0 && (*i as usize) < ids.len());
                (idx.map(|i| ids[i as usize].clone()), idx)
            } else {
                let idx = current_id
                    .as_ref()
                    .and_then(|cid| ids.iter().position(|i| i == cid))
                    .map(|i| i as i64);
                (current_id.clone(), idx)
            };
            tx.execute("DELETE FROM play_queue_songs WHERE user_id = ?1", [&uid])?;
            for (i, id) in ids.iter().enumerate() {
                tx.execute(
                    "INSERT INTO play_queue_songs(user_id, position, song_id) VALUES (?1, ?2, ?3)",
                    params![uid, i as i64, id],
                )?;
            }
            tx.execute(
                "INSERT INTO play_queue(user_id, current_id, current_index, position_ms, changed_at, changed_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(user_id) DO UPDATE SET current_id = excluded.current_id,
                    current_index = excluded.current_index, position_ms = excluded.position_ms,
                    changed_at = excluded.changed_at, changed_by = excluded.changed_by",
                params![uid, cur_id, cur_idx, position, now(), client],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    empty()
}

pub async fn save_play_queue(ctx: Ctx) -> ApiResult {
    save(ctx, false).await
}

pub async fn save_play_queue_by_index(ctx: Ctx) -> ApiResult {
    save(ctx, true).await
}

async fn load(ctx: Ctx, by_index: bool) -> ApiResult {
    let user = ctx.user().clone();
    let key = if by_index {
        "playQueueByIndex"
    } else {
        "playQueue"
    };
    let body: Option<Value> = ctx
        .st
        .db
        .run(move |c| {
            let head: Option<QueueHead> = c
                .query_row(
                    "SELECT current_id, current_index, position_ms, changed_at, changed_by FROM play_queue WHERE user_id = ?1",
                    [&user.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?;
            let Some((cur_id, cur_idx, position, changed, by)) = head else {
                return Ok(None);
            };
            let mut stmt = c.prepare(
                "SELECT song_id FROM play_queue_songs WHERE user_id = ?1 ORDER BY position",
            )?;
            let ids: Vec<String> = stmt
                .query_map([&user.id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let songs = songs_by_ids(c, &user.id, &ids)?;
            let mut v = json!({
                "position": position,
                "username": user.name,
                "changed": iso(changed),
                "changedBy": by,
                "entry": songs_json(c, &songs)?,
            });
            if by_index {
                if let Some(i) = cur_idx {
                    v["currentIndex"] = json!(i);
                }
            } else if let Some(id) = cur_id {
                v["current"] = json!(id);
            }
            Ok(Some(v))
        })
        .await?;
    match body {
        Some(v) => ok(json!({ key: v })),
        None => ok(json!({ key: {} })),
    }
}

pub async fn get_play_queue(ctx: Ctx) -> ApiResult {
    load(ctx, false).await
}

pub async fn get_play_queue_by_index(ctx: Ctx) -> ApiResult {
    load(ctx, true).await
}

pub async fn get_bookmarks(ctx: Ctx) -> ApiResult {
    let user = ctx.user().clone();
    let list = ctx
        .st
        .db
        .run(move |c| {
            let mut stmt = c.prepare(
                "SELECT song_id, position_ms, comment, created_at, changed_at FROM bookmarks
                 WHERE user_id = ?1 ORDER BY changed_at DESC",
            )?;
            let rows: Vec<(String, i64, String, i64, i64)> = stmt
                .query_map([&user.id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let ids: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
            let songs = songs_by_ids(c, &user.id, &ids)?;
            let children = songs_json(c, &songs)?;
            let mut out = Vec::new();
            for (song, child) in songs.iter().zip(children) {
                if let Some((_, pos, comment, created, changed)) =
                    rows.iter().find(|r| r.0 == song.id)
                {
                    out.push(json!({
                        "position": pos, "username": user.name, "comment": comment,
                        "created": iso(*created), "changed": iso(*changed), "entry": child,
                    }));
                }
            }
            Ok(out)
        })
        .await?;
    ok(json!({"bookmarks": {"bookmark": list}}))
}

pub async fn create_bookmark(ctx: Ctx) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let position = ctx
        .params
        .int("position")?
        .ok_or_else(|| ApiError::missing("position"))?
        .max(0);
    let comment = ctx.params.get("comment").unwrap_or("").to_string();
    let uid = ctx.user().id.clone();
    ctx.st
        .db
        .run(move |c| {
            if kind_of_for(c, &uid, &id)? != Some(ItemKind::Song) {
                return Err(ApiError::not_found("song"));
            }
            c.execute(
                "INSERT INTO bookmarks(user_id, song_id, position_ms, comment, created_at, changed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT(user_id, song_id) DO UPDATE SET position_ms = excluded.position_ms,
                    comment = excluded.comment, changed_at = excluded.changed_at",
                params![uid, id, position, comment, now()],
            )?;
            Ok(())
        })
        .await?;
    empty()
}

pub async fn delete_bookmark(ctx: Ctx) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let uid = ctx.user().id.clone();
    ctx.st
        .db
        .run(move |c| {
            c.execute(
                "DELETE FROM bookmarks WHERE user_id = ?1 AND song_id = ?2",
                params![uid, id],
            )?;
            Ok(())
        })
        .await?;
    empty()
}
