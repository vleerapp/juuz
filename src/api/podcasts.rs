use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use super::{ApiResult, Ctx, empty, ok};
use crate::error::ApiError;
use crate::podcast;
use crate::util::iso;

fn require(ctx: &Ctx) -> Result<(), ApiError> {
    if !ctx.st.cfg.podcasts.enabled {
        return Err(ApiError::generic("podcasts are disabled on this server"));
    }
    let u = ctx.user();
    if u.podcast || u.admin {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "user is not authorized to manage podcasts",
        ))
    }
}

fn suffix_of(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
}

type ChannelRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

type EpisodeRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
);

fn episode_json(r: &EpisodeRow, channel_title: &str) -> Value {
    let (
        id,
        channel_id,
        _guid,
        title,
        description,
        published,
        duration,
        size,
        ctype,
        status,
        error,
        path,
    ) = r;
    let mut v = json!({
        "id": id,
        "parent": channel_id,
        "channelId": channel_id,
        "isDir": false,
        "isVideo": false,
        "type": "podcast",
        "title": title,
        "album": channel_title,
        "artist": channel_title,
        "genre": "Podcast",
        "status": status,
    });
    if let Some(d) = description {
        v["description"] = json!(d);
    }
    if let Some(p) = published {
        v["publishDate"] = json!(iso(*p));
        v["created"] = json!(iso(*p));
    }
    if let Some(d) = duration {
        v["duration"] = json!(d);
    }
    if let Some(s) = size {
        v["size"] = json!(s);
    }
    if let Some(c) = ctype {
        v["contentType"] = json!(c);
    }
    if let Some(e) = error {
        v["errorMessage"] = json!(e);
    }
    if status == "completed" {
        v["streamId"] = json!(id);
        if let Some(p) = path
            && let Some(s) = suffix_of(p)
        {
            v["suffix"] = json!(s);
        }
    }
    v
}

const EPISODE_COLS: &str =
    "e.id, e.channel_id, e.guid, e.title, e.description, e.published_at, e.duration_s,
    e.size, e.content_type, e.status, e.error, e.path";

fn map_episode(r: &rusqlite::Row) -> rusqlite::Result<EpisodeRow> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
        r.get(10)?,
        r.get(11)?,
    ))
}

fn channels_json(
    conn: &Connection,
    only: Option<&str>,
    episodes: bool,
) -> Result<Vec<Value>, ApiError> {
    let mut stmt = conn.prepare(
        "SELECT id, url, title, description, image_url, status, error FROM podcast_channels
         WHERE (?1 IS NULL OR id = ?1) ORDER BY COALESCE(title, url) COLLATE NOCASE",
    )?;
    let rows: Vec<ChannelRow> = stmt
        .query_map([only], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (id, url, title, description, image, status, error) in rows {
        let mut v = json!({"id": id, "url": url, "status": status});
        if let Some(t) = &title {
            v["title"] = json!(t);
        }
        if let Some(d) = description {
            v["description"] = json!(d);
        }
        if let Some(i) = image {
            v["originalImageUrl"] = json!(i);
        }
        if let Some(e) = error {
            v["errorMessage"] = json!(e);
        }
        if episodes {
            let mut stmt = conn.prepare(&format!(
                "SELECT {EPISODE_COLS} FROM podcast_episodes e WHERE e.channel_id = ?1 AND e.status <> 'deleted'
                 ORDER BY e.published_at DESC"
            ))?;
            let eps: Vec<EpisodeRow> = stmt
                .query_map([&id], map_episode)?
                .collect::<rusqlite::Result<_>>()?;
            let name = title.clone().unwrap_or_default();
            v["episode"] = json!(
                eps.iter()
                    .map(|e| episode_json(e, &name))
                    .collect::<Vec<_>>()
            );
        }
        out.push(v);
    }
    Ok(out)
}

pub async fn get_podcasts(ctx: Ctx) -> ApiResult {
    let episodes = ctx.params.bool("includeEpisodes").unwrap_or(true);
    let only = ctx.params.get("id").map(str::to_string);
    let list = ctx
        .st
        .db
        .run(move |c| channels_json(c, only.as_deref(), episodes))
        .await?;
    ok(json!({"podcasts": {"channel": list}}))
}

pub async fn get_newest_podcasts(ctx: Ctx) -> ApiResult {
    let count = ctx.params.int_or("count", 20)?.clamp(1, 500);
    let list = ctx
        .st
        .db
        .run(move |c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {EPISODE_COLS}, COALESCE(ch.title, '') FROM podcast_episodes e
                 JOIN podcast_channels ch ON ch.id = e.channel_id WHERE e.status <> 'deleted'
                 ORDER BY e.published_at DESC LIMIT {count}"
            ))?;
            let rows = stmt
                .query_map([], |r| Ok((map_episode(r)?, r.get::<_, String>(12)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows
                .iter()
                .map(|(e, title)| episode_json(e, title))
                .collect::<Vec<_>>())
        })
        .await?;
    ok(json!({"newestPodcasts": {"episode": list}}))
}

