use std::collections::HashSet;
use std::path::{Path, PathBuf};

use axum::http::header;
use lofty::file::FileType;
use lofty::probe::Probe;
use rusqlite::{OptionalExtension, params};
use serde_json::json;
use tokio::io::AsyncWriteExt;

use super::model::*;
use super::{ApiResult, Ctx, empty, ok};
use crate::auth;
use crate::db::libraries::{self, Library};
use crate::db::users::User;
use crate::error::ApiError;
use crate::ids;
use crate::omm::{self, SongRes};
use crate::params::Params;
use crate::scanner::tags::{AUDIO_EXTENSIONS, TrackInfo, parse_artists, read_track, split_genres};
use crate::state::AppState;
use crate::upload::{self, Meta, parse_hint, target_relative, unique_path, write_tags};

const TMP_DIR: &str = ".juuz-tmp";
const MAX_LYRICS: u64 = 1024 * 1024;
const MAX_TEXT_FIELD: usize = 8192;
const MAX_TEXT_FIELDS: usize = 64;
const MULTIPART_OVERHEAD: u64 = 1024 * 1024;
const MAX_COVER_DIMENSION: u32 = 12_000;

fn check_role(st: &AppState, user: &User) -> Result<(), ApiError> {
    if !st.cfg.upload.enabled {
        return Err(ApiError::forbidden("uploads are disabled on this server"));
    }
    if !user.can_upload() {
        return Err(ApiError::forbidden("user is not authorized to upload"));
    }
    Ok(())
}

fn bad_upload(message: impl Into<String>) -> ApiError {
    ApiError::generic(message)
}

fn writable_library(
    conn: &rusqlite::Connection,
    user: &User,
    id: i64,
) -> Result<Library, ApiError> {
    let library = libraries::get(conn, id)?.ok_or_else(|| ApiError::not_found("library"))?;
    if !user.admin && !libraries::can_access(conn, &user.id, id)? {
        return Err(ApiError::not_found("library"));
    }
    if !library.writable {
        return Err(ApiError::forbidden("this library is read only"));
    }
    if !library.online() {
        return Err(ApiError::generic("this library is offline"));
    }
    Ok(library)
}

async fn target_library(
    st: &AppState,
    user: &User,
    params: &Params,
    kind: &str,
) -> Result<Library, ApiError> {
    let user = user.clone();
    let requested = params.int("musicFolderId")?;
    let key = match kind {
        "cover" => params.get("albumId").or_else(|| params.get("songId")),
        "lyrics" => params.get("songId"),
        _ => None,
    }
    .map(str::to_string);
    let kind = kind.to_string();
    st.db
        .run(move |c| {
            if kind != "song" {
                let key = key.ok_or_else(|| ApiError::missing("albumId"))?;
                let id: Option<i64> = c
                    .query_row(
                        "SELECT library_id FROM songs WHERE id = ?1
                         UNION ALL SELECT library_id FROM albums WHERE id = ?1 LIMIT 1",
                        [&key],
                        |r| r.get(0),
                    )
                    .optional()?;
                let id = id.ok_or_else(|| ApiError::not_found("item"))?;
                return writable_library(c, &user, id);
            }
            if let Some(id) = requested {
                return writable_library(c, &user, id);
            }
            if let Some(id) = user.upload_library_id
                && let Ok(library) = writable_library(c, &user, id)
            {
                return Ok(library);
            }
            libraries::visible_to(c, &user.id)?
                .into_iter()
                .find(|l| l.writable && l.online())
                .ok_or_else(|| ApiError::forbidden("no writable library available"))
        })
        .await
}

async fn receive(
    field: &mut multer::Field<'_>,
    dir: &Path,
    max: u64,
) -> Result<(PathBuf, u64), ApiError> {
    tokio::fs::create_dir_all(dir).await?;
    let path = dir.join(format!("{}.part", ids::random()));
    let mut file = tokio::fs::File::create(&path).await?;
    let mut written: u64 = 0;
    let result: Result<(), ApiError> = async {
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| bad_upload(format!("upload interrupted: {e}")))?
        {
            written += chunk.len() as u64;
            if written > max {
                return Err(bad_upload(format!("file exceeds the {max} byte limit")));
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok(())
    }
    .await;
    drop(file);
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(e);
    }
    if written == 0 {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(bad_upload("uploaded file is empty"));
    }
    Ok((path, written))
}

