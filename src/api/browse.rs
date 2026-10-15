use rusqlite::Connection;
use serde_json::{Value, json};

use super::model::*;
use super::{ApiResult, Ctx, ok};
use crate::error::ApiError;
use crate::util::strip_article;

fn articles() -> String {
    IGNORED_ARTICLES.join(" ")
}

pub async fn get_music_folders(ctx: Ctx) -> ApiResult {
    let folders = ctx
        .st
        .db
        .run(|c| {
            let mut stmt = c.prepare("SELECT id, name FROM music_folders ORDER BY id")?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(json!({"id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?;
    ok(json!({"musicFolders": {"musicFolder": folders}}))
}

fn indexed_artists(
    conn: &Connection,
    uid: &str,
    folder: Option<i64>,
) -> Result<Vec<Value>, ApiError> {
    let (tail, args) = match folder {
        Some(f) => (
            "WHERE EXISTS (SELECT 1 FROM album_artists aa JOIN albums al ON al.id = aa.album_id
                 WHERE aa.artist_id = ar.id AND al.folder_id = ?)",
            vec![iv(f)],
        ),
        None => (
            "WHERE EXISTS (SELECT 1 FROM album_artists aa WHERE aa.artist_id = ar.id)",
            vec![],
        ),
    };
    let mut rows = artists_where(conn, uid, tail, args)?;
    let arts: Vec<&str> = IGNORED_ARTICLES.to_vec();
    rows.sort_by_cached_key(|a| {
        strip_article(a.sort_name.as_deref().unwrap_or(&a.name), &arts).to_lowercase()
    });

    let mut index: Vec<(String, Vec<Value>)> = Vec::new();
    for a in &rows {
        let sort = strip_article(a.sort_name.as_deref().unwrap_or(&a.name), &arts);
        let letter = sort
            .chars()
            .next()
            .filter(|c| c.is_alphabetic())
            .map(|c| c.to_uppercase().collect::<String>())
            .unwrap_or_else(|| "#".to_string());
        match index.last_mut() {
            Some((l, list)) if *l == letter => list.push(artist_json(a)),
            _ => index.push((letter, vec![artist_json(a)])),
        }
    }
    Ok(index
        .into_iter()
        .map(|(name, artist)| json!({"name": name, "artist": artist}))
        .collect())
}

pub async fn get_artists(ctx: Ctx) -> ApiResult {
    let uid = ctx.user().id.clone();
    let folder = ctx.params.int("musicFolderId")?;
    let index = ctx
        .st
        .db
        .run(move |c| indexed_artists(c, &uid, folder))
        .await?;
    ok(json!({"artists": {"ignoredArticles": articles(), "index": index}}))
}

pub async fn get_indexes(ctx: Ctx) -> ApiResult {
    let uid = ctx.user().id.clone();
    let folder = ctx.params.int("musicFolderId")?;
    let (index, modified) = ctx
        .st
        .db
        .run(move |c| {
            let modified: Option<i64> = c.query_row(
                "SELECT last_scan_at FROM scan_state WHERE id = 1",
                [],
                |r| r.get(0),
            )?;
            Ok((
                indexed_artists(c, &uid, folder)?,
                modified.unwrap_or(0) * 1000,
            ))
        })
        .await?;
    ok(
        json!({"indexes": {"lastModified": modified, "ignoredArticles": articles(), "index": index}}),
    )
}

fn artist_albums(conn: &Connection, uid: &str, artist_id: &str) -> Result<Vec<AlbumRow>, ApiError> {
    let direct = albums_where(
        conn,
        uid,
        "WHERE al.id IN (SELECT album_id FROM album_artists WHERE artist_id = ?)
         ORDER BY al.year, al.title COLLATE NOCASE",
        vec![sv(artist_id)],
    )?;
    if !direct.is_empty() {
        return Ok(direct);
    }
    albums_where(
        conn,
        uid,
        "WHERE al.id IN (SELECT s.album_id FROM song_artists sa JOIN songs s ON s.id = sa.song_id
             WHERE sa.artist_id = ?)
         ORDER BY al.year, al.title COLLATE NOCASE",
        vec![sv(artist_id)],
    )
}

pub async fn get_artist(ctx: Ctx) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let uid = ctx.user().id.clone();
    let body = ctx
        .st
        .db
        .run(move |c| {
            let artist = artists_by_ids(c, &uid, std::slice::from_ref(&id))?
                .pop()
                .ok_or_else(|| ApiError::not_found("artist"))?;
            let albums = artist_albums(c, &uid, &id)?;
            let mut obj = artist_json(&artist);
            obj["albumCount"] = json!(albums.len());
            obj["album"] = json!(albums_json(c, &albums)?);
            Ok(obj)
        })
        .await?;
    ok(json!({"artist": body}))
}

pub async fn get_album(ctx: Ctx) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let uid = ctx.user().id.clone();
    let body = ctx
        .st
        .db
        .run(move |c| {
            let album = albums_by_ids(c, &uid, std::slice::from_ref(&id))?
                .pop()
                .ok_or_else(|| ApiError::not_found("album"))?;
            let songs = songs_where(
                c,
                &uid,
                "WHERE s.album_id = ? ORDER BY COALESCE(s.disc, 1), COALESCE(s.track, 0), s.title COLLATE NOCASE",
                vec![sv(&id)],
            )?;
            let mut obj = albums_json(c, std::slice::from_ref(&album))?.remove(0);
            obj["song"] = json!(songs_json(c, &songs)?);
            let mut stmt = c.prepare(
                "SELECT DISTINCT disc, disc_subtitle FROM songs
                 WHERE album_id = ?1 AND disc_subtitle IS NOT NULL ORDER BY disc",
            )?;
            let titles: Vec<Value> = stmt
                .query_map([&id], |r| {
                    Ok(json!({"disc": r.get::<_, Option<i64>>(0)?.unwrap_or(1), "title": r.get::<_, String>(1)?}))
                })?
                .collect::<rusqlite::Result<_>>()?;
            if !titles.is_empty() {
                obj["discTitles"] = json!(titles);
            }
            Ok(obj)
        })
        .await?;
    ok(json!({"album": body}))
}

pub async fn get_song(ctx: Ctx) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let uid = ctx.user().id.clone();
    let body = ctx
        .st
        .db
        .run(move |c| {
            let rows = songs_by_ids(c, &uid, std::slice::from_ref(&id))?;
            if rows.is_empty() {
                return Err(ApiError::not_found("song"));
            }
            Ok(songs_json(c, &rows)?.remove(0))
        })
        .await?;
    ok(json!({"song": body}))
}

