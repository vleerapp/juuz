use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Request, header};
use axum::response::Response;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use super::model::{ItemKind, kind_of};
use super::{ApiResult, Ctx, Reply, ok};
use crate::error::ApiError;
use crate::lyrics;
use crate::scanner::tags::read_cover;
use crate::state::AppState;
use crate::transcode::Format;

struct SongFile {
    id: String,
    mtime: i64,
    bitrate: i64,
    duration_ms: i64,
    from_library: bool,
    path: PathBuf,
    content_type: String,
    title: String,
    suffix: String,
}

fn song_file(conn: &Connection, id: &str) -> Result<SongFile, ApiError> {
    let song = conn
        .query_row(
            "SELECT path, content_type, title, suffix, mtime, bitrate, duration_ms FROM songs WHERE id = ?1",
            [id],
            |r| {
                Ok(SongFile {
                    id: id.to_string(),
                    mtime: r.get(4)?,
                    bitrate: r.get(5)?,
                    duration_ms: r.get(6)?,
                    from_library: true,
                    path: PathBuf::from(r.get::<_, String>(0)?),
                    content_type: r.get(1)?,
                    title: r.get(2)?,
                    suffix: r.get(3)?,
                })
            },
        )
        .optional()?;
    if let Some(song) = song {
        return Ok(song);
    }
    conn.query_row(
        "SELECT path, COALESCE(content_type, 'audio/mpeg'), title FROM podcast_episodes
         WHERE id = ?1 AND status = 'completed' AND path IS NOT NULL",
        [id],
        |r| {
            let path = PathBuf::from(r.get::<_, String>(0)?);
            let suffix = path
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(SongFile {
                id: id.to_string(),
                mtime: 0,
                bitrate: 0,
                duration_ms: 0,
                from_library: false,
                path,
                content_type: r.get(1)?,
                title: r.get(2)?,
                suffix,
            })
        },
    )
    .optional()?
    .ok_or_else(|| ApiError::not_found("song"))
}