fn detect_extension(path: &Path) -> Result<&'static str, ApiError> {
    let probe = Probe::open(path)
        .map_err(|_| bad_upload("not a valid audio file"))?
        .guess_file_type()
        .map_err(|_| bad_upload("not a valid audio file"))?;
    let ext = match probe.file_type() {
        Some(FileType::Flac) => "flac",
        Some(FileType::Mpeg) => "mp3",
        Some(FileType::Mp4) => "m4a",
        Some(FileType::Aac) => "aac",
        Some(FileType::Opus) => "opus",
        Some(FileType::Vorbis) => "ogg",
        Some(FileType::Wav) => "wav",
        Some(FileType::Aiff) => "aiff",
        Some(FileType::WavPack) => "wv",
        _ => return Err(bad_upload("not a valid audio file")),
    };
    debug_assert!(AUDIO_EXTENSIONS.contains(&ext));
    Ok(ext)
}

fn meta_from_track(t: &TrackInfo) -> Meta {
    Meta {
        title: if t.tagged_title {
            t.title.clone()
        } else {
            String::new()
        },
        artists: t.artists.clone(),
        album_artist: (!t.album_artists.is_empty()).then(|| t.album_artists.join(", ")),
        album: t.album.clone(),
        track: t.track,
        disc: t.disc,
        year: t.year,
        date: t.date.clone(),
        genres: t.genres.clone(),
        isrc: t.isrc.clone(),
        upc: t.upc.clone(),
    }
}

fn artist_list(params: &Params, key: &str) -> Option<Vec<String>> {
    let values = params.all(key);
    if values.is_empty() {
        return None;
    }
    if values.len() > 1 {
        return Some(parse_artists(&values, &[]));
    }
    Some(parse_artists(&[], &values))
}

fn apply_overrides(meta: &mut Meta, params: &Params) -> Result<bool, ApiError> {
    let mut explicit = false;
    if let Some(v) = params.get("title").map(str::trim).filter(|v| !v.is_empty()) {
        meta.title = v.to_string();
        explicit = true;
    }
    if let Some(list) = artist_list(params, "artist") {
        meta.artists = list;
        explicit = true;
    }
    if let Some(v) = params.get("albumArtist") {
        let v = v.trim();
        meta.album_artist = (!v.is_empty()).then(|| v.to_string());
        explicit = true;
    }
    if let Some(v) = params.get("album").map(str::trim).filter(|v| !v.is_empty()) {
        meta.album = Some(v.to_string());
        explicit = true;
    }
    if let Some(v) = params.int("track")? {
        meta.track = u32::try_from(v).ok().filter(|t| *t > 0);
        explicit = true;
    }
    if let Some(v) = params.int("disc")? {
        meta.disc = u32::try_from(v).ok().filter(|t| *t > 0);
        explicit = true;
    }
    if let Some(v) = params.get("date").map(str::trim).filter(|v| v.len() >= 4) {
        meta.year = v.get(..4).and_then(|y| y.parse().ok());
        meta.date = Some(v.to_string());
        explicit = true;
    } else if let Some(v) = params.int("year")? {
        meta.year = i32::try_from(v).ok().filter(|y| (1000..=2999).contains(y));
        meta.date = None;
        explicit = true;
    }
    let genres: Vec<String> = params
        .all("genre")
        .into_iter()
        .flat_map(split_genres)
        .collect();
    if !genres.is_empty() {
        meta.genres = genres;
        explicit = true;
    }
    Ok(explicit)
}

fn apply_fallbacks(meta: &mut Meta, params: &Params, original: Option<&str>) {
    let hint = parse_hint(params.get("relativePath").unwrap_or(""), original);
    if meta.title.is_empty() {
        meta.title = hint.title.unwrap_or_else(|| "Untitled".to_string());
    }
    if meta.artists.is_empty()
        && let Some(a) = hint.artist
    {
        meta.artists = vec![a];
    }
    if meta.album.is_none() {
        meta.album = hint.album;
    }
    if meta.year.is_none() {
        meta.year = hint.year;
    }
    if meta.track.is_none() {
        meta.track = hint.track;
    }
}

