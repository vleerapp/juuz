use rusqlite::types::Value as Sql;
use serde_json::{Value, json};

use super::model::*;
use super::{ApiResult, Ctx, ok};
use crate::db::now;
use crate::error::ApiError;
use crate::util::iso;

const PLAYS: &str =
    "(SELECT COALESCE(SUM(p.play_count), 0) FROM play_stats p JOIN songs s ON s.id = p.song_id
    WHERE s.album_id = al.id AND p.user_id = ?1)";
const LAST_PLAYED: &str =
    "(SELECT MAX(p.last_played) FROM play_stats p JOIN songs s ON s.id = p.song_id
    WHERE s.album_id = al.id AND p.user_id = ?1)";

fn album_list_tail(ctx: &Ctx) -> Result<(String, Vec<Sql>), ApiError> {
    let p = &ctx.params;
    let kind = p.req("type")?;
    let size = p.int_or("size", 10)?.clamp(0, 500);
    let offset = p.int_or("offset", 0)?.max(0);
    let mut wheres: Vec<String> = Vec::new();
    let mut args: Vec<Sql> = Vec::new();

    let order = match kind {
        "random" => "RANDOM()".to_string(),
        "newest" => "al.created_at DESC".to_string(),
        "highest" => {
            wheres.push("rt.rating IS NOT NULL".into());
            "rt.rating DESC, al.title COLLATE NOCASE".to_string()
        }
        "frequent" => {
            wheres.push(format!("{PLAYS} > 0"));
            format!("{PLAYS} DESC")
        }
        "recent" => {
            wheres.push(format!("{LAST_PLAYED} IS NOT NULL"));
            format!("{LAST_PLAYED} DESC")
        }
        "alphabeticalByName" => "al.title COLLATE NOCASE".to_string(),
        "alphabeticalByArtist" => "(SELECT a.name FROM album_artists aa JOIN artists a ON a.id = aa.artist_id
            WHERE aa.album_id = al.id ORDER BY aa.position LIMIT 1) COLLATE NOCASE, al.title COLLATE NOCASE"
            .to_string(),
        "starred" => {
            wheres.push("st.starred_at IS NOT NULL".into());
            "st.starred_at DESC".to_string()
        }
        "byYear" => {
            let from = p.int("fromYear")?.ok_or_else(|| ApiError::missing("fromYear"))?;
            let to = p.int("toYear")?.ok_or_else(|| ApiError::missing("toYear"))?;
            wheres.push("al.year BETWEEN ? AND ?".into());
            args.push(iv(from.min(to)));
            args.push(iv(from.max(to)));
            if from > to {
                "al.year DESC, al.title COLLATE NOCASE".to_string()
            } else {
                "al.year, al.title COLLATE NOCASE".to_string()
            }
        }
        "byGenre" => {
            let genre = p.req("genre")?;
            wheres.push(
                "EXISTS (SELECT 1 FROM songs s JOIN song_genres sg ON sg.song_id = s.id
                    JOIN genres g ON g.id = sg.genre_id WHERE s.album_id = al.id AND g.name = ? COLLATE NOCASE)"
                    .into(),
            );
            args.push(sv(genre));
            "al.title COLLATE NOCASE".to_string()
        }
        _ => return Err(ApiError::bad_param("type")),
    };

    let where_sql = if wheres.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", wheres.join(" AND "))
    };
    Ok((
        format!("{where_sql} ORDER BY {order} LIMIT {size} OFFSET {offset}"),
        args,
    ))
}

pub async fn get_album_list(ctx: Ctx) -> ApiResult {
    let (tail, args) = album_list_tail(&ctx)?;
    let uid = ctx.user().id.clone();
    let list = ctx
        .st
        .db
        .run(move |c| {
            let rows = albums_where(c, &uid, &tail, args)?;
            let ex = load_album_extras(c, &rows)?;
            Ok(rows
                .iter()
                .map(|a| album_child_json(a, &ex))
                .collect::<Vec<_>>())
        })
        .await?;
    ok(json!({"albumList": {"album": list}}))
}

pub async fn get_album_list2(ctx: Ctx) -> ApiResult {
    let (tail, args) = album_list_tail(&ctx)?;
    let uid = ctx.user().id.clone();
    let list = ctx
        .st
        .db
        .run(move |c| {
            let rows = albums_where(c, &uid, &tail, args)?;
            albums_json(c, &rows)
        })
        .await?;
    ok(json!({"albumList2": {"album": list}}))
}