pub async fn get_podcast_episode(ctx: Ctx) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let body = ctx
        .st
        .db
        .run(move |c| {
            let row = c
                .query_row(
                    &format!(
                        "SELECT {EPISODE_COLS}, COALESCE(ch.title, '') FROM podcast_episodes e
                         JOIN podcast_channels ch ON ch.id = e.channel_id WHERE e.id = ?1"
                    ),
                    [&id],
                    |r| Ok((map_episode(r)?, r.get::<_, String>(12)?)),
                )
                .optional()?
                .ok_or_else(|| ApiError::not_found("episode"))?;
            Ok(episode_json(&row.0, &row.1))
        })
        .await?;
    ok(json!({"podcastEpisode": body}))
}

pub async fn create_channel(ctx: Ctx) -> ApiResult {
    require(&ctx)?;
    let url = ctx.params.req("url")?.trim().to_string();
    let id = {
        let url = url.clone();
        ctx.st
            .db
            .run(move |c| {
                if c.query_row(
                    "SELECT 1 FROM podcast_channels WHERE url = ?1",
                    [&url],
                    |_| Ok(()),
                )
                .optional()?
                .is_some()
                {
                    return Err(ApiError::generic("podcast channel already exists"));
                }
                podcast::create_channel(c, &url).map_err(|e| ApiError::generic(e.to_string()))
            })
            .await?
    };
    let (db, fetcher) = (ctx.st.db.clone(), ctx.st.podcasts.clone());
    tokio::task::spawn_blocking(move || podcast::refresh_channel(&db, &fetcher, &id));
    empty()
}

pub async fn refresh(ctx: Ctx) -> ApiResult {
    require(&ctx)?;
    let (db, fetcher) = (ctx.st.db.clone(), ctx.st.podcasts.clone());
    tokio::task::spawn_blocking(move || {
        let ids: Vec<String> = match db.conn().and_then(|c| {
            let mut stmt = c.prepare("SELECT id FROM podcast_channels")?;
            Ok(stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?)
        }) {
            Ok(ids) => ids,
            Err(_) => return,
        };
        for id in ids {
            podcast::refresh_channel(&db, &fetcher, &id);
        }
    });
    empty()
}

pub async fn delete_channel(ctx: Ctx) -> ApiResult {
    require(&ctx)?;
    let id = ctx.params.req("id")?.to_string();
    let dir = ctx.st.cfg.podcast_dir().join(&id);
    let removed = ctx
        .st
        .db
        .run(move |c| Ok(c.execute("DELETE FROM podcast_channels WHERE id = ?1", [id])?))
        .await?;
    if removed == 0 {
        return Err(ApiError::not_found("channel"));
    }
    let _ = tokio::fs::remove_dir_all(dir).await;
    empty()
}

pub async fn delete_episode(ctx: Ctx) -> ApiResult {
    require(&ctx)?;
    let id = ctx.params.req("id")?.to_string();
    let path = ctx
        .st
        .db
        .run(move |c| {
            let path: Option<Option<String>> = c
                .query_row(
                    "SELECT path FROM podcast_episodes WHERE id = ?1",
                    [&id],
                    |r| r.get(0),
                )
                .optional()?;
            let path = path.ok_or_else(|| ApiError::not_found("episode"))?;
            c.execute(
                "UPDATE podcast_episodes SET status = 'deleted', path = NULL WHERE id = ?1",
                params![id],
            )?;
            Ok(path)
        })
        .await?;
    if let Some(p) = path {
        let _ = tokio::fs::remove_file(p).await;
    }
    empty()
}

pub async fn download_episode(ctx: Ctx) -> ApiResult {
    require(&ctx)?;
    let id = ctx.params.req("id")?.to_string();
    let exists = {
        let id = id.clone();
        ctx.st
            .db
            .run(move |c| {
                Ok(
                    c.query_row("SELECT 1 FROM podcast_episodes WHERE id = ?1", [id], |_| {
                        Ok(())
                    })
                    .optional()?
                    .is_some(),
                )
            })
            .await?
    };
    if !exists {
        return Err(ApiError::not_found("episode"));
    }
    let (db, fetcher, dir) = (
        ctx.st.db.clone(),
        ctx.st.podcasts.clone(),
        ctx.st.cfg.podcast_dir(),
    );
    tokio::task::spawn_blocking(move || podcast::download_episode(&db, &fetcher, &dir, &id));
    empty()
}