fn apply_omm(
    meta: &mut Meta,
    params: &Params,
    client: &omm::Client,
    duration_ms: i64,
) -> Option<SongRes> {
    let prefer = params.bool("preferOmm").unwrap_or(false);
    let by_id = params
        .get("ommId")
        .filter(|id| omm::valid_id(id, "song"))
        .and_then(|id| match client.catalog_song(id) {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!("omm lookup for {id} failed: {e:#}");
                None
            }
        });
    let matched = match by_id {
        Some(m) => Some(m),
        None if !meta.title.is_empty() && params.get("ommId").is_none() => client
            .identify_song(
                &meta.title,
                meta.artists.first().map(String::as_str).unwrap_or(""),
                meta.album.as_deref(),
                duration_ms,
            )
            .unwrap_or_else(|e| {
                tracing::warn!("omm identify failed: {e:#}");
                None
            })
            .filter(|m| omm::accept_song(m, &meta.title, &meta.artists, duration_ms, true)),
        None => None,
    }?;

    let a = &matched.attributes;
    if (prefer || meta.title.is_empty()) && !a.name.is_empty() {
        meta.title = a.name.clone();
    }
    if (prefer || meta.artists.is_empty()) && !a.artist_names.is_empty() {
        meta.artists = a.artist_names.clone();
    }
    if (prefer || meta.album.is_none())
        && let Some(album) = omm::related_album(&matched, meta.album.as_deref())
            .map(|al| al.attributes.name.clone())
            .or_else(|| a.album_name.clone())
    {
        meta.album = Some(album);
    }
    if prefer || meta.track.is_none() {
        meta.track = a
            .track_number
            .and_then(|t| u32::try_from(t).ok())
            .or(meta.track);
    }
    if prefer || meta.disc.is_none() {
        meta.disc = a
            .disc_number
            .and_then(|t| u32::try_from(t).ok())
            .or(meta.disc);
    }
    if (prefer || meta.genres.is_empty()) && !a.genres.is_empty() {
        meta.genres = a.genres.clone();
    }
    if (prefer || meta.year.is_none())
        && let Some(date) = a.release_date.as_deref()
    {
        meta.year = date.get(..4).and_then(|y| y.parse().ok()).or(meta.year);
        meta.date = Some(date.to_string());
    }
    if meta.isrc.is_none() {
        meta.isrc = a.isrc.clone();
    }
    Some(matched)
}

fn prepare(
    path: &Path,
    params: &Params,
    original: Option<&str>,
    client: Option<&omm::Client>,
) -> Result<(TrackInfo, Meta, Option<SongRes>), ApiError> {
    let first = read_track(path).map_err(|e| {
        tracing::warn!("upload rejected {}: {e:#}", path.display());
        bad_upload("not a valid audio file")
    })?;
    let old = meta_from_track(&first);
    let mut new = old.clone();
    let matched = client.and_then(|c| apply_omm(&mut new, params, c, first.duration_ms as i64));
    let explicit = apply_overrides(&mut new, params)?;
    apply_fallbacks(&mut new, params, original);

    if new == old {
        return Ok((first, new, matched));
    }
    match write_tags(path, &new, &old) {
        Ok(()) => {}
        Err(e) if explicit => {
            return Err(bad_upload(format!(
                "cannot write tags to this file format: {e:#}"
            )));
        }
        Err(e) => tracing::warn!("could not write fallback tags: {e:#}"),
    }
    let info = read_track(path).map_err(|_| bad_upload("file became unreadable after tagging"))?;
    Ok((info, new, matched))
}

async fn song_json(st: &AppState, uid: &str, id: &str) -> Result<serde_json::Value, ApiError> {
    let (uid, id) = (uid.to_string(), id.to_string());
    st.db
        .run(move |c| {
            let rows = songs_by_ids(c, &uid, std::slice::from_ref(&id))?;
            if rows.is_empty() {
                return Err(ApiError::not_found("song"));
            }
            Ok(songs_json(c, &rows)?.remove(0))
        })
        .await
}