pub async fn get_music_directory(ctx: Ctx) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let uid = ctx.user().id.clone();
    let body = ctx
        .st
        .db
        .run(move |c| {
            match kind_of(c, &id)? {
                Some(ItemKind::Artist) => {
                    let artist = artists_by_ids(c, &uid, std::slice::from_ref(&id))?
                        .pop()
                        .ok_or_else(|| ApiError::not_found("directory"))?;
                    let albums = artist_albums(c, &uid, &id)?;
                    let ex = load_album_extras(c, &albums)?;
                    let children: Vec<Value> =
                        albums.iter().map(|a| album_child_json(a, &ex)).collect();
                    Ok(json!({"id": id, "name": artist.name, "child": children,
                        "starred": artist.starred_at.map(crate::util::iso)}))
                }
                Some(ItemKind::Album) => {
                    let album = albums_by_ids(c, &uid, std::slice::from_ref(&id))?
                        .pop()
                        .ok_or_else(|| ApiError::not_found("directory"))?;
                    let ex = load_album_extras(c, std::slice::from_ref(&album))?;
                    let parent = ex
                        .artists
                        .get(&id)
                        .and_then(|a| a.first())
                        .map(|a| a.0.clone());
                    let songs = songs_where(
                        c,
                        &uid,
                        "WHERE s.album_id = ? ORDER BY COALESCE(s.disc, 1), COALESCE(s.track, 0), s.title COLLATE NOCASE",
                        vec![sv(&id)],
                    )?;
                    Ok(json!({"id": id, "parent": parent, "name": album.title,
                        "playCount": album.play_count,
                        "starred": album.starred_at.map(crate::util::iso),
                        "child": songs_json(c, &songs)?}))
                }
                _ => Err(ApiError::not_found("directory")),
            }
        })
        .await?;
    ok(json!({"directory": strip_nulls(body)}))
}

fn strip_nulls(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_nulls(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(strip_nulls).collect()),
        other => other,
    }
}

