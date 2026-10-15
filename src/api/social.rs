use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use super::media::serve_file;
use super::model::*;
use super::{ApiResult, Ctx, empty, ok};
use crate::db::now;
use crate::error::ApiError;
use crate::ids;
use crate::state::AppState;
use crate::util::iso;

fn public_base(headers: &HeaderMap) -> String {
    let get = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
    let scheme = get("x-forwarded-proto").unwrap_or("http");
    let host = get("x-forwarded-host")
        .or_else(|| get("host"))
        .unwrap_or("localhost");
    format!("{scheme}://{host}")
}

struct ShareRow {
    id: String,
    owner: String,
    description: String,
    created_at: i64,
    expires_at: Option<i64>,
    last_visited: Option<i64>,
    visit_count: i64,
}

fn expand_songs(conn: &Connection, share_id: &str) -> Result<Vec<String>, ApiError> {
    let mut stmt =
        conn.prepare("SELECT item_id FROM share_entries WHERE share_id = ?1 ORDER BY position")?;
    let items: Vec<String> = stmt
        .query_map([share_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut songs = Vec::new();
    for item in items {
        match kind_of(conn, &item)? {
            Some(ItemKind::Song) => songs.push(item),
            Some(ItemKind::Album) => {
                let mut s = conn.prepare(
                    "SELECT id FROM songs WHERE album_id = ?1 ORDER BY COALESCE(disc, 1), COALESCE(track, 0)",
                )?;
                songs.extend(
                    s.query_map([&item], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                );
            }
            Some(ItemKind::Playlist) => {
                let mut s = conn.prepare(
                    "SELECT song_id FROM playlist_songs WHERE playlist_id = ?1 ORDER BY position",
                )?;
                songs.extend(
                    s.query_map([&item], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                );
            }
            _ => {}
        }
    }
    Ok(songs)
}

fn share_json(conn: &Connection, uid: &str, base: &str, s: &ShareRow) -> Result<Value, ApiError> {
    let ids = expand_songs(conn, &s.id)?;
    let songs = songs_by_ids(conn, uid, &ids)?;
    let mut v = json!({
        "id": s.id,
        "url": format!("{base}/share/{}", s.id),
        "description": s.description,
        "username": s.owner,
        "created": iso(s.created_at),
        "visitCount": s.visit_count,
        "entry": songs_json(conn, &songs)?,
    });
    if let Some(e) = s.expires_at {
        v["expires"] = json!(iso(e));
    }
    if let Some(l) = s.last_visited {
        v["lastVisited"] = json!(iso(l));
    }
    Ok(v)
}

const SHARE_SELECT: &str = "SELECT sh.id, u.name, sh.description, sh.created_at, sh.expires_at, sh.last_visited, sh.visit_count
    FROM shares sh JOIN users u ON u.id = sh.user_id";

fn map_share(r: &rusqlite::Row) -> rusqlite::Result<ShareRow> {
    Ok(ShareRow {
        id: r.get(0)?,
        owner: r.get(1)?,
        description: r.get(2)?,
        created_at: r.get(3)?,
        expires_at: r.get(4)?,
        last_visited: r.get(5)?,
        visit_count: r.get(6)?,
    })
}

fn require_share_role(ctx: &Ctx) -> Result<(), ApiError> {
    let u = ctx.user();
    if u.share || u.admin {
        Ok(())
    } else {
        Err(ApiError::forbidden("user is not authorized to share"))
    }
}

pub async fn get_shares(ctx: Ctx) -> ApiResult {
    let me = ctx.user().clone();
    let base = public_base(&ctx.headers);
    let list = ctx
        .st
        .db
        .run(move |c| {
            let mut stmt = c.prepare(&format!(
                "{SHARE_SELECT} WHERE (sh.user_id = ?1 OR ?2 = 1) ORDER BY sh.created_at DESC"
            ))?;
            let rows = stmt
                .query_map(params![me.id, me.admin], map_share)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.iter()
                .map(|s| share_json(c, &me.id, &base, s))
                .collect::<Result<Vec<_>, _>>()
        })
        .await?;
    ok(json!({"shares": {"share": list}}))
}

pub async fn create_share(ctx: Ctx) -> ApiResult {
    require_share_role(&ctx)?;
    let me = ctx.user().clone();
    let base = public_base(&ctx.headers);
    let items: Vec<String> = ctx
        .params
        .all("id")
        .into_iter()
        .map(str::to_string)
        .collect();
    if items.is_empty() {
        return Err(ApiError::missing("id"));
    }
    let description = ctx.params.get("description").unwrap_or("").to_string();
    let expires = ctx
        .params
        .int("expires")?
        .filter(|e| *e > 0)
        .map(|e| e / 1000);
    let body = ctx
        .st
        .db
        .run(move |c| {
            for item in &items {
                if kind_of(c, item)?.is_none() {
                    return Err(ApiError::not_found("item"));
                }
            }
            let id = ids::random();
            c.execute(
                "INSERT INTO shares(id, user_id, description, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, me.id, description, now(), expires],
            )?;
            for (i, item) in items.iter().enumerate() {
                c.execute(
                    "INSERT INTO share_entries(share_id, position, item_id) VALUES (?1, ?2, ?3)",
                    params![id, i as i64, item],
                )?;
            }
            let row = c.query_row(&format!("{SHARE_SELECT} WHERE sh.id = ?1"), [&id], map_share)?;
            share_json(c, &me.id, &base, &row)
        })
        .await?;
    ok(json!({"shares": {"share": [body]}}))
}

fn owned_share(conn: &Connection, id: &str, me: &crate::db::users::User) -> Result<(), ApiError> {
    let owner: Option<String> = conn
        .query_row("SELECT user_id FROM shares WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    match owner {
        None => Err(ApiError::not_found("share")),
        Some(o) if o == me.id || me.admin => Ok(()),
        Some(_) => Err(ApiError::forbidden("not your share")),
    }
}

pub async fn update_share(ctx: Ctx) -> ApiResult {
    require_share_role(&ctx)?;
    let me = ctx.user().clone();
    let id = ctx.params.req("id")?.to_string();
    let description = ctx.params.get("description").map(str::to_string);
    let expires = ctx.params.int("expires")?;
    ctx.st
        .db
        .run(move |c| {
            owned_share(c, &id, &me)?;
            if let Some(d) = description {
                c.execute(
                    "UPDATE shares SET description = ?2 WHERE id = ?1",
                    params![id, d],
                )?;
            }
            if let Some(e) = expires {
                let v = (e > 0).then_some(e / 1000);
                c.execute(
                    "UPDATE shares SET expires_at = ?2 WHERE id = ?1",
                    params![id, v],
                )?;
            }
            Ok(())
        })
        .await?;
    empty()
}

pub async fn delete_share(ctx: Ctx) -> ApiResult {
    require_share_role(&ctx)?;
    let me = ctx.user().clone();
    let id = ctx.params.req("id")?.to_string();
    ctx.st
        .db
        .run(move |c| {
            owned_share(c, &id, &me)?;
            c.execute("DELETE FROM shares WHERE id = ?1", [&id])?;
            Ok(())
        })
        .await?;
    empty()
}

fn live_share(conn: &Connection, id: &str) -> Result<ShareRow, ApiError> {
    let row = conn
        .query_row(&format!("{SHARE_SELECT} WHERE sh.id = ?1"), [id], map_share)
        .optional()?
        .ok_or_else(|| ApiError::not_found("share"))?;
    if row.expires_at.is_some_and(|e| e < now()) {
        return Err(ApiError::not_found("share"));
    }
    Ok(row)
}

fn plain_error(e: ApiError) -> Response {
    let status = if e.code == 70 {
        axum::http::StatusCode::NOT_FOUND
    } else {
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    };
    (status, e.message).into_response()
}

pub async fn public_share(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    let result = st
        .db
        .run(move |c| {
            let row = live_share(c, &id)?;
            c.execute(
                "UPDATE shares SET visit_count = visit_count + 1, last_visited = ?2 WHERE id = ?1",
                params![row.id, now()],
            )?;
            let ids = expand_songs(c, &row.id)?;
            let mut stmt = c.prepare("SELECT id, title, display_artist, duration_ms FROM songs WHERE id = ?1")?;
            let mut entries = Vec::new();
            for sid in ids {
                if let Some(e) = stmt
                    .query_row([&sid], |r| {
                        Ok(json!({"id": r.get::<_, String>(0)?, "title": r.get::<_, String>(1)?,
                            "artist": r.get::<_, String>(2)?, "duration": r.get::<_, i64>(3)? / 1000,
                            "stream": format!("/share/{}/stream/{}", row.id, sid)}))
                    })
                    .optional()?
                {
                    entries.push(e);
                }
            }
            Ok(json!({"id": row.id, "description": row.description, "entries": entries}))
        })
        .await;
    match result {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => plain_error(e),
    }
}

pub async fn public_share_stream(
    State(st): State<AppState>,
    Path((id, song)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let found = st
        .db
        .run(move |c| {
            let share = live_share(c, &id)?;
            if !expand_songs(c, &share.id)?.contains(&song) {
                return Err(ApiError::not_found("song"));
            }
            c.query_row(
                "SELECT path, content_type FROM songs WHERE id = ?1",
                [&song],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| ApiError::not_found("song"))
        })
        .await;
    match found {
        Ok((path, ct)) => {
            match serve_file(&headers, std::path::Path::new(&path), &ct, None).await {
                Ok(resp) => resp,
                Err(e) => plain_error(e),
            }
        }
        Err(e) => plain_error(e),
    }
}

fn station_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let mut v = json!({"id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?,
        "streamUrl": r.get::<_, String>(2)?});
    if let Some(h) = r.get::<_, Option<String>>(3)? {
        v["homePageUrl"] = json!(h);
    }
    Ok(v)
}

pub async fn get_radio_stations(ctx: Ctx) -> ApiResult {
    let list = ctx
        .st
        .db
        .run(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, stream_url, homepage_url FROM radio_stations ORDER BY name COLLATE NOCASE",
            )?;
            let rows = stmt
                .query_map([], station_json)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?;
    ok(json!({"internetRadioStations": {"internetRadioStation": list}}))
}

pub async fn create_radio_station(ctx: Ctx) -> ApiResult {
    ctx.require_admin()?;
    let url = ctx.params.req("streamUrl")?.to_string();
    let name = ctx.params.req("name")?.to_string();
    let home = ctx.params.get("homepageUrl").map(str::to_string);
    ctx.st
        .db
        .run(move |c| {
            c.execute(
                "INSERT INTO radio_stations(id, name, stream_url, homepage_url) VALUES (?1, ?2, ?3, ?4)",
                params![ids::random(), name, url, home],
            )?;
            Ok(())
        })
        .await?;
    empty()
}

pub async fn update_radio_station(ctx: Ctx) -> ApiResult {
    ctx.require_admin()?;
    let id = ctx.params.req("id")?.to_string();
    let url = ctx.params.req("streamUrl")?.to_string();
    let name = ctx.params.req("name")?.to_string();
    let home = ctx.params.get("homepageUrl").map(str::to_string);
    let changed = ctx
        .st
        .db
        .run(move |c| {
            Ok(c.execute(
                "UPDATE radio_stations SET name = ?2, stream_url = ?3, homepage_url = ?4 WHERE id = ?1",
                params![id, name, url, home],
            )?)
        })
        .await?;
    if changed == 0 {
        return Err(ApiError::not_found("station"));
    }
    empty()
}

pub async fn delete_radio_station(ctx: Ctx) -> ApiResult {
    ctx.require_admin()?;
    let id = ctx.params.req("id")?.to_string();
    let changed = ctx
        .st
        .db
        .run(move |c| Ok(c.execute("DELETE FROM radio_stations WHERE id = ?1", [id])?))
        .await?;
    if changed == 0 {
        return Err(ApiError::not_found("station"));
    }
    empty()
}

pub async fn get_chat_messages(ctx: Ctx) -> ApiResult {
    let since = ctx.params.int_or("since", 0)?.max(0) / 1000;
    let list = ctx
        .st
        .db
        .run(move |c| {
            let mut stmt = c.prepare(
                "SELECT u.name, m.created_at, m.message FROM chat_messages m JOIN users u ON u.id = m.user_id
                 WHERE m.created_at > ?1 ORDER BY m.created_at DESC, m.id DESC LIMIT 100",
            )?;
            let mut rows = stmt
                .query_map([since], |r| {
                    Ok(json!({"username": r.get::<_, String>(0)?, "time": r.get::<_, i64>(1)? * 1000,
                        "message": r.get::<_, String>(2)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.reverse();
            Ok(rows)
        })
        .await?;
    ok(json!({"chatMessages": {"chatMessage": list}}))
}

pub async fn add_chat_message(ctx: Ctx) -> ApiResult {
    let message = ctx
        .params
        .req("message")?
        .chars()
        .take(2000)
        .collect::<String>();
    let uid = ctx.user().id.clone();
    ctx.st
        .db
        .run(move |c| {
            c.execute(
                "INSERT INTO chat_messages(user_id, created_at, message) VALUES (?1, ?2, ?3)",
                params![uid, now(), message],
            )?;
            Ok(())
        })
        .await?;
    empty()
}