async fn process_song(
    st: &AppState,
    user: &User,
    params: &Params,
    work: &mut PathBuf,
    original: Option<String>,
    library: &Library,
) -> Result<(&'static str, String), ApiError> {
    let probe_path = work.clone();
    let ext = tokio::task::spawn_blocking(move || detect_extension(&probe_path))
        .await
        .map_err(|_| ApiError::generic("internal server error"))??;
    let typed = work.with_extension(ext);
    tokio::fs::rename(&*work, &typed).await?;
    *work = typed;

    let (path, p, orig, client) = (
        work.clone(),
        params.clone(),
        original,
        st.omm_upload.clone(),
    );
    let (info, meta, matched) =
        tokio::task::spawn_blocking(move || prepare(&path, &p, orig.as_deref(), client.as_deref()))
            .await
            .map_err(|_| ApiError::generic("internal server error"))??;

    let _guard = st.upload_lock.lock().await;

    let relative = target_relative(&meta, ext);
    let root = &library.path;
    let final_path = unique_path(root.join(&relative));
    if !upload::contained(root, &final_path) {
        return Err(bad_upload("invalid target path"));
    }
    let predicted = st.scanner.predict_song_id(library, &final_path, &info);
    let existing: Option<String> = {
        let id = predicted.clone();
        st.db
            .run(move |c| {
                Ok(
                    c.query_row("SELECT path FROM songs WHERE id = ?1", [id], |r| r.get(0))
                        .optional()?,
                )
            })
            .await?
    };
    if let Some(existing_path) = existing
        && Path::new(&existing_path).exists()
    {
        return Ok(("duplicate", predicted));
    }

    if let Some(parent) = final_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::rename(&*work, &final_path).await?;
    let scanner = st.scanner.clone();
    let indexed = final_path.clone();
    let id = tokio::task::spawn_blocking(move || scanner.index_file(&indexed))
        .await
        .map_err(|_| ApiError::generic("internal server error"))?;
    let id = match id {
        Ok(id) => id,
        Err(e) => {
            let _ = tokio::fs::remove_file(&final_path).await;
            return Err(e);
        }
    };
    tracing::info!("{} uploaded {}", user.name, final_path.display());

    let valid = |key: &str, kind: &str| {
        params
            .get(key)
            .filter(|v| omm::valid_id(v, kind))
            .map(str::to_string)
    };
    let (omm_song, omm_album, omm_artist) = (
        valid("ommId", "song"),
        valid("ommAlbumId", "album"),
        valid("ommArtistId", "artist"),
    );
    if matched.is_some() || omm_song.is_some() || omm_album.is_some() || omm_artist.is_some() {
        let (song_id, album) = (id.clone(), meta.album.clone());
        st.db
            .run(move |c| {
                if let Some(m) = &matched {
                    omm::store_song_match(c, &song_id, m, album.as_deref())?;
                }
                omm::set_explicit_ids(
                    c,
                    &song_id,
                    omm_song.as_deref(),
                    omm_album.as_deref(),
                    omm_artist.as_deref(),
                )?;
                Ok(())
            })
            .await?;
    }
    if let Some(h) = &st.omm_handle {
        h.wake();
    }
    Ok(("created", id))
}

fn image_extension(path: &Path) -> Result<&'static str, ApiError> {
    use std::io::Read;
    let mut head = [0u8; 32];
    let n = std::fs::File::open(path)?.read(&mut head)?;
    let ext = match image::guess_format(&head[..n]) {
        Ok(image::ImageFormat::Jpeg) => "jpg",
        Ok(image::ImageFormat::Png) => "png",
        Ok(image::ImageFormat::WebP) => "webp",
        _ => return Err(bad_upload("cover must be a jpeg, png or webp image")),
    };
    let (w, h) = image::ImageReader::open(path)?
        .with_guessed_format()?
        .into_dimensions()
        .map_err(|_| bad_upload("cover is not a readable image"))?;
    if w > MAX_COVER_DIMENSION || h > MAX_COVER_DIMENSION {
        return Err(bad_upload("cover dimensions are too large"));
    }
    Ok(ext)
}