pub async fn get_random_songs(ctx: Ctx) -> ApiResult {
    let p = &ctx.params;
    let size = p.int_or("size", 10)?.clamp(0, 500);
    let mut wheres: Vec<String> = Vec::new();
    let mut args: Vec<Sql> = Vec::new();
    if let Some(g) = p.get("genre").filter(|g| !g.is_empty()) {
        wheres.push(
            "s.id IN (SELECT sg.song_id FROM song_genres sg JOIN genres g ON g.id = sg.genre_id
                WHERE g.name = ? COLLATE NOCASE)"
                .into(),
        );
        args.push(sv(g));
    }
    if let Some(y) = p.int("fromYear")? {
        wheres.push("s.year >= ?".into());
        args.push(iv(y));
    }
    if let Some(y) = p.int("toYear")? {
        wheres.push("s.year <= ?".into());
        args.push(iv(y));
    }
    let where_sql = if wheres.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", wheres.join(" AND "))
    };
    let tail = format!("{where_sql} ORDER BY RANDOM() LIMIT {size}");
    let uid = ctx.user().id.clone();
    let songs = ctx
        .st
        .db
        .run(move |c| {
            let rows = songs_where(c, &uid, &tail, args)?;
            songs_json(c, &rows)
        })
        .await?;
    ok(json!({"randomSongs": {"song": songs}}))
}

pub async fn get_songs_by_genre(ctx: Ctx) -> ApiResult {
    let p = &ctx.params;
    let genre = p.req("genre")?.to_string();
    let count = p.int_or("count", 10)?.clamp(0, 500);
    let offset = p.int_or("offset", 0)?.max(0);
    let args = vec![sv(&genre)];
    let tail = format!(
        "WHERE s.id IN (SELECT sg.song_id FROM song_genres sg JOIN genres g ON g.id = sg.genre_id
            WHERE g.name = ? COLLATE NOCASE)
         ORDER BY s.title COLLATE NOCASE LIMIT {count} OFFSET {offset}"
    );
    let uid = ctx.user().id.clone();
    let songs = ctx
        .st
        .db
        .run(move |c| {
            let rows = songs_where(c, &uid, &tail, args)?;
            songs_json(c, &rows)
        })
        .await?;
    ok(json!({"songsByGenre": {"song": songs}}))
}

pub async fn get_now_playing(ctx: Ctx) -> ApiResult {
    let entries = ctx.st.playing_now();
    let uid = ctx.user().id.clone();
    let list = ctx
        .st
        .db
        .run(move |c| {
            let mut ids: Vec<String> = entries.iter().map(|e| e.song_id.clone()).collect();
            ids.sort();
            ids.dedup();
            let rows = songs_by_ids(c, &uid, &ids)?;
            let json_rows = songs_json(c, &rows)?;
            let by_id: std::collections::HashMap<String, serde_json::Value> = rows
                .iter()
                .zip(json_rows)
                .map(|(row, child)| (row.id.clone(), child))
                .collect();
            Ok(entries
                .iter()
                .filter_map(|e| {
                    let mut child = by_id.get(&e.song_id)?.clone();
                    child["username"] = json!(e.username);
                    child["minutesAgo"] = json!((now() - e.started) / 60);
                    child["playerName"] = json!(e.player);
                    child["playerId"] = json!(e.player);
                    Some(child)
                })
                .collect::<Vec<_>>())
        })
        .await?;
    ok(json!({"nowPlaying": {"entry": list}}))
}

async fn starred(ctx: Ctx, v2: bool) -> ApiResult {
    let uid = ctx.user().id.clone();
    let (artists, albums, songs) = ctx
        .st
        .db
        .run(move |c| {
            let artists = artists_where(
                c,
                &uid,
                "WHERE st.starred_at IS NOT NULL ORDER BY st.starred_at DESC",
                vec![],
            )?;
            let albums = albums_where(
                c,
                &uid,
                "WHERE st.starred_at IS NOT NULL ORDER BY st.starred_at DESC",
                vec![],
            )?;
            let songs = songs_where(
                c,
                &uid,
                "WHERE st.starred_at IS NOT NULL ORDER BY st.starred_at DESC",
                vec![],
            )?;
            let album_json: Vec<Value> = if v2 {
                albums_json(c, &albums)?
            } else {
                let ex = load_album_extras(c, &albums)?;
                albums.iter().map(|a| album_child_json(a, &ex)).collect()
            };
            let artist_json_list: Vec<Value> = artists
                .iter()
                .map(|a| {
                    let mut j = artist_json(a);
                    if !v2 {
                        j["starred"] = json!(a.starred_at.map(iso));
                    }
                    j
                })
                .collect();
            Ok((artist_json_list, album_json, songs_json(c, &songs)?))
        })
        .await?;
    let key = if v2 { "starred2" } else { "starred" };
    ok(json!({ key: {"artist": artists, "album": albums, "song": songs} }))
}

pub async fn get_starred(ctx: Ctx) -> ApiResult {
    starred(ctx, false).await
}

pub async fn get_starred2(ctx: Ctx) -> ApiResult {
    starred(ctx, true).await
}