pub async fn get_genres(ctx: Ctx) -> ApiResult {
    let list = ctx
        .st
        .db
        .run(|c| {
            let mut stmt = c.prepare(
                "SELECT g.name, COUNT(DISTINCT sg.song_id), COUNT(DISTINCT s.album_id)
                 FROM genres g JOIN song_genres sg ON sg.genre_id = g.id JOIN songs s ON s.id = sg.song_id
                 GROUP BY g.id ORDER BY g.name COLLATE NOCASE",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(json!({"value": r.get::<_, String>(0)?, "songCount": r.get::<_, i64>(1)?,
                        "albumCount": r.get::<_, i64>(2)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?;
    ok(json!({"genres": {"genre": list}}))
}

async fn artist_info(ctx: Ctx, key: &str) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let uid = ctx.user().id.clone();
    let artist = ctx
        .st
        .db
        .run(move |c| {
            artists_by_ids(c, &uid, std::slice::from_ref(&id))?
                .pop()
                .ok_or_else(|| ApiError::not_found("artist"))
        })
        .await?;
    let mut info = json!({"biography": "", "lastFmUrl": "", "similarArtist": []});
    if let Some(m) = &artist.mbid {
        info["musicBrainzId"] = json!(m);
    }
    if let Some(o) = &artist.omm_id {
        info["ommId"] = json!(o);
    }
    ok(json!({ key: info }))
}

pub async fn get_artist_info(ctx: Ctx) -> ApiResult {
    artist_info(ctx, "artistInfo").await
}

pub async fn get_artist_info2(ctx: Ctx) -> ApiResult {
    artist_info(ctx, "artistInfo2").await
}

pub async fn get_album_info(ctx: Ctx) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let uid = ctx.user().id.clone();
    let album = ctx
        .st
        .db
        .run(move |c| {
            if let Some(a) = albums_by_ids(c, &uid, std::slice::from_ref(&id))?.pop() {
                return Ok(a);
            }
            let songs = songs_by_ids(c, &uid, std::slice::from_ref(&id))?;
            let song = songs.first().ok_or_else(|| ApiError::not_found("album"))?;
            albums_by_ids(c, &uid, std::slice::from_ref(&song.album_id))?
                .pop()
                .ok_or_else(|| ApiError::not_found("album"))
        })
        .await?;
    let mut info = json!({"notes": ""});
    if let Some(m) = &album.mbid {
        info["musicBrainzId"] = json!(m);
    }
    if let Some(o) = &album.omm_id {
        info["ommId"] = json!(o);
    }
    ok(json!({"albumInfo": info}))
}

async fn similar(ctx: Ctx, key: &str) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let count = ctx.params.int_or("count", 50)?.clamp(1, 500);
    let uid = ctx.user().id.clone();
    let songs = ctx
        .st
        .db
        .run(move |c| {
            let source = match kind_of(c, &id)? {
                Some(ItemKind::Song) => "s.id = ?1",
                Some(ItemKind::Album) => "s.album_id = ?1",
                Some(ItemKind::Artist) => "s.id IN (SELECT song_id FROM song_artists WHERE artist_id = ?1)",
                _ => return Err(ApiError::not_found("item")),
            };
            let by_genre = format!(
                "WHERE s.id NOT IN (SELECT s.id FROM songs s WHERE {source}) AND s.id IN (
                    SELECT sg2.song_id FROM song_genres sg2 WHERE sg2.genre_id IN (
                        SELECT sg.genre_id FROM song_genres sg JOIN songs s ON s.id = sg.song_id WHERE {source}))
                 ORDER BY RANDOM() LIMIT {count}"
            );
            let mut rows = songs_where(c, &uid, &by_genre.replace("?1", "?2"), vec![sv(&id)])?;
            if rows.is_empty() {
                let by_artist = format!(
                    "WHERE s.id NOT IN (SELECT s.id FROM songs s WHERE {source}) AND s.id IN (
                        SELECT sa2.song_id FROM song_artists sa2 WHERE sa2.artist_id IN (
                            SELECT sa.artist_id FROM song_artists sa JOIN songs s ON s.id = sa.song_id WHERE {source}))
                     ORDER BY RANDOM() LIMIT {count}"
                );
                rows = songs_where(c, &uid, &by_artist.replace("?1", "?2"), vec![sv(&id)])?;
            }
            songs_json(c, &rows)
        })
        .await?;
    ok(json!({ key: {"song": songs} }))
}

pub async fn get_similar_songs(ctx: Ctx) -> ApiResult {
    similar(ctx, "similarSongs").await
}

pub async fn get_similar_songs2(ctx: Ctx) -> ApiResult {
    similar(ctx, "similarSongs2").await
}

pub async fn get_top_songs(ctx: Ctx) -> ApiResult {
    let artist = ctx.params.req("artist")?.to_string();
    let count = ctx.params.int_or("count", 50)?.clamp(1, 500);
    let uid = ctx.user().id.clone();
    let songs = ctx
        .st
        .db
        .run(move |c| {
            let tail = format!(
                "WHERE s.id IN (SELECT sa.song_id FROM song_artists sa JOIN artists a ON a.id = sa.artist_id
                     WHERE a.name = ? COLLATE NOCASE)
                 ORDER BY (SELECT COALESCE(SUM(p.play_count), 0) FROM play_stats p WHERE p.song_id = s.id) DESC,
                          s.title COLLATE NOCASE LIMIT {count}"
            );
            let rows = songs_where(c, &uid, &tail, vec![sv(&artist)])?;
            songs_json(c, &rows)
        })
        .await?;
    ok(json!({"topSongs": {"song": songs}}))
}

pub async fn get_videos(_: Ctx) -> ApiResult {
    ok(json!({"videos": {}}))
}

pub async fn get_video_info(_: Ctx) -> ApiResult {
    Err(ApiError::not_found("video"))
}