async fn process_cover(
    st: &AppState,
    params: &Params,
    work: &mut PathBuf,
) -> Result<serde_json::Value, ApiError> {
    let key = params
        .get("albumId")
        .or_else(|| params.get("songId"))
        .ok_or_else(|| ApiError::missing("albumId"))?
        .to_string();
    let probe = work.clone();
    let ext = tokio::task::spawn_blocking(move || image_extension(&probe))
        .await
        .map_err(|_| ApiError::generic("internal server error"))??;

    let (album_id, dir) = st
        .db
        .run(move |c| {
            let album_id = match kind_of(c, &key)? {
                Some(ItemKind::Album) => key.clone(),
                Some(ItemKind::Song) => {
                    c.query_row("SELECT album_id FROM songs WHERE id = ?1", [&key], |r| {
                        r.get::<_, String>(0)
                    })?
                }
                _ => return Err(ApiError::not_found("album")),
            };
            let path: String = c.query_row(
                "SELECT path FROM songs WHERE album_id = ?1 ORDER BY disc, track LIMIT 1",
                [&album_id],
                |r| r.get(0),
            )?;
            let dir = Path::new(&path)
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| ApiError::not_found("album directory"))?;
            Ok((album_id, dir))
        })
        .await?;

    let _guard = st.upload_lock.lock().await;
    for old_ext in ["jpg", "jpeg", "png", "webp"] {
        let _ = tokio::fs::remove_file(dir.join(format!("cover.{old_ext}"))).await;
    }
    let target = dir.join(format!("cover.{ext}"));
    tokio::fs::rename(&*work, &target).await?;

    let scanner = st.scanner.clone();
    let album = album_id.clone();
    tokio::task::spawn_blocking(move || scanner.refresh_albums(&HashSet::from([album])))
        .await
        .map_err(|_| ApiError::generic("internal server error"))??;
    Ok(json!({"status": "created", "kind": "cover", "albumId": album_id}))
}

async fn process_lyrics(
    st: &AppState,
    params: &Params,
    work: &mut PathBuf,
) -> Result<serde_json::Value, ApiError> {
    let song_id = params.req("songId")?.to_string();
    let bytes = tokio::fs::read(&*work).await?;
    let text = String::from_utf8(bytes).map_err(|_| bad_upload("lyrics must be utf-8 text"))?;
    let synced = text.lines().any(|l| {
        l.trim_start().starts_with('[') && l.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
    });
    let ext = if synced { "lrc" } else { "txt" };

    let id = song_id.clone();
    let song_path: String = st
        .db
        .run(move |c| {
            c.query_row("SELECT path FROM songs WHERE id = ?1", [id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| ApiError::not_found("song"))
        })
        .await?;
    let song_path = Path::new(&song_path);
    let _guard = st.upload_lock.lock().await;
    for other in ["lrc", "txt"] {
        let _ = tokio::fs::remove_file(song_path.with_extension(other)).await;
    }
    tokio::fs::rename(&*work, song_path.with_extension(ext)).await?;
    Ok(json!({"status": "created", "kind": "lyrics", "songId": song_id}))
}

pub async fn upload_song(ctx: Ctx) -> ApiResult {
    let st = ctx.st.clone();
    let mut params = ctx.params.clone();
    let mut user: Option<User> = None;
    if params.has("apiKey") || params.has("u") {
        let u =
            auth::authenticate(&st, &params, &crate::ratelimit::client_key(&ctx.headers)).await?;
        check_role(&st, &u)?;
        user = Some(u);
    }

    let content_type = ctx
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let boundary = multer::parse_boundary(&content_type)
        .map_err(|_| bad_upload("multipart/form-data body required"))?;
    let max = st.cfg.upload.max_size;
    if let Some(len) = ctx
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        && len > max.saturating_add(1024 * 1024)
    {
        return Err(bad_upload(format!("file exceeds the {max} byte limit")));
    }
    let body = ctx
        .take_body()
        .ok_or_else(|| bad_upload("missing request body"))?;
    let constraints = multer::Constraints::new()
        .size_limit(multer::SizeLimit::new().whole_stream(max.saturating_add(MULTIPART_OVERHEAD)));
    let mut multipart =
        multer::Multipart::with_constraints(body.into_data_stream(), boundary, constraints);
    let mut text_fields = 0usize;

    loop {
        let mut field = multipart
            .next_field()
            .await
            .map_err(|e| bad_upload(format!("malformed multipart body: {e}")))?
            .ok_or_else(|| ApiError::missing("file"))?;
        let name = field.name().unwrap_or("").to_string();
        if name != "file" {
            text_fields += 1;
            if text_fields > MAX_TEXT_FIELDS {
                return Err(bad_upload("too many form fields before the file part"));
            }
            let mut buf: Vec<u8> = Vec::new();
            while let Some(chunk) = field
                .chunk()
                .await
                .map_err(|e| bad_upload(format!("malformed multipart body: {e}")))?
            {
                if buf.len() + chunk.len() > MAX_TEXT_FIELD {
                    return Err(bad_upload("form field too large"));
                }
                buf.extend_from_slice(&chunk);
            }
            params
                .0
                .push((name, String::from_utf8_lossy(&buf).into_owned()));
            continue;
        }

        let user = match user.take() {
            Some(u) => u,
            None => {
                let u =
                    auth::authenticate(&st, &params, &crate::ratelimit::client_key(&ctx.headers))
                        .await?;
                check_role(&st, &u)?;
                u
            }
        };

        let kind = params.get("kind").unwrap_or("song").to_string();
        let original = field.file_name().map(str::to_string);
        let library = target_library(&st, &user, &params, &kind).await?;
        let tmp_dir = library.path.join(TMP_DIR);
        let limit = match kind.as_str() {
            "song" => max,
            "cover" => st.cfg.upload.max_companion_size,
            "lyrics" => MAX_LYRICS.min(st.cfg.upload.max_companion_size),
            _ => return Err(ApiError::bad_param("kind")),
        };
        let (tmp, _) = receive(&mut field, &tmp_dir, limit).await?;
        let mut work = tmp.clone();

        let result = match kind.as_str() {
            "cover" => process_cover(&st, &params, &mut work).await,
            "lyrics" => process_lyrics(&st, &params, &mut work).await,
            _ => match process_song(&st, &user, &params, &mut work, original, &library).await {
                Ok((status, id)) => match song_json(&st, &user.id, &id).await {
                    Ok(song) => Ok(json!({"status": status, "song": song})),
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            },
        };
        let _ = tokio::fs::remove_file(&work).await;
        let _ = tokio::fs::remove_file(&tmp).await;
        return ok(json!({"juuzUploadResult": result?}));
    }
}

fn has_audio(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .any(|e| crate::scanner::tags::is_audio(&e.path()))
        })
        .unwrap_or(false)
}

