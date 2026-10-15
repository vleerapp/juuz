use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use super::model::*;
use super::{ApiResult, Ctx, empty, ok};
use crate::db::now;
use crate::db::users::User;
use crate::error::ApiError;
use crate::ids;
use crate::util::iso;

struct PlaylistRow {
    id: String,
    name: String,
    comment: String,
    public: bool,
    owner_id: String,
    owner: String,
    created_at: i64,
    changed_at: i64,
    song_count: i64,
    duration_ms: i64,
}

const SELECT: &str =
    "SELECT p.id, p.name, p.comment, p.public, p.owner_id, u.name, p.created_at, p.changed_at,
    (SELECT COUNT(*) FROM playlist_songs ps WHERE ps.playlist_id = p.id),
    (SELECT COALESCE(SUM(s.duration_ms), 0) FROM playlist_songs ps JOIN songs s ON s.id = ps.song_id
        WHERE ps.playlist_id = p.id)
    FROM playlists p JOIN users u ON u.id = p.owner_id";

fn map(r: &rusqlite::Row) -> rusqlite::Result<PlaylistRow> {
    Ok(PlaylistRow {
        id: r.get(0)?,
        name: r.get(1)?,
        comment: r.get(2)?,
        public: r.get(3)?,
        owner_id: r.get(4)?,
        owner: r.get(5)?,
        created_at: r.get(6)?,
        changed_at: r.get(7)?,
        song_count: r.get(8)?,
        duration_ms: r.get(9)?,
    })
}

fn row_json(p: &PlaylistRow) -> Value {
    let mut v = json!({
        "id": p.id,
        "name": p.name,
        "comment": p.comment,
        "owner": p.owner,
        "public": p.public,
        "songCount": p.song_count,
        "duration": p.duration_ms / 1000,
        "created": iso(p.created_at),
        "changed": iso(p.changed_at),
    });
    if p.song_count > 0 {
        v["coverArt"] = json!(p.id);
    }
    v
}

fn fetch(conn: &Connection, id: &str, user: &User) -> Result<PlaylistRow, ApiError> {
    let row = conn
        .query_row(&format!("{SELECT} WHERE p.id = ?1"), [id], map)
        .optional()?
        .ok_or_else(|| ApiError::not_found("playlist"))?;
    if row.owner_id != user.id && !row.public && !user.admin {
        return Err(ApiError::forbidden("playlist is not accessible"));
    }
    Ok(row)
}

fn owned(conn: &Connection, id: &str, user: &User) -> Result<PlaylistRow, ApiError> {
    let row = fetch(conn, id, user)?;
    if row.owner_id != user.id && !user.admin {
        return Err(ApiError::forbidden(
            "only the owner can modify this playlist",
        ));
    }
    Ok(row)
}

fn song_ids(conn: &Connection, id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT song_id FROM playlist_songs WHERE playlist_id = ?1 ORDER BY position")?;
    stmt.query_map([id], |r| r.get(0))?.collect()
}