pub async fn serve_file(
    headers: &HeaderMap,
    path: &Path,
    content_type: &str,
    attachment: Option<String>,
) -> Result<Response, ApiError> {
    let mut builder = Request::builder().uri("/");
    for name in [
        header::RANGE,
        header::IF_RANGE,
        header::IF_NONE_MATCH,
        header::IF_MODIFIED_SINCE,
    ] {
        if let Some(v) = headers.get(&name) {
            builder = builder.header(name, v);
        }
    }
    let req = builder
        .body(Body::empty())
        .map_err(|_| ApiError::generic("bad request"))?;
    let resp = ServeFile::new(path)
        .oneshot(req)
        .await
        .map_err(|_| ApiError::generic("could not read file"))?;
    let mut resp = resp.map(Body::new);
    if resp.status() == axum::http::StatusCode::NOT_FOUND {
        return Err(ApiError::not_found("file"));
    }
    if let Ok(v) = HeaderValue::from_str(content_type) {
        resp.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    if let Some(name) = attachment {
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_control() || matches!(c, '"' | '\\' | '/') {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{safe}\"")) {
            resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
    }
    Ok(resp)
}

pub async fn serve_song(
    st: &AppState,
    id: &str,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let id = id.to_string();
    let song = st.db.run(move |c| song_file(c, &id)).await?;
    serve_file(headers, &song.path, &song.content_type, None).await
}

async fn song_for(ctx: &Ctx) -> Result<SongFile, ApiError> {
    let id = ctx.params.req("id")?.to_string();
    ctx.st.db.run(move |c| song_file(c, &id)).await
}

fn note_streaming(ctx: &Ctx, song: &SongFile) {
    let user = ctx.user();
    let player = ctx.params.get("c").unwrap_or("unknown").to_string();
    let mut map = ctx.st.now_playing.lock().unwrap_or_else(|e| e.into_inner());
    let key = (user.id.clone(), player.clone());
    if map.get(&key).is_some_and(|n| n.song_id == song.id) {
        return;
    }
    let started = crate::db::now();
    map.insert(
        key,
        crate::state::NowPlaying {
            user_id: user.id.clone(),
            song_id: song.id.clone(),
            username: user.name.clone(),
            player,
            started,
            until: started + (song.duration_ms / 1000).max(30) + 15,
        },
    );
    drop(map);
    ctx.st.live.notify();
}

pub async fn stream(ctx: Ctx) -> ApiResult {
    let user = ctx.user();
    if !(user.stream || user.admin) {
        return Err(ApiError::forbidden("user is not authorized for streaming"));
    }
    let song = song_for(&ctx).await?;
    note_streaming(&ctx, &song);
    let wanted = ctx
        .params
        .get("format")
        .and_then(Format::parse)
        .filter(|_| song.from_library);
    if let Some(format) = wanted {
        let default = match format {
            Format::Opus | Format::Aac => 128,
            Format::Mp3 => 192,
        };
        let requested = ctx
            .params
            .int("maxBitRate")?
            .filter(|b| *b > 0)
            .unwrap_or(default)
            .clamp(32, 320);
        let same_format = song.suffix.eq_ignore_ascii_case(format.extension());
        let lossy_small_enough = song.bitrate > 0 && song.bitrate <= requested;
        let too_long =
            format != Format::Opus && song.duration_ms / 1000 > crate::transcode::MAX_BUFFERED_SECS;
        if (!same_format || !lossy_small_enough) && !too_long {
            let kbps = if song.bitrate > 0 {
                requested.min(song.bitrate.max(32))
            } else {
                requested
            };
            match transcoded_file(&ctx, &song, format, kbps).await {
                Ok(path) => {
                    return Ok(Reply::Raw(
                        serve_file(&ctx.headers, &path, format.content_type(), None).await?,
                    ));
                }
                Err(e) => tracing::warn!(
                    "{} transcode of {} failed, serving original: {e:#}",
                    format.extension(),
                    song.id
                ),
            }
        }
    }
    Ok(Reply::Raw(
        serve_file(&ctx.headers, &song.path, &song.content_type, None).await?,
    ))
}

const TRANSCODE_CACHE_LIMIT: u64 = 4 * 1024 * 1024 * 1024;

fn evict_cache(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_file().then(|| {
                (
                    meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                    meta.len(),
                    e.path(),
                )
            })
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= TRANSCODE_CACHE_LIMIT {
        return;
    }
    files.sort();
    for (_, size, path) in files {
        if total <= TRANSCODE_CACHE_LIMIT * 3 / 4 {
            break;
        }
        if path != keep && std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

async fn transcoded_file(
    ctx: &Ctx,
    song: &SongFile,
    format: Format,
    kbps: i64,
) -> anyhow::Result<PathBuf> {
    let dir = ctx.st.cfg.transcode_dir();
    let target = dir.join(format!(
        "{}-{kbps}-{}.{}",
        song.id,
        song.mtime,
        format.extension()
    ));
    if target.exists() {
        return Ok(target);
    }
    let _permit = ctx.st.transcode_slots.acquire().await?;
    if target.exists() {
        return Ok(target);
    }
    tokio::fs::create_dir_all(&dir).await?;
    let (src, dest, tmp) = (
        song.path.clone(),
        target.clone(),
        dir.join(format!(".{}.{}.part", song.id, crate::ids::random())),
    );
    tokio::task::spawn_blocking(move || {
        let result = crate::transcode::transcode(&src, &tmp, format, (kbps * 1000) as i32)
            .and_then(|()| std::fs::rename(&tmp, &dest).map_err(Into::into));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        } else if let Some(parent) = dest.parent() {
            evict_cache(parent, &dest);
        }
        result
    })
    .await??;
    Ok(target)
}

pub async fn download(ctx: Ctx) -> ApiResult {
    let user = ctx.user();
    if !(user.download || user.admin) {
        return Err(ApiError::forbidden("user is not authorized for downloads"));
    }
    let song = song_for(&ctx).await?;
    let name = format!("{}.{}", song.title, song.suffix);
    Ok(Reply::Raw(
        serve_file(&ctx.headers, &song.path, &song.content_type, Some(name)).await?,
    ))
}

enum ArtSource {
    File(PathBuf),
    Embedded(PathBuf),
}

fn album_art(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<(String, ArtSource)>> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT cover_hash, cover_path FROM albums WHERE id = ?1",
            [album_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((Some(hash), path)) = row else {
        return Ok(None);
    };
    if let Some(p) = path {
        return Ok(Some((hash, ArtSource::File(PathBuf::from(p)))));
    }
    let song: Option<String> = conn
        .query_row(
            "SELECT path FROM songs WHERE album_id = ?1 AND cover_hash = ?2 LIMIT 1",
            [album_id, &hash],
            |r| r.get(0),
        )
        .optional()?;
    Ok(song.map(|p| (hash, ArtSource::Embedded(PathBuf::from(p)))))
}

fn resolve_art(conn: &Connection, id: &str) -> rusqlite::Result<Option<(String, ArtSource)>> {
    match kind_of(conn, id)? {
        Some(ItemKind::Song) => {
            let row: Option<(String, Option<String>, String)> = conn
                .query_row(
                    "SELECT album_id, cover_hash, path FROM songs WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let Some((album_id, hash, path)) = row else {
                return Ok(None);
            };
            match hash {
                Some(h) => Ok(Some((h, ArtSource::Embedded(PathBuf::from(path))))),
                None => album_art(conn, &album_id),
            }
        }
        Some(ItemKind::Album) => album_art(conn, id),
        Some(ItemKind::Artist) => {
            let image: Option<(String, String)> = conn
                .query_row(
                    "SELECT image_hash, image_path FROM artists WHERE id = ?1 AND image_path IS NOT NULL",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((hash, path)) = image {
                return Ok(Some((hash, ArtSource::File(PathBuf::from(path)))));
            }
            let album: Option<String> = conn
                .query_row(
                    "SELECT al.id FROM album_artists aa JOIN albums al ON al.id = aa.album_id
                     WHERE aa.artist_id = ?1 AND al.cover_hash IS NOT NULL ORDER BY al.year DESC LIMIT 1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            match album {
                Some(a) => album_art(conn, &a),
                None => Ok(None),
            }
        }
        Some(ItemKind::Playlist) => {
            let song: Option<String> = conn
                .query_row(
                    "SELECT song_id FROM playlist_songs WHERE playlist_id = ?1 ORDER BY position LIMIT 1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            match song {
                Some(s) => resolve_art(conn, &s),
                None => Ok(None),
            }
        }
        None => Ok(None),
    }
}

fn load_art(source: &ArtSource) -> Option<Vec<u8>> {
    match source {
        ArtSource::File(p) => std::fs::read(p).ok(),
        ArtSource::Embedded(p) => read_cover(p),
    }
}

fn resized(bytes: &[u8], size: u32) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > size || img.height() > size {
        img.thumbnail(size, size)
    } else {
        img
    };
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
    img.to_rgb8().write_with_encoder(encoder).ok()?;
    Some(out)
}

fn image_response(bytes: Vec<u8>, content_type: &str, etag: &str) -> Response {
    let mut resp = Response::new(Body::from(bytes));
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(content_type) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("\"{etag}\"")) {
        h.insert(header::ETAG, v);
    }
    resp
}

fn mime_of(bytes: &[u8]) -> &'static str {
    match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png) => "image/png",
        Ok(image::ImageFormat::WebP) => "image/webp",
        Ok(image::ImageFormat::Gif) => "image/gif",
        _ => "image/jpeg",
    }
}

