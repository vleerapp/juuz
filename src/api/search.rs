use rusqlite::types::Value as Sql;
use rusqlite::{Connection, params_from_iter};
use serde_json::{Value, json};

use super::model::*;
use super::{ApiResult, Ctx, ok};
use crate::db::libraries::{artist_visible, visible};
use crate::error::ApiError;
use crate::search::{self, Mode, Query};

struct Page {
    count: i64,
    offset: i64,
    folder: Option<i64>,
}

fn ids_for(
    conn: &Connection,
    uid: &str,
    kind: &str,
    query: &Option<Query>,
    page: &Page,
) -> Result<Vec<String>, ApiError> {
    if page.count <= 0 {
        return Ok(Vec::new());
    }
    if let Some(q) = query {
        let ranked = match kind {
            "artist" => q.rank(Mode::Plain, &search::artist_docs(conn, uid, page.folder)?),
            "album" => q.rank(Mode::Plain, &search::album_docs(conn, uid, page.folder)?),
            _ => q.rank(Mode::Song, &search::song_docs(conn, uid, page.folder, "")?),
        };
        return Ok(ranked
            .into_iter()
            .skip(page.offset as usize)
            .take(page.count as usize)
            .collect());
    }
    let args: Vec<Sql> = vec![sv(uid)];
    let (song_folder, album_folder, artist_folder) = match page.folder {
        Some(f) => (
            format!(" AND library_id = {f}"),
            format!(" AND library_id = {f}"),
            format!(
                " AND EXISTS (SELECT 1 FROM album_artists fa JOIN albums fl ON fl.id = fa.album_id
                    WHERE fa.artist_id = ar.id AND fl.library_id = {f})"
            ),
        ),
        None => Default::default(),
    };
    let sql = match kind {
        "artist" => format!(
            "SELECT ar.id FROM artists ar WHERE {}{artist_folder} ORDER BY ar.name COLLATE NOCASE LIMIT {} OFFSET {}",
            artist_visible!("ar.id"),
            page.count,
            page.offset
        ),
        "album" => format!(
            "SELECT id FROM albums WHERE {}{album_folder} ORDER BY title COLLATE NOCASE LIMIT {} OFFSET {}",
            visible!("library_id"),
            page.count,
            page.offset
        ),
        _ => format!(
            "SELECT id FROM songs WHERE {}{song_folder} ORDER BY title COLLATE NOCASE LIMIT {} OFFSET {}",
            visible!("library_id"),
            page.count,
            page.offset
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    let ids = stmt
        .query_map(params_from_iter(args), |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

struct Results {
    artists: Vec<ArtistRow>,
    albums: Vec<AlbumRow>,
    songs: Vec<SongRow>,
}

fn page_of(ctx: &Ctx, count_key: &str, offset_key: &str) -> Result<Page, ApiError> {
    Ok(Page {
        count: ctx.params.int_or(count_key, 20)?.clamp(0, 10_000),
        offset: ctx.params.int_or(offset_key, 0)?.max(0),
        folder: super::browse::folder_filter(ctx)?,
    })
}

async fn run_search(ctx: &Ctx) -> Result<Results, ApiError> {
    let query = Query::parse(ctx.params.get("query").unwrap_or(""));
    let artist_page = page_of(ctx, "artistCount", "artistOffset")?;
    let album_page = page_of(ctx, "albumCount", "albumOffset")?;
    let song_page = page_of(ctx, "songCount", "songOffset")?;
    let uid = ctx.user().id.clone();
    ctx.st
        .db
        .run(move |c| {
            let artist_ids = ids_for(c, &uid, "artist", &query, &artist_page)?;
            let album_ids = ids_for(c, &uid, "album", &query, &album_page)?;
            let song_ids = ids_for(c, &uid, "song", &query, &song_page)?;
            Ok(Results {
                artists: artists_by_ids(c, &uid, &artist_ids)?,
                albums: albums_by_ids(c, &uid, &album_ids)?,
                songs: songs_by_ids(c, &uid, &song_ids)?,
            })
        })
        .await
}

async fn render(ctx: Ctx, v3: bool) -> ApiResult {
    let res = run_search(&ctx).await?;
    let body = ctx
        .st
        .db
        .run(move |c| {
            let artists: Vec<Value> = res
                .artists
                .iter()
                .map(|a| {
                    if v3 {
                        artist_json(a)
                    } else {
                        json!({"id": a.id, "name": a.name})
                    }
                })
                .collect();
            let albums: Vec<Value> = if v3 {
                albums_json(c, &res.albums)?
            } else {
                let ex = load_album_extras(c, &res.albums)?;
                res.albums
                    .iter()
                    .map(|a| album_child_json(a, &ex))
                    .collect()
            };
            let songs = songs_json(c, &res.songs)?;
            Ok(json!({"artist": artists, "album": albums, "song": songs}))
        })
        .await?;
    let key = if v3 { "searchResult3" } else { "searchResult2" };
    ok(json!({ key: body }))
}

pub async fn search3(ctx: Ctx) -> ApiResult {
    render(ctx, true).await
}

pub async fn search2(ctx: Ctx) -> ApiResult {
    render(ctx, false).await
}

pub async fn legacy_search(ctx: Ctx) -> ApiResult {
    let query = ctx
        .params
        .get("any")
        .or_else(|| ctx.params.get("title"))
        .or_else(|| ctx.params.get("artist"))
        .or_else(|| ctx.params.get("album"))
        .unwrap_or("")
        .to_string();
    let count = ctx.params.int_or("count", 20)?.clamp(0, 500);
    let offset = ctx.params.int_or("offset", 0)?.max(0);
    let uid = ctx.user().id.clone();
    let fts = Query::parse(&query);
    let folder = super::browse::folder_filter(&ctx)?;
    let (total, songs) = ctx
        .st
        .db
        .run(move |c| {
            let page = Page {
                count,
                offset,
                folder,
            };
            let ids = ids_for(c, &uid, "song", &fts, &page)?;
            let rows = songs_by_ids(c, &uid, &ids)?;
            Ok((offset + rows.len() as i64, songs_json(c, &rows)?))
        })
        .await?;
    ok(json!({"searchResult": {"offset": offset, "totalHits": total, "match": songs}}))
}