fn write_songs(conn: &Connection, id: &str, songs: &[String]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM playlist_songs WHERE playlist_id = ?1", [id])?;
    for (i, s) in songs.iter().enumerate() {
        conn.execute(
            "INSERT INTO playlist_songs(playlist_id, position, song_id) VALUES (?1, ?2, ?3)",
            params![id, i as i64, s],
        )?;
    }
    conn.execute(
        "UPDATE playlists SET changed_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

fn existing_songs(conn: &Connection, ids: &[String]) -> Result<Vec<String>, ApiError> {
    for id in ids {
        if kind_of(conn, id)? != Some(ItemKind::Song) {
            return Err(ApiError::not_found("song"));
        }
    }
    Ok(ids.to_vec())
}

fn full(conn: &Connection, id: &str, user: &User) -> Result<Value, ApiError> {
    let row = fetch(conn, id, user)?;
    let ids = song_ids(conn, id)?;
    let songs = songs_by_ids(conn, &user.id, &ids)?;
    let mut v = row_json(&row);
    v["entry"] = json!(songs_json(conn, &songs)?);
    Ok(v)
}

pub async fn get_playlists(ctx: Ctx) -> ApiResult {
    let me = ctx.user().clone();
    let target = ctx.params.get("username").map(str::to_string);
    let list = ctx
        .st
        .db
        .run(move |c| {
            let mut stmt = c.prepare(&format!(
                "{SELECT} WHERE (p.owner_id = ?1 OR p.public = 1 OR ?2 = 1) AND (?3 IS NULL OR u.name = ?3 COLLATE NOCASE)
                 ORDER BY p.name COLLATE NOCASE"
            ))?;
            let rows = stmt
                .query_map(params![me.id, me.admin, target], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows.iter().map(row_json).collect::<Vec<_>>())
        })
        .await?;
    ok(json!({"playlists": {"playlist": list}}))
}

pub async fn get_playlist(ctx: Ctx) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let me = ctx.user().clone();
    let body = ctx.st.db.run(move |c| full(c, &id, &me)).await?;
    ok(json!({"playlist": body}))
}

pub async fn create_playlist(ctx: Ctx) -> ApiResult {
    let me = ctx.user().clone();
    if !(me.playlist || me.admin) {
        return Err(ApiError::forbidden(
            "user is not authorized to manage playlists",
        ));
    }
    let existing = ctx.params.get("playlistId").map(str::to_string);
    let name = ctx.params.get("name").map(str::to_string);
    let songs: Vec<String> = ctx
        .params
        .all("songId")
        .into_iter()
        .map(str::to_string)
        .collect();
    if existing.is_none() && name.is_none() {
        return Err(ApiError::missing("name"));
    }
    let body = ctx
        .st
        .db
        .run(move |c| {
            let songs = existing_songs(c, &songs)?;
            let id = match existing {
                Some(id) => {
                    owned(c, &id, &me)?;
                    if let Some(n) = &name {
                        c.execute("UPDATE playlists SET name = ?2 WHERE id = ?1", params![id, n])?;
                    }
                    write_songs(c, &id, &songs)?;
                    id
                }
                None => {
                    let id = ids::random();
                    let ts = now();
                    c.execute(
                        "INSERT INTO playlists(id, owner_id, name, created_at, changed_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                        params![id, me.id, name.unwrap_or_default(), ts],
                    )?;
                    write_songs(c, &id, &songs)?;
                    id
                }
            };
            full(c, &id, &me)
        })
        .await?;
    ok(json!({"playlist": body}))
}

pub async fn update_playlist(ctx: Ctx) -> ApiResult {
    let me = ctx.user().clone();
    let id = ctx.params.req("playlistId")?.to_string();
    let name = ctx.params.get("name").map(str::to_string);
    let comment = ctx.params.get("comment").map(str::to_string);
    let public = ctx.params.bool("public");
    let add: Vec<String> = ctx
        .params
        .all("songIdToAdd")
        .into_iter()
        .map(str::to_string)
        .collect();
    let remove = ctx.params.ints("songIndexToRemove")?;
    ctx.st
        .db
        .run(move |c| {
            owned(c, &id, &me)?;
            if let Some(n) = name {
                c.execute(
                    "UPDATE playlists SET name = ?2 WHERE id = ?1",
                    params![id, n],
                )?;
            }
            if let Some(n) = comment {
                c.execute(
                    "UPDATE playlists SET comment = ?2 WHERE id = ?1",
                    params![id, n],
                )?;
            }
            if let Some(p) = public {
                c.execute(
                    "UPDATE playlists SET public = ?2 WHERE id = ?1",
                    params![id, p],
                )?;
            }
            if !add.is_empty() || !remove.is_empty() {
                let mut songs = song_ids(c, &id)?;
                let mut drop: Vec<usize> = remove
                    .iter()
                    .filter(|i| **i >= 0 && (**i as usize) < songs.len())
                    .map(|i| *i as usize)
                    .collect();
                drop.sort_unstable();
                drop.dedup();
                for i in drop.into_iter().rev() {
                    songs.remove(i);
                }
                songs.extend(existing_songs(c, &add)?);
                write_songs(c, &id, &songs)?;
            }
            c.execute(
                "UPDATE playlists SET changed_at = ?2 WHERE id = ?1",
                params![id, now()],
            )?;
            Ok(())
        })
        .await?;
    empty()
}

pub async fn delete_playlist(ctx: Ctx) -> ApiResult {
    let me = ctx.user().clone();
    let id = ctx.params.req("id")?.to_string();
    ctx.st
        .db
        .run(move |c| {
            owned(c, &id, &me)?;
            c.execute("DELETE FROM playlists WHERE id = ?1", [&id])?;
            Ok(())
        })
        .await?;
    empty()
}