pub async fn get_cover_art(ctx: Ctx) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let size = ctx.params.int("size")?.map(|s| s.clamp(16, 2400) as u32);
    let cache_dir = ctx.st.cfg.art_cache_dir();

    let found = ctx.st.db.run(move |c| Ok(resolve_art(c, &id)?)).await?;
    let (hash, source) = found.ok_or_else(|| ApiError::not_found("cover art"))?;

    let resp = tokio::task::spawn_blocking(move || -> Result<Response, ApiError> {
        if let Some(size) = size {
            let cached = cache_dir.join(format!("{hash}_{size}.jpg"));
            if let Ok(bytes) = std::fs::read(&cached) {
                return Ok(image_response(
                    bytes,
                    "image/jpeg",
                    &format!("{hash}-{size}"),
                ));
            }
            let original = load_art(&source).ok_or_else(|| ApiError::not_found("cover art"))?;
            return match resized(&original, size) {
                Some(bytes) => {
                    if std::fs::create_dir_all(&cache_dir).is_ok() {
                        let tmp = cache_dir.join(format!(".{hash}_{size}.tmp"));
                        if std::fs::write(&tmp, &bytes).is_ok() {
                            let _ = std::fs::rename(&tmp, &cached);
                        }
                    }
                    Ok(image_response(
                        bytes,
                        "image/jpeg",
                        &format!("{hash}-{size}"),
                    ))
                }
                None => {
                    let mime = mime_of(&original);
                    Ok(image_response(original, mime, &hash))
                }
            };
        }
        let original = load_art(&source).ok_or_else(|| ApiError::not_found("cover art"))?;
        let mime = mime_of(&original);
        Ok(image_response(original, mime, &hash))
    })
    .await
    .map_err(|_| ApiError::generic("internal server error"))??;
    Ok(Reply::Raw(resp))
}

