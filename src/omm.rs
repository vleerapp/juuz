use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;

use crate::config::OmmConfig;
use crate::db::{Db, now, now_ms};
use crate::ids;
use crate::scanner::tags::{normalize_isrc, normalize_upc};

const REQUEST_INTERVAL: Duration = Duration::from_millis(60);
const RATE_LIMIT_RETRIES: u32 = 5;
const MAX_RETRY_AFTER_SECS: u64 = 60;
const MAX_LOOKUP_VALUES: usize = 100;
const MAX_LOOKUP_CHARS: usize = 10_000;
const MAX_NAME_CHARS: usize = 256;
const BATCH: usize = 24;
const MAX_IMAGE_BYTES: u64 = 15 * 1024 * 1024;
const ART_RETRY_SECS: i64 = 7 * 24 * 3600;
const LOOKUP_THREADS: usize = 4;
const UNRESOLVED_RETRY_SECS: i64 = 14 * 24 * 3600;
const DURATION_TOLERANCE_MS: i64 = 5_000;

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Resource {
    Song(SongRes),
    Album(AlbumRes),
    Artist(ArtistRes),
}

#[derive(Clone, Debug, Deserialize)]
pub struct SongRes {
    pub id: String,
    pub attributes: SongAttrs,
    #[serde(default)]
    pub relationships: Option<SongRels>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SongAttrs {
    pub name: String,
    pub album_name: Option<String>,
    pub artist_names: Vec<String>,
    pub isrc: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub genres: Vec<String>,
    pub release_date: Option<String>,
    pub duration_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SongRels {
    #[serde(default)]
    pub albums: Option<List<AlbumRes>>,
    #[serde(default)]
    pub artists: Option<List<ArtistRes>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct List<T> {
    #[serde(default = "Vec::new")]
    pub data: Vec<T>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AlbumRes {
    pub id: String,
    pub attributes: AlbumAttrs,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AlbumAttrs {
    pub name: String,
    pub upc: Option<String>,
    pub label: Option<String>,
    pub release_date: Option<String>,
    pub artist_names: Vec<String>,
    pub genres: Vec<String>,
    pub artwork_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ArtistRes {
    pub id: String,
    pub attributes: ArtistAttrs,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ArtistAttrs {
    pub name: String,
    pub artwork_url: Option<String>,
}

#[derive(Deserialize)]
struct Single {
    data: Resource,
}

#[derive(Deserialize)]
struct Many {
    data: Vec<Resource>,
}

impl Resource {
    fn into_song(self) -> Option<SongRes> {
        match self {
            Resource::Song(s) => Some(s),
            _ => None,
        }
    }

    fn into_album(self) -> Option<AlbumRes> {
        match self {
            Resource::Album(a) => Some(a),
            _ => None,
        }
    }

    fn into_artist(self) -> Option<ArtistRes> {
        match self {
            Resource::Artist(a) => Some(a),
            _ => None,
        }
    }
}

pub fn valid_id(id: &str, kind: &str) -> bool {
    id.strip_prefix("omm:")
        .and_then(|r| r.strip_prefix(kind))
        .and_then(|r| r.strip_prefix(':'))
        .is_some_and(|tail| {
            (1..=18).contains(&tail.len())
                && !tail.starts_with('0')
                && tail.bytes().all(|b| b.is_ascii_digit())
        })
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn similar(a: &str, b: &str) -> bool {
    let (na, nb) = (normalize(a), normalize(b));
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    if na == nb {
        return true;
    }
    let (short, long) = if na.len() <= nb.len() {
        (&na, &nb)
    } else {
        (&nb, &na)
    };
    if short.len() >= 5 && long.contains(short.as_str()) {
        return true;
    }
    let tokens = |s: &str| -> std::collections::HashSet<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let (ta, tb) = (tokens(a), tokens(b));
    let inter = ta.intersection(&tb).count() as f64;
    let union = ta.union(&tb).count() as f64;
    union > 0.0 && inter / union >= 0.75
}

fn clip(value: &str) -> &str {
    match value.char_indices().nth(MAX_NAME_CHARS) {
        Some((end, _)) => &value[..end],
        None => value,
    }
}

fn lookup_chunks(values: &[String]) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut count = 0usize;
    for value in values {
        if value.is_empty() || value.contains(',') {
            continue;
        }
        let extra = if current.is_empty() {
            value.len()
        } else {
            value.len() + 1
        };
        if count >= MAX_LOOKUP_VALUES || current.len() + extra > MAX_LOOKUP_CHARS {
            chunks.push(std::mem::take(&mut current));
            count = 0;
        }
        if !current.is_empty() {
            current.push(',');
        }
        current.push_str(value);
        count += 1;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

pub struct Client {
    base: String,
    allow_http_art: bool,
    agent: ureq::Agent,
    last: Mutex<Option<Instant>>,
}

impl Client {
    pub fn new(cfg: &OmmConfig) -> Client {
        Client::with_timeout(cfg, cfg.timeout_secs)
    }

    pub fn for_uploads(cfg: &OmmConfig) -> Client {
        Client::with_timeout(cfg, cfg.upload_timeout_secs)
    }

    fn with_timeout(cfg: &OmmConfig, secs: u64) -> Client {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(secs.max(1))))
            .http_status_as_error(false)
            .user_agent(concat!("juuz/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Client {
            base: cfg.base_url.trim_end_matches('/').to_string(),
            allow_http_art: cfg.base_url.starts_with("http://"),
            agent,
            last: Mutex::new(None),
        }
    }

    fn throttle(&self) {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < REQUEST_INTERVAL {
                std::thread::sleep(REQUEST_INTERVAL - elapsed);
            }
        }
        *last = Some(Instant::now());
    }

    fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<Option<T>> {
        let url = format!("{}{path}", self.base);
        for _ in 0..=RATE_LIMIT_RETRIES {
            self.throttle();
            let mut req = self.agent.get(&url);
            for (k, v) in query {
                req = req.query(*k, *v);
            }
            let mut resp = req.call()?;
            match resp.status().as_u16() {
                200 => return Ok(Some(resp.body_mut().read_json::<T>()?)),
                400 | 404 => return Ok(None),
                429 => {
                    let wait = ["retry-after", "x-ratelimit-after"]
                        .iter()
                        .find_map(|h| {
                            resp.headers()
                                .get(*h)?
                                .to_str()
                                .ok()?
                                .trim()
                                .parse::<u64>()
                                .ok()
                        })
                        .unwrap_or(1)
                        .clamp(1, MAX_RETRY_AFTER_SECS);
                    tracing::debug!("omm rate limited, waiting {wait}s");
                    std::thread::sleep(Duration::from_secs(wait));
                }
                504 => {
                    let body = resp.body_mut().read_to_string().unwrap_or_default();
                    if body.contains("Match failed") {
                        return Ok(None);
                    }
                    return Err(anyhow!("{path} returned HTTP 504"));
                }
                s => return Err(anyhow!("{path} returned HTTP {s}")),
            }
        }
        Err(anyhow!("{path} stayed rate limited"))
    }

    fn catalog(&self, param: &str, values: &[String]) -> Result<Vec<Resource>> {
        let mut found = Vec::new();
        for joined in lookup_chunks(values) {
            if let Some(many) = self.get::<Many>("/catalog", &[(param, joined.as_str())])? {
                found.extend(many.data);
            }
        }
        Ok(found)
    }

    pub fn fetch_image(&self, template: &str) -> Result<Vec<u8>> {
        let url = template.replace("{w}", "600").replace("{h}", "600");
        let plain_http_ok = self.allow_http_art && url.starts_with("http://");
        if !url.starts_with("https://") && !plain_http_ok {
            bail!("artwork url is not https");
        }
        let mut resp = self.agent.get(&url).call()?;
        let status = resp.status().as_u16();
        if status != 200 {
            bail!("artwork returned HTTP {status}");
        }
        let bytes = resp
            .body_mut()
            .with_config()
            .limit(MAX_IMAGE_BYTES)
            .read_to_vec()?;
        image::guess_format(&bytes).map_err(|_| anyhow!("artwork is not an image"))?;
        Ok(bytes)
    }

    pub fn catalog_song(&self, id: &str) -> Result<Option<SongRes>> {
        if !valid_id(id, "song") {
            return Ok(None);
        }
        Ok(self
            .get::<Single>(&format!("/catalog/{id}"), &[])?
            .and_then(|s| s.data.into_song()))
    }

    pub fn songs_by_ids(&self, ids: &[String]) -> Result<Vec<SongRes>> {
        Ok(self
            .catalog("ids", ids)?
            .into_iter()
            .filter_map(Resource::into_song)
            .collect())
    }

    pub fn songs_by_isrc(&self, isrcs: &[String]) -> Result<HashMap<String, Vec<SongRes>>> {
        let mut found: HashMap<String, Vec<SongRes>> = HashMap::new();
        for r in self.catalog("isrc", isrcs)? {
            if let Some(song) = r.into_song()
                && let Some(isrc) = song.attributes.isrc.as_deref().and_then(normalize_isrc)
            {
                found.entry(isrc).or_default().push(song);
            }
        }
        Ok(found)
    }

    pub fn albums_by_upc(&self, upcs: &[String]) -> Result<HashMap<String, Vec<AlbumRes>>> {
        let mut found: HashMap<String, Vec<AlbumRes>> = HashMap::new();
        for r in self.catalog("upc", upcs)? {
            if let Some(album) = r.into_album()
                && let Some(upc) = album.attributes.upc.as_deref().and_then(normalize_upc)
            {
                found
                    .entry(upc.trim_start_matches('0').to_string())
                    .or_default()
                    .push(album);
            }
        }
        Ok(found)
    }

    pub fn identify_song(
        &self,
        title: &str,
        artist: &str,
        album: Option<&str>,
        duration_ms: i64,
    ) -> Result<Option<SongRes>> {
        let duration = duration_ms.to_string();
        let mut q = vec![("name", clip(title.trim()))];
        if !artist.trim().is_empty() {
            q.push(("artist", clip(artist.trim())));
        }
        if let Some(a) = album.map(str::trim).filter(|a| !a.is_empty()) {
            q.push(("album", clip(a)));
        }
        if duration_ms > 0 {
            q.push(("duration", &duration));
        }
        Ok(self
            .get::<Single>("/identify/song", &q)?
            .and_then(|s| s.data.into_song()))
    }

    pub fn search_songs(
        &self,
        title: &str,
        artist: &str,
        album: Option<&str>,
        duration_ms: i64,
        limit: usize,
    ) -> Result<Vec<SongRes>> {
        let duration = duration_ms.to_string();
        let limit_text = limit.to_string();
        let mut q = vec![("name", clip(title.trim())), ("limit", limit_text.as_str())];
        if !artist.trim().is_empty() {
            q.push(("artist", clip(artist.trim())));
        }
        if let Some(a) = album.map(str::trim).filter(|a| !a.is_empty()) {
            q.push(("album", clip(a)));
        }
        if duration_ms > 0 {
            q.push(("duration", &duration));
        }
        match self.get::<Many>("/search/song", &q)? {
            Some(many) => Ok(many
                .data
                .into_iter()
                .filter_map(Resource::into_song)
                .collect()),
            None => Ok(self
                .identify_song(title, artist, album, duration_ms)?
                .into_iter()
                .collect()),
        }
    }

    pub fn identify_album(&self, title: &str, artist: &str) -> Result<Option<AlbumRes>> {
        let mut q = vec![("name", clip(title.trim()))];
        if !artist.trim().is_empty() {
            q.push(("artist", clip(artist.trim())));
        }
        Ok(self
            .get::<Single>("/identify/album", &q)?
            .and_then(|s| s.data.into_album()))
    }

    pub fn identify_artist(&self, name: &str) -> Result<Option<ArtistRes>> {
        Ok(self
            .get::<Single>("/identify/artist", &[("name", clip(name.trim()))])?
            .and_then(|s| s.data.into_artist()))
    }
}

pub fn accept_song(
    c: &SongRes,
    title: &str,
    artists: &[String],
    duration_ms: i64,
    strict: bool,
) -> bool {
    if !similar(&c.attributes.name, title) {
        return false;
    }
    let have_artists = !artists.is_empty() && !c.attributes.artist_names.is_empty();
    if have_artists {
        let artist_ok = artists
            .iter()
            .any(|l| c.attributes.artist_names.iter().any(|r| similar(l, r)));
        if artist_ok {
            return true;
        }
        if strict {
            return false;
        }
    }
    match c.attributes.duration_ms {
        Some(d) if duration_ms > 0 => {
            (d - duration_ms).abs() <= DURATION_TOLERANCE_MS * if strict { 1 } else { 3 }
        }
        _ => !strict,
    }
}

pub fn related_album<'a>(song: &'a SongRes, local_album: Option<&str>) -> Option<&'a AlbumRes> {
    let albums = &song.relationships.as_ref()?.albums.as_ref()?.data;
    local_album
        .and_then(|local| albums.iter().find(|a| similar(&a.attributes.name, local)))
        .or_else(|| (albums.len() == 1).then(|| &albums[0]))
}

fn seed_album_songs(conn: &Connection, album_id: &str, a: &AlbumAttrs) -> rusqlite::Result<()> {
    let year: Option<i64> = a
        .release_date
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse().ok());
    conn.execute(
        "UPDATE albums SET year = COALESCE(year, ?2), date = COALESCE(date, ?3) WHERE id = ?1",
        params![album_id, year, a.release_date],
    )?;
    conn.execute(
        "UPDATE songs SET year = COALESCE(year, ?2), date = COALESCE(date, ?3) WHERE album_id = ?1",
        params![album_id, year, a.release_date],
    )?;
    if a.genres.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT id FROM songs s WHERE album_id = ?1
           AND NOT EXISTS (SELECT 1 FROM song_genres g WHERE g.song_id = s.id)",
    )?;
    let ids: Vec<String> = stmt
        .query_map([album_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for song_id in ids {
        for (i, g) in a.genres.iter().enumerate() {
            let gid = ids::genre(g);
            conn.execute(
                "INSERT OR IGNORE INTO genres(id, name) VALUES (?1, ?2)",
                params![gid, g],
            )?;
            conn.execute(
                "INSERT OR IGNORE INTO song_genres(song_id, genre_id, position) VALUES (?1, ?2, ?3)",
                params![song_id, gid, i as i64],
            )?;
        }
    }
    Ok(())
}

pub fn store_song_match(
    conn: &Connection,
    song_id: &str,
    m: &SongRes,
    local_album: Option<&str>,
) -> rusqlite::Result<()> {
    let ts = now();
    conn.execute(
        "UPDATE songs SET omm_id = ?2, omm_checked_at = ?3, omm_synced_at = ?4, omm_retry_at = NULL WHERE id = ?1",
        params![song_id, m.id, ts, now_ms()],
    )?;
    let year: Option<i64> = m
        .attributes
        .release_date
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse().ok());
    conn.execute(
        "UPDATE songs SET year = COALESCE(year, ?2), date = COALESCE(date, ?3), isrc = COALESCE(isrc, ?4),
             track = COALESCE(track, ?5), disc = COALESCE(disc, ?6) WHERE id = ?1",
        params![
            song_id,
            year,
            m.attributes.release_date,
            m.attributes.isrc.as_deref().and_then(normalize_isrc),
            m.attributes.track_number,
            m.attributes.disc_number
        ],
    )?;
    let has_genres: bool = conn
        .query_row(
            "SELECT 1 FROM song_genres WHERE song_id = ?1 LIMIT 1",
            [song_id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !has_genres {
        for (i, g) in m.attributes.genres.iter().enumerate() {
            let gid = ids::genre(g);
            conn.execute(
                "INSERT OR IGNORE INTO genres(id, name) VALUES (?1, ?2)",
                params![gid, g],
            )?;
            conn.execute(
                "INSERT OR IGNORE INTO song_genres(song_id, genre_id, position) VALUES (?1, ?2, ?3)",
                params![song_id, gid, i as i64],
            )?;
        }
    }
    let album_id: Option<String> = conn
        .query_row("SELECT album_id FROM songs WHERE id = ?1", [song_id], |r| {
            r.get(0)
        })
        .optional()?;
    if let (Some(album_id), Some(album)) = (album_id, related_album(m, local_album)) {
        conn.execute(
            "UPDATE albums SET omm_id = COALESCE(omm_id, ?2), omm_checked_at = ?5, upc = COALESCE(upc, ?3),
                 label = COALESCE(label, ?4), omm_art_url = COALESCE(omm_art_url, ?6) WHERE id = ?1",
            params![
                album_id,
                album.id,
                album.attributes.upc.as_deref().and_then(normalize_upc),
                album.attributes.label,
                ts,
                album.attributes.artwork_url
            ],
        )?;
        seed_album_songs(conn, &album_id, &album.attributes)?;
    }
    if let Some(rels) = m.relationships.as_ref().and_then(|r| r.artists.as_ref()) {
        let mut stmt = conn.prepare(
            "SELECT a.id, a.name FROM song_artists sa JOIN artists a ON a.id = sa.artist_id WHERE sa.song_id = ?1",
        )?;
        let locals: Vec<(String, String)> = stmt
            .query_map([song_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for remote in &rels.data {
            if let Some((aid, _)) = locals
                .iter()
                .find(|(_, n)| similar(n, &remote.attributes.name))
            {
                conn.execute(
                    "UPDATE artists SET omm_id = COALESCE(omm_id, ?2), omm_checked_at = ?3,
                         omm_art_url = COALESCE(omm_art_url, ?4) WHERE id = ?1",
                    params![aid, remote.id, ts, remote.attributes.artwork_url],
                )?;
            }
        }
    }
    Ok(())
}

pub fn store_manual_match(
    conn: &Connection,
    song_id: &str,
    m: &SongRes,
    local_album: Option<&str>,
    file: Option<&crate::scanner::tags::TrackInfo>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE albums SET omm_id = NULL, omm_art_url = NULL, omm_art_tried_at = NULL,
             cover_hash = CASE WHEN cover_source = 'omm' THEN NULL ELSE cover_hash END,
             cover_path = CASE WHEN cover_source = 'omm' THEN NULL ELSE cover_path END,
             cover_source = CASE WHEN cover_source = 'omm' THEN NULL ELSE cover_source END
         WHERE id = (SELECT album_id FROM songs WHERE id = ?1)",
        [song_id],
    )?;
    conn.execute(
        "UPDATE artists SET omm_id = NULL, omm_art_url = NULL, omm_art_tried_at = NULL,
             image_hash = NULL, image_path = NULL
         WHERE id IN (SELECT artist_id FROM song_artists WHERE song_id = ?1)",
        [song_id],
    )?;
    conn.execute(
        "UPDATE songs SET year = ?2, date = ?3, isrc = ?4, track = ?5, disc = ?6 WHERE id = ?1",
        params![
            song_id,
            file.and_then(|t| t.year),
            file.and_then(|t| t.date.clone()),
            file.and_then(|t| t.isrc.as_deref())
                .and_then(normalize_isrc),
            file.and_then(|t| t.track),
            file.and_then(|t| t.disc),
        ],
    )?;
    conn.execute("DELETE FROM song_genres WHERE song_id = ?1", [song_id])?;
    for (i, g) in file
        .map(|t| t.genres.as_slice())
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let gid = ids::genre(g);
        conn.execute(
            "INSERT OR IGNORE INTO genres(id, name) VALUES (?1, ?2)",
            params![gid, g],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO song_genres(song_id, genre_id, position) VALUES (?1, ?2, ?3)",
            params![song_id, gid, i as i64],
        )?;
    }
    conn.execute(
        "UPDATE albums SET year = ?2, date = ?3, upc = ?4, label = ?5
         WHERE id = (SELECT album_id FROM songs WHERE id = ?1)",
        params![
            song_id,
            file.and_then(|t| t.year),
            file.and_then(|t| t.date.clone()),
            file.and_then(|t| t.upc.as_deref()).and_then(normalize_upc),
            file.and_then(|t| t.label.clone()),
        ],
    )?;
    store_song_match(conn, song_id, m, local_album)
}

pub fn clear_match(conn: &Connection, song_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE songs SET omm_id = NULL, omm_checked_at = NULL, omm_synced_at = NULL,
             omm_retry_at = ?2 WHERE id = ?1",
        params![song_id, now() + UNRESOLVED_RETRY_SECS],
    )?;
    Ok(())
}

pub fn set_explicit_ids(
    conn: &Connection,
    song_id: &str,
    omm_song: Option<&str>,
    omm_album: Option<&str>,
    omm_artist: Option<&str>,
) -> rusqlite::Result<()> {
    if let Some(id) = omm_song {
        conn.execute(
            "UPDATE songs SET omm_id = ?2, omm_checked_at = ?3, omm_retry_at = NULL WHERE id = ?1",
            params![song_id, id, now()],
        )?;
    }
    if let Some(id) = omm_album {
        conn.execute(
            "UPDATE albums SET omm_id = COALESCE(omm_id, ?2) WHERE id = (SELECT album_id FROM songs WHERE id = ?1)",
            params![song_id, id],
        )?;
    }
    if let Some(id) = omm_artist {
        conn.execute(
            "UPDATE artists SET omm_id = COALESCE(omm_id, ?2) WHERE id = (
                SELECT artist_id FROM song_artists WHERE song_id = ?1 ORDER BY position LIMIT 1)",
            params![song_id, id],
        )?;
    }
    Ok(())
}

type SongRow = (
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
);

struct SongWork {
    id: String,
    title: String,
    artist: String,
    artists: Vec<String>,
    album: Option<String>,
    duration_ms: i64,
    isrc: Option<String>,
    omm_id: Option<String>,
}

fn load_song_work(conn: &Connection, resolve: bool) -> Result<Vec<SongWork>> {
    let filter = if resolve {
        "s.omm_id IS NULL AND (s.omm_retry_at IS NULL OR s.omm_retry_at <= ?1)"
    } else {
        "s.omm_id IS NOT NULL AND (s.omm_synced_at IS NULL OR s.omm_synced_at < s.indexed_at) AND ?1 = ?1"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT s.id, s.title, s.display_artist, al.title, s.duration_ms, s.isrc, s.omm_id
         FROM songs s JOIN albums al ON al.id = s.album_id WHERE {filter} LIMIT {BATCH}"
    ))?;
    let rows: Vec<SongRow> = stmt
        .query_map([now()], |r| {
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
    for (id, title, display, album, duration_ms, isrc, omm_id) in rows {
        let mut stmt = conn.prepare(
            "SELECT a.name FROM song_artists sa JOIN artists a ON a.id = sa.artist_id WHERE sa.song_id = ?1 ORDER BY sa.position",
        )?;
        let artists: Vec<String> = stmt
            .query_map([&id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        out.push(SongWork {
            id,
            title,
            artist: artists.first().cloned().unwrap_or(display),
            artists,
            album: (!album.starts_with('[')).then_some(album),
            duration_ms,
            isrc,
            omm_id,
        });
    }
    Ok(out)
}

fn lookup_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(LOOKUP_THREADS)
            .thread_name(|i| format!("juuz-omm-lookup-{i}"))
            .build()
            .expect("omm lookup pool")
    })
}

fn parallel<T, R>(items: Vec<T>, f: impl Fn(T) -> R + Sync + Send) -> Vec<R>
where
    T: Send,
    R: Send,
{
    use rayon::prelude::*;
    lookup_pool().install(|| items.into_par_iter().map(f).collect())
}

const DEFER_SECS: i64 = 300;
const MAX_ITEM_FAILURES: usize = 3;

fn defer(conn: &Connection, table: &str, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!("UPDATE {table} SET omm_retry_at = ?2 WHERE id = ?1"),
        params![id, now() + DEFER_SECS],
    )?;
    Ok(())
}

fn tolerate<T>(
    conn: &Connection,
    table: &str,
    id: &str,
    failures: &mut usize,
    result: Result<T>,
) -> Result<Option<T>> {
    match result {
        Ok(v) => {
            *failures = 0;
            Ok(Some(v))
        }
        Err(e) => {
            *failures += 1;
            tracing::warn!("omm lookup for {table} {id} failed: {e:#}");
            defer(conn, table, id)?;
            if *failures >= MAX_ITEM_FAILURES {
                return Err(e);
            }
            Ok(None)
        }
    }
}

fn mark_unresolved(conn: &Connection, table: &str, id: &str) -> rusqlite::Result<()> {
    let ts = now();
    conn.execute(
        &format!("UPDATE {table} SET omm_checked_at = ?2, omm_retry_at = ?3 WHERE id = ?1"),
        params![id, ts, ts + UNRESOLVED_RETRY_SECS],
    )?;
    Ok(())
}

fn resolve_songs(db: &Db, client: &Client) -> Result<bool> {
    let conn = db.conn()?;
    let work = load_song_work(&conn, true)?;
    if work.is_empty() {
        return Ok(false);
    }
    tracing::debug!("omm: looking up {} songs", work.len());
    let isrcs: Vec<String> = work.iter().filter_map(|w| w.isrc.clone()).collect();
    let by_isrc = if isrcs.is_empty() {
        HashMap::new()
    } else {
        client.songs_by_isrc(&isrcs)?
    };

    let mut ready: Vec<(SongWork, Option<SongRes>)> = Vec::new();
    for w in work {
        let from_isrc = w
            .isrc
            .as_deref()
            .and_then(normalize_isrc)
            .and_then(|i| by_isrc.get(&i))
            .and_then(|c| {
                c.iter()
                    .filter(|c| accept_song(c, &w.title, &w.artists, w.duration_ms, false))
                    .min_by_key(|c| {
                        c.attributes
                            .duration_ms
                            .map(|d| (d - w.duration_ms).abs())
                            .unwrap_or(i64::MAX)
                    })
                    .cloned()
            });
        ready.push((w, from_isrc));
    }

    let looked = parallel(ready, |(w, from_isrc)| {
        let skip = w.title.trim().is_empty() || w.title.chars().count() > MAX_NAME_CHARS;
        let result = if skip || from_isrc.is_some() {
            Ok(from_isrc)
        } else {
            client
                .identify_song(&w.title, &w.artist, w.album.as_deref(), w.duration_ms)
                .map(|found| {
                    found.filter(|c| accept_song(c, &w.title, &w.artists, w.duration_ms, true))
                })
        };
        (w, result)
    });

    let mut failures = 0usize;
    let (mut resolved, mut unmatched, mut deferred) = (0usize, 0usize, 0usize);
    for (w, result) in looked {
        let Some(matched) = tolerate(&conn, "songs", &w.id, &mut failures, result)? else {
            deferred += 1;
            continue;
        };
        match matched {
            Some(m) => {
                store_song_match(&conn, &w.id, &m, w.album.as_deref())?;
                resolved += 1;
            }
            None => {
                mark_unresolved(&conn, "songs", &w.id)?;
                unmatched += 1;
            }
        }
    }
    tracing::info!("omm songs: {resolved} resolved, {unmatched} unmatched, {deferred} deferred");
    Ok(true)
}

fn resync_songs(db: &Db, client: &Client) -> Result<bool> {
    let conn = db.conn()?;
    let work = load_song_work(&conn, false)?;
    if work.is_empty() {
        return Ok(false);
    }
    let ids: Vec<String> = work.iter().filter_map(|w| w.omm_id.clone()).collect();
    let fetched: HashMap<String, SongRes> = client
        .songs_by_ids(&ids)?
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();
    for w in work {
        match w.omm_id.as_ref().and_then(|id| fetched.get(id)) {
            Some(m) => store_song_match(&conn, &w.id, m, w.album.as_deref())?,
            None => {
                conn.execute(
                    "UPDATE songs SET omm_synced_at = ?2 WHERE id = ?1",
                    params![w.id, now()],
                )?;
            }
        }
    }
    Ok(true)
}

fn resolve_albums(db: &Db, client: &Client) -> Result<bool> {
    let conn = db.conn()?;
    let rows: Vec<(String, String, String, Option<String>)> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT al.id, al.title, COALESCE((SELECT a.name FROM album_artists aa JOIN artists a ON a.id = aa.artist_id
                 WHERE aa.album_id = al.id ORDER BY aa.position LIMIT 1), ''), al.upc
             FROM albums al WHERE al.omm_id IS NULL AND al.title NOT LIKE '[%'
               AND (al.omm_retry_at IS NULL OR al.omm_retry_at <= ?1) LIMIT {BATCH}"
        ))?;
        stmt.query_map([now()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    if rows.is_empty() {
        return Ok(false);
    }
    tracing::debug!("omm: looking up {} albums", rows.len());
    let upcs: Vec<String> = rows.iter().filter_map(|r| r.3.clone()).collect();
    let by_upc = if upcs.is_empty() {
        HashMap::new()
    } else {
        client.albums_by_upc(&upcs)?
    };

    let looked = parallel(rows, |(id, title, artist, upc)| {
        let from_upc = upc
            .as_deref()
            .and_then(normalize_upc)
            .and_then(|u| by_upc.get(u.trim_start_matches('0')))
            .and_then(|c| {
                c.iter()
                    .find(|a| similar(&a.attributes.name, &title))
                    .cloned()
            });
        let result = match from_upc {
            Some(m) => Ok(Some(m)),
            None => client
                .identify_album(&title, &artist)
                .map(|found| found.filter(|a| similar(&a.attributes.name, &title))),
        };
        (id, result)
    });

    let mut failures = 0usize;
    let (mut resolved, mut unmatched) = (0usize, 0usize);
    for (id, result) in looked {
        let Some(matched) = tolerate(&conn, "albums", &id, &mut failures, result)? else {
            continue;
        };
        match matched {
            Some(m) => {
                conn.execute(
                    "UPDATE albums SET omm_id = ?2, omm_checked_at = ?5, omm_retry_at = NULL,
                         upc = COALESCE(upc, ?3), label = COALESCE(label, ?4),
                         omm_art_url = COALESCE(omm_art_url, ?6) WHERE id = ?1",
                    params![
                        id,
                        m.id,
                        m.attributes.upc.as_deref().and_then(normalize_upc),
                        m.attributes.label,
                        now(),
                        m.attributes.artwork_url
                    ],
                )?;
                seed_album_songs(&conn, &id, &m.attributes)?;
                resolved += 1;
            }
            None => {
                mark_unresolved(&conn, "albums", &id)?;
                unmatched += 1;
            }
        }
    }
    tracing::info!("omm albums: {resolved} resolved, {unmatched} unmatched");
    Ok(true)
}

fn resolve_artists(db: &Db, client: &Client) -> Result<bool> {
    let conn = db.conn()?;
    let rows: Vec<(String, String)> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT id, name FROM artists WHERE omm_id IS NULL AND name NOT LIKE '[%' AND name <> 'Various Artists'
               AND (omm_retry_at IS NULL OR omm_retry_at <= ?1) LIMIT {BATCH}"
        ))?;
        stmt.query_map([now()], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    if rows.is_empty() {
        return Ok(false);
    }
    tracing::debug!("omm: looking up {} artists", rows.len());
    let looked = parallel(rows, |(id, name)| {
        let result = if name.chars().count() > MAX_NAME_CHARS {
            Ok(None)
        } else {
            client
                .identify_artist(&name)
                .map(|found| found.filter(|a| similar(&a.attributes.name, &name)))
        };
        (id, result)
    });

    let mut failures = 0usize;
    let (mut resolved, mut unmatched) = (0usize, 0usize);
    for (id, result) in looked {
        let Some(matched) = tolerate(&conn, "artists", &id, &mut failures, result)? else {
            continue;
        };
        match matched {
            Some(m) => {
                conn.execute(
                    "UPDATE artists SET omm_id = ?2, omm_checked_at = ?3, omm_retry_at = NULL,
                         omm_art_url = COALESCE(omm_art_url, ?4) WHERE id = ?1",
                    params![id, m.id, now(), m.attributes.artwork_url],
                )?;
                resolved += 1;
            }
            None => {
                mark_unresolved(&conn, "artists", &id)?;
                unmatched += 1;
            }
        }
    }
    tracing::info!("omm artists: {resolved} resolved, {unmatched} unmatched");
    Ok(true)
}

fn refresh_art_urls(db: &Db, client: &Client) -> Result<bool> {
    let conn = db.conn()?;
    let cutoff = now() - ART_RETRY_SECS;
    let pending = |sql: String| -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_map([cutoff], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    };
    let albums = pending(format!(
        "SELECT id, omm_id FROM albums WHERE omm_id IS NOT NULL AND omm_art_url IS NULL AND cover_hash IS NULL
           AND (omm_art_tried_at IS NULL OR omm_art_tried_at < ?1) LIMIT {BATCH}"
    ))?;
    let artists = pending(format!(
        "SELECT id, omm_id FROM artists WHERE omm_id IS NOT NULL AND omm_art_url IS NULL AND image_path IS NULL
           AND (omm_art_tried_at IS NULL OR omm_art_tried_at < ?1) LIMIT {BATCH}"
    ))?;
    if albums.is_empty() && artists.is_empty() {
        return Ok(false);
    }
    let ids: Vec<String> = albums
        .iter()
        .chain(artists.iter())
        .map(|(_, omm)| omm.clone())
        .collect();
    let mut found: HashMap<String, Resource> = HashMap::new();
    for r in client.catalog("ids", &ids)? {
        let id = match &r {
            Resource::Album(a) => a.id.clone(),
            Resource::Artist(a) => a.id.clone(),
            Resource::Song(_) => continue,
        };
        found.insert(id, r);
    }
    let mut with_url = 0usize;
    for (id, omm_id) in albums {
        let album = match found.get(&omm_id) {
            Some(Resource::Album(a)) => Some(a),
            _ => None,
        };
        conn.execute(
            "UPDATE albums SET omm_art_url = ?2, omm_art_tried_at = CASE WHEN ?2 IS NULL THEN ?3 END WHERE id = ?1",
            params![
                id,
                album.and_then(|a| a.attributes.artwork_url.clone()),
                now()
            ],
        )?;
        if let Some(a) = album {
            seed_album_songs(&conn, &id, &a.attributes)?;
            with_url += usize::from(a.attributes.artwork_url.is_some());
        }
    }
    for (id, omm_id) in artists {
        let url = match found.get(&omm_id) {
            Some(Resource::Artist(a)) => a.attributes.artwork_url.clone(),
            _ => None,
        };
        with_url += usize::from(url.is_some());
        conn.execute(
            "UPDATE artists SET omm_art_url = ?2, omm_art_tried_at = CASE WHEN ?2 IS NULL THEN ?3 END WHERE id = ?1",
            params![id, url, now()],
        )?;
    }
    tracing::debug!("omm: {with_url} artwork urls found");
    Ok(true)
}

fn store_image(dir: &Path, bytes: &[u8]) -> Result<(String, String)> {
    let hash = format!("{:032x}", xxhash_rust::xxh3::xxh3_128(bytes));
    std::fs::create_dir_all(dir)?;
    let path = dir.join(&hash);
    if !path.exists() {
        let tmp = dir.join(format!(".{hash}.part"));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok((hash, path.to_string_lossy().into_owned()))
}

fn fetch_artwork(db: &Db, client: &Client, dir: &Path) -> Result<bool> {
    let conn = db.conn()?;
    let cutoff = now() - ART_RETRY_SECS;
    let pending = |sql: &str| -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = conn.prepare(sql)?;
        stmt.query_map([cutoff], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    };
    let albums = pending(&format!(
        "SELECT id, omm_art_url FROM albums WHERE omm_art_url IS NOT NULL AND cover_hash IS NULL
           AND (omm_art_tried_at IS NULL OR omm_art_tried_at < ?1) LIMIT {BATCH}"
    ))?;
    let artists = pending(&format!(
        "SELECT id, omm_art_url FROM artists WHERE omm_art_url IS NOT NULL AND image_path IS NULL
           AND (omm_art_tried_at IS NULL OR omm_art_tried_at < ?1) LIMIT {BATCH}"
    ))?;
    if albums.is_empty() && artists.is_empty() {
        return Ok(false);
    }

    let download = |(id, url): (String, String)| (id, client.fetch_image(&url));
    let album_results = parallel(albums, download);
    let artist_results = parallel(artists, download);

    let (mut covers, mut images, mut failed) = (0usize, 0usize, 0usize);
    let retry_soon = now() - ART_RETRY_SECS + 600;
    for (id, result) in album_results {
        match result.and_then(|bytes| store_image(dir, &bytes)) {
            Ok((hash, path)) => {
                conn.execute(
                    "UPDATE albums SET cover_hash = ?2, cover_path = ?3, cover_source = 'omm', omm_art_tried_at = ?4
                     WHERE id = ?1 AND cover_hash IS NULL",
                    params![id, hash, path, now()],
                )?;
                covers += 1;
            }
            Err(e) => {
                tracing::warn!("omm album artwork {id}: {e:#}");
                conn.execute(
                    "UPDATE albums SET omm_art_tried_at = ?2 WHERE id = ?1",
                    params![id, retry_soon],
                )?;
                failed += 1;
            }
        }
    }
    for (id, result) in artist_results {
        match result.and_then(|bytes| store_image(dir, &bytes)) {
            Ok((hash, path)) => {
                conn.execute(
                    "UPDATE artists SET image_hash = ?2, image_path = ?3, omm_art_tried_at = ?4 WHERE id = ?1",
                    params![id, hash, path, now()],
                )?;
                images += 1;
            }
            Err(e) => {
                tracing::warn!("omm artist image {id}: {e:#}");
                conn.execute(
                    "UPDATE artists SET omm_art_tried_at = ?2 WHERE id = ?1",
                    params![id, retry_soon],
                )?;
                failed += 1;
            }
        }
    }
    tracing::info!("omm artwork: {covers} album covers, {images} artist images, {failed} failed");
    Ok(true)
}

pub fn pass(db: &Db, client: &Client, art_dir: &Path) -> Result<bool> {
    if resolve_songs(db, client)? {
        return Ok(true);
    }
    if resync_songs(db, client)? {
        return Ok(true);
    }
    if resolve_albums(db, client)? {
        return Ok(true);
    }
    if resolve_artists(db, client)? {
        return Ok(true);
    }
    if refresh_art_urls(db, client)? {
        return Ok(true);
    }
    fetch_artwork(db, client, art_dir)
}

#[derive(Clone)]
pub struct Handle {
    signal: Arc<(Mutex<bool>, Condvar)>,
    stop: Arc<AtomicBool>,
}

impl Handle {
    pub fn wake(&self) {
        let (lock, cv) = &*self.signal;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cv.notify_all();
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.wake();
    }

    fn wait(&self, timeout: Duration) {
        let (lock, cv) = &*self.signal;
        let mut woken = lock.lock().unwrap_or_else(|e| e.into_inner());
        if !*woken {
            woken = cv
                .wait_timeout(woken, timeout)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *woken = false;
    }
}

pub fn spawn(db: Db, client: Arc<Client>, art_dir: PathBuf) -> Handle {
    let handle = Handle {
        signal: Arc::new((Mutex::new(false), Condvar::new())),
        stop: Arc::new(AtomicBool::new(false)),
    };
    let worker = handle.clone();
    let spawned = std::thread::Builder::new()
        .name("juuz-omm".into())
        .spawn(move || {
            let mut failures = 0u32;
            let mut idle = false;
            while !worker.stop.load(Ordering::Acquire) {
                match pass(&db, &client, &art_dir) {
                    Ok(true) => {
                        failures = 0;
                        idle = false;
                    }
                    Ok(false) => {
                        failures = 0;
                        if !idle {
                            tracing::info!(
                                "omm: everything resolvable is resolved, waiting for new items"
                            );
                            idle = true;
                        }
                        worker.wait(Duration::from_secs(15));
                    }
                    Err(e) => {
                        failures += 1;
                        let wait = Duration::from_secs((15 * u64::from(failures)).min(300));
                        tracing::warn!(
                            "omm lookup failed ({failures}): {e:#}; retrying in {wait:?}"
                        );
                        worker.wait(wait);
                    }
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("could not start omm worker: {e}");
    }
    handle
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_validation() {
        assert!(valid_id("omm:song:1234567890", "song"));
        assert!(!valid_id("omm:album:1234567890", "song"));
        assert!(!valid_id("omm:song:SHORT", "song"));
        assert!(!valid_id("song:1234567890", "song"));
        assert!(!valid_id("omm:song:0123456789", "song"));
        assert!(!valid_id("omm:song:", "song"));
        assert!(!valid_id("omm:song:1234567890123456789", "song"));
        assert!(valid_id("omm:song:123456789012345678", "song"));
        assert!(!valid_id("omm:song:0a1b2c3d4e5f6g7h", "song"));
    }

    #[test]
    fn similarity() {
        assert!(similar("Digital Love", "digital love"));
        assert!(similar("Digital Love (Remastered)", "Digital Love"));
        assert!(similar("Temple", "TEMPLE!"));
        assert!(!similar("Temple", "Castle"));
        assert!(!similar("", ""));
    }

    #[test]
    fn chunking() {
        let v: Vec<String> = (0..250).map(|i| format!("ISRC{i:08}")).collect();
        let chunks = lookup_chunks(&v);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.split(',').count() <= 100));
    }

    #[test]
    fn parses_song_with_relationships() {
        let json = r#"{"data":{"id":"omm:song:1234567890","type":"song",
            "attributes":{"name":"X","artistNames":["A"],"isrc":"USRC17607839","durationMs":1000,"genres":["Pop"]},
            "relationships":{"albums":{"data":[{"id":"omm:album:2345678901","type":"album",
            "attributes":{"name":"Y","trackCount":1,"artistNames":["A"],"upc":"602455878540"}}]},
            "artists":{"data":[{"id":"omm:artist:3456789012","type":"artist","attributes":{"name":"A"}}]}}}}"#;
        let s: Single = serde_json::from_str(json).unwrap();
        let song = s.data.into_song().unwrap();
        assert_eq!(song.attributes.genres, vec!["Pop"]);
        assert_eq!(
            related_album(&song, Some("y"))
                .unwrap()
                .attributes
                .upc
                .as_deref(),
            Some("602455878540")
        );
    }

    #[test]
    fn acceptance_rules() {
        let c = SongRes {
            id: "omm:song:1234567890".into(),
            attributes: SongAttrs {
                name: "Temple".into(),
                artist_names: vec!["Lifelikemoviee".into()],
                duration_ms: Some(181_000),
                ..Default::default()
            },
            relationships: None,
        };
        assert!(accept_song(
            &c,
            "Temple",
            &["Lifelikemoviee".into()],
            181_500,
            true
        ));
        assert!(!accept_song(
            &c,
            "Temple",
            &["Someone Else".into()],
            181_500,
            true
        ));
        assert!(!accept_song(&c, "Temple", &[], 120_000, true));
        assert!(!accept_song(&c, "Castle", &[], 181_000, false));
        assert!(
            accept_song(&c, "Temple", &["Lifelikemoviee".into()], 250_000, true),
            "same title and artist is the same song even when the edit length differs"
        );
    }
}