fn prune_dirs(root: &Path, start: &Path) {
    let mut dir = start.to_path_buf();
    while dir != root && dir.starts_with(root) {
        if !has_audio(&dir) {
            for name in ["cover", "folder", "front"] {
                for ext in ["jpg", "jpeg", "png", "webp"] {
                    let _ = std::fs::remove_file(dir.join(format!("{name}.{ext}")));
                }
            }
        }
        if std::fs::remove_dir(&dir).is_err() {
            break;
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
}

pub async fn delete_song(ctx: Ctx) -> ApiResult {
    let user = ctx.user().clone();
    if !ctx.st.cfg.upload.enabled {
        return Err(ApiError::forbidden("uploads are disabled on this server"));
    }
    if !user.can_delete() {
        return Err(ApiError::forbidden(
            "user is not authorized to delete songs",
        ));
    }
    let ids: Vec<String> = ctx
        .params
        .all("id")
        .into_iter()
        .map(str::to_string)
        .collect();
    if ids.is_empty() {
        return Err(ApiError::missing("id"));
    }
    let actor = user.clone();
    let found = ctx
        .st
        .db
        .run(move |c| {
            let mut out = Vec::new();
            for id in &ids {
                let row: Option<(String, i64)> = c
                    .query_row(
                        "SELECT path, library_id FROM songs WHERE id = ?1",
                        params![id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let (path, library_id) = row.ok_or_else(|| ApiError::not_found("song"))?;
                let library = writable_library(c, &actor, library_id)?;
                if !upload::contained(&library.path, Path::new(&path)) {
                    return Err(ApiError::forbidden("song is outside its library"));
                }
                out.push((path, library.path));
            }
            Ok(out)
        })
        .await?;

    let _guard = ctx.st.upload_lock.lock().await;
    for (path, root) in found {
        let path = PathBuf::from(path);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        for ext in ["lrc", "txt"] {
            let _ = tokio::fs::remove_file(path.with_extension(ext)).await;
        }
        let scanner = ctx.st.scanner.clone();
        let p = path.clone();
        tokio::task::spawn_blocking(move || scanner.remove_path(&p, true))
            .await
            .map_err(|_| ApiError::generic("internal server error"))??;
        if let Some(parent) = path.parent() {
            let (root, parent) = (root.clone(), parent.to_path_buf());
            tokio::task::spawn_blocking(move || prune_dirs(&root, &parent))
                .await
                .map_err(|_| ApiError::generic("internal server error"))?;
        }
        tracing::info!("{} deleted {}", user.name, path.display());
    }
    empty()
}