pub async fn get_avatar(_: Ctx) -> ApiResult {
    Err(ApiError::not_found("avatar"))
}

pub async fn get_lyrics_by_song_id(ctx: Ctx) -> ApiResult {
    let id = ctx.params.req("id")?.to_string();
    let found = find_lyrics(&ctx, id).await?;
    let Some(found) = found else {
        return ok(json!({"lyricsList": {"structuredLyrics": []}}));
    };
    let (synced, lines) = lyrics::parse(&found.text);
    let line_json: Vec<Value> = lines
        .iter()
        .map(|l| match l.start {
            Some(s) => json!({"start": s, "value": l.text}),
            None => json!({"value": l.text}),
        })
        .collect();
    ok(json!({"lyricsList": {"structuredLyrics": [{
        "displayArtist": found.artist,
        "displayTitle": found.title,
        "lang": "xxx",
        "synced": synced,
        "line": line_json,
    }]}}))
}

async fn find_lyrics(ctx: &Ctx, id: String) -> Result<Option<lyrics::Found>, ApiError> {
    let (db, client) = (ctx.st.db.clone(), ctx.st.lyrics.clone());
    tokio::task::spawn_blocking(move || lyrics::find(&db, &client, &id))
        .await
        .map_err(|_| ApiError::generic("internal server error"))?
}

pub async fn get_lyrics(ctx: Ctx) -> ApiResult {
    let title = ctx.params.get("title").unwrap_or("").to_string();
    let artist = ctx.params.get("artist").unwrap_or("").to_string();
    if title.is_empty() {
        return ok(json!({"lyrics": {}}));
    }
    let id = ctx
        .st
        .db
        .run(move |c| {
            Ok(c.query_row(
                "SELECT id FROM songs WHERE title = ?1 COLLATE NOCASE
                   AND (?2 = '' OR display_artist LIKE '%' || ?2 || '%' COLLATE NOCASE) LIMIT 1",
                [&title, &artist],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
        })
        .await?;
    let Some(id) = id else {
        return ok(json!({"lyrics": {}}));
    };
    let Some(found) = find_lyrics(&ctx, id).await? else {
        return ok(json!({"lyrics": {}}));
    };
    let (_, lines) = lyrics::parse(&found.text);
    let plain = lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    ok(json!({"lyrics": {"artist": found.artist, "title": found.title, "value": plain}}))
}
