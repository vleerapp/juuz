use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;

use crate::db::{Db, now};
use crate::error::ApiError;

const CLOSE_ENOUGH_SECS: u64 = 3;
const WAY_OFF_SECS: u64 = 10;
const SYNCED_SCORE: i64 = 200;
const ALBUM_SCORE: i64 = 15;
const MISS_TTL_SECS: i64 = 7 * 24 * 3600;

pub struct Found {
    pub text: String,
    pub title: String,
    pub artist: String,
}

pub struct Query<'a> {
    pub title: &'a str,
    pub artists: &'a [String],
    pub album: Option<&'a str>,
    pub duration: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hit {
    pub synced: Option<String>,
    pub plain: Option<String>,
    pub instrumental: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Track {
    id: Option<i64>,
    track_name: Option<String>,
    artist_name: Option<String>,
    album_name: Option<String>,
    duration: Option<f64>,
    instrumental: Option<bool>,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

fn non_empty(text: &Option<String>) -> Option<&str> {
    text.as_deref().map(str::trim).filter(|t| !t.is_empty())
}

impl Track {
    fn has_lyrics(&self) -> bool {
        non_empty(&self.synced_lyrics).is_some()
            || non_empty(&self.plain_lyrics).is_some()
            || self.instrumental == Some(true)
    }

    fn is_synced(&self) -> bool {
        non_empty(&self.synced_lyrics).is_some()
    }

    fn into_hit(self) -> Hit {
        Hit {
            synced: non_empty(&self.synced_lyrics).map(str::to_owned),
            plain: non_empty(&self.plain_lyrics).map(str::to_owned),
            instrumental: self.instrumental == Some(true),
        }
    }
}

pub struct LrclibClient {
    agent: ureq::Agent,
    root: String,
}

impl LrclibClient {
    pub fn new(root: &str) -> LrclibClient {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(8)))
            .http_status_as_error(false)
            .user_agent(concat!("juuz/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        LrclibClient {
            agent,
            root: root.trim_end_matches('/').to_string(),
        }
    }

    pub fn find(&self, query: &Query) -> Result<Vec<Hit>> {
        let Some(artist) = query.artists.first() else {
            return Ok(Vec::new());
        };

        let exact = self.get(query, artist)?.filter(Track::has_lyrics);
        if exact.as_ref().is_some_and(Track::is_synced) {
            return Ok(exact.into_iter().map(Track::into_hit).collect());
        }

        let mut ranked = rank(
            query,
            self.search(&[("track_name", query.title), ("artist_name", artist)])?,
        );
        if !ranked.iter().any(Track::is_synced) {
            let text = format!("{} {}", query.title, artist);
            if let Ok(extra) = self.search(&[("q", text.as_str())]) {
                ranked.extend(rank(query, extra));
            }
        }
        ranked.extend(exact);

        let mut seen = HashSet::new();
        ranked.retain(|track| track.id.is_none_or(|id| seen.insert(id)));
        ranked.sort_by_key(|track| std::cmp::Reverse(score(query, track)));
        Ok(ranked.into_iter().map(Track::into_hit).collect())
    }

    fn get(&self, query: &Query, artist: &str) -> Result<Option<Track>> {
        let mut params = vec![
            ("track_name", query.title.to_string()),
            ("artist_name", artist.to_string()),
        ];
        if let Some(album) = query.album {
            params.push(("album_name", album.to_string()));
        }
        if query.duration > 0 {
            params.push(("duration", query.duration.to_string()));
        }
        let mut resp = self.send("get", &params)?;
        match resp.status().as_u16() {
            200 => Ok(Some(resp.body_mut().read_json()?)),
            404 => Ok(None),
            s => Err(anyhow!("lrclib returned HTTP {s}")),
        }
    }

    fn search(&self, params: &[(&str, &str)]) -> Result<Vec<Track>> {
        let params: Vec<(&str, String)> = params.iter().map(|(k, v)| (*k, v.to_string())).collect();
        let mut resp = self.send("search", &params)?;
        match resp.status().as_u16() {
            200 => Ok(resp.body_mut().read_json()?),
            404 => Ok(Vec::new()),
            s => Err(anyhow!("lrclib returned HTTP {s}")),
        }
    }

    fn send(
        &self,
        endpoint: &str,
        params: &[(&str, String)],
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let request = || {
            let mut req = self.agent.get(format!("{}/{endpoint}", self.root));
            for (k, v) in params {
                req = req.query(*k, v.as_str());
            }
            req.call()
        };
        let resp = request()?;
        if resp.status().as_u16() != 429 {
            return Ok(resp);
        }
        let wait = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(2)
            .clamp(1, 5);
        std::thread::sleep(Duration::from_secs(wait));
        Ok(request()?)
    }
}

fn rank(query: &Query, tracks: Vec<Track>) -> Vec<Track> {
    let mut ranked: Vec<Track> = tracks
        .into_iter()
        .filter(|track| track.has_lyrics() && matches(query, track))
        .collect();
    ranked.sort_by_key(|track| std::cmp::Reverse(score(query, track)));
    ranked
}

fn drift(query: &Query, track: &Track) -> Option<u64> {
    let duration = track.duration?;
    (query.duration > 0).then(|| (duration.round() as i64).abs_diff(i64::from(query.duration)))
}

fn matches(query: &Query, track: &Track) -> bool {
    let title = track.track_name.as_deref().unwrap_or_default();
    let artist = track.artist_name.as_deref().unwrap_or_default();
    alike(title, query.title)
        && query.artists.iter().any(|a| artists_alike(artist, a))
        && drift(query, track).is_none_or(|d| d <= WAY_OFF_SECS)
}

fn score(query: &Query, track: &Track) -> i64 {
    let mut score = 0;
    if track.is_synced() {
        score += SYNCED_SCORE;
    }
    if let Some(d) = drift(query, track)
        && d <= CLOSE_ENOUGH_SECS
    {
        score += 100 - d as i64 * 10;
    }
    if let (Some(wanted), Some(named)) = (query.album, track.album_name.as_deref())
        && alike(wanted, named)
    {
        score += ALBUM_SCORE;
    }
    score
}

fn undecorated(text: &str) -> String {
    let text = text.split(" - ").next().unwrap_or(text);
    let mut depth = 0usize;
    text.chars()
        .filter(|c| match c {
            '(' | '[' => {
                depth += 1;
                false
            }
            ')' | ']' => {
                depth = depth.saturating_sub(1);
                false
            }
            _ => depth == 0,
        })
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn alike(left: &str, right: &str) -> bool {
    let (left, right) = (undecorated(left), undecorated(right));
    if left.is_empty() || right.is_empty() {
        return false;
    }
    if left == right {
        return true;
    }
    let (short, long) = if left.len() <= right.len() {
        (&left, &right)
    } else {
        (&right, &left)
    };
    long.contains(short.as_str()) && short.len() * 2 >= long.len()
}

fn artists_alike(left: &str, right: &str) -> bool {
    let names = |s: &str| {
        let mut text = s.to_lowercase();
        for joiner in [" feat. ", " feat ", " ft. ", " featuring ", " with ", " x "] {
            text = text.replace(joiner, ",");
        }
        text.split([',', '&', ';'])
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .collect::<Vec<_>>()
    };
    alike(left, right)
        || names(left)
            .iter()
            .any(|l| names(right).iter().any(|r| alike(l, r)))
}

struct Cached {
    content: Option<String>,
    fetched_at: i64,
}

fn cached(conn: &Connection, id: &str) -> rusqlite::Result<Option<Cached>> {
    conn.query_row(
        "SELECT content, fetched_at FROM lyrics WHERE song_id = ?1",
        [id],
        |r| {
            Ok(Cached {
                content: r.get(0)?,
                fetched_at: r.get(1)?,
            })
        },
    )
    .optional()
}

fn store(conn: &Connection, id: &str, content: Option<&str>, source: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO lyrics(song_id, source, content, fetched_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, source, content, now()],
    )?;
    Ok(())
}

struct SongInfo {
    path: String,
    title: String,
    display_artist: String,
    album: Option<String>,
    duration_s: u32,
    embedded: Option<String>,
    artists: Vec<String>,
}

fn song_info(conn: &Connection, id: &str) -> Result<SongInfo, ApiError> {
    let row = conn
        .query_row(
            "SELECT s.path, s.title, s.display_artist, al.title, s.duration_ms, s.lyrics
             FROM songs s JOIN albums al ON al.id = s.album_id WHERE s.id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| ApiError::not_found("song"))?;
    let mut stmt = conn.prepare(
        "SELECT a.name FROM song_artists sa JOIN artists a ON a.id = sa.artist_id
         WHERE sa.song_id = ?1 ORDER BY sa.position",
    )?;
    let artists: Vec<String> = stmt
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(SongInfo {
        path: row.0,
        title: row.1,
        display_artist: row.2,
        album: (!row.3.starts_with('[')).then_some(row.3),
        duration_s: (row.4 / 1000).max(0) as u32,
        embedded: row.5.filter(|t| !t.trim().is_empty()),
        artists,
    })
}

pub fn find(db: &Db, client: &LrclibClient, id: &str) -> Result<Option<Found>, ApiError> {
    let conn = db.conn()?;
    let info = song_info(&conn, id)?;
    let found = |text: String| Found {
        text,
        title: info.title.clone(),
        artist: info.display_artist.clone(),
    };

    let mut local: Vec<String> = Vec::new();
    let base = Path::new(&info.path).with_extension("");
    for ext in ["lrc", "txt"] {
        if let Ok(text) = std::fs::read_to_string(base.with_extension(ext))
            && !text.trim().is_empty()
        {
            local.push(text);
        }
    }
    local.extend(info.embedded.clone());
    if let Some(synced) = local.iter().find(|t| parse(t).0) {
        return Ok(Some(found(synced.clone())));
    }

    let mut fetched: Option<String> = None;
    match cached(&conn, id)? {
        Some(Cached {
            content: Some(text),
            ..
        }) => fetched = Some(text),
        Some(Cached {
            content: None,
            fetched_at,
        }) if now() - fetched_at < MISS_TTL_SECS => {}
        _ => {
            let artists: Vec<String> = if info.artists.is_empty() {
                vec![info.display_artist.clone()]
            } else {
                info.artists.clone()
            };
            let query = Query {
                title: &info.title,
                artists: &artists,
                album: info.album.as_deref(),
                duration: info.duration_s,
            };
            match client.find(&query) {
                Ok(hits) => {
                    let best = hits
                        .iter()
                        .find(|h| h.synced.is_some())
                        .or_else(|| hits.iter().find(|h| h.plain.is_some()));
                    let text = best.and_then(|h| h.synced.clone().or_else(|| h.plain.clone()));
                    store(&conn, id, text.as_deref(), "lrclib")?;
                    fetched = text;
                }
                Err(e) => tracing::warn!("lyrics lookup for {} failed: {e:#}", info.title),
            }
        }
    }

    if let Some(text) = fetched.filter(|t| parse(t).0) {
        return Ok(Some(found(text)));
    }
    Ok(local
        .into_iter()
        .next()
        .or(fetched_plain(&conn, id)?)
        .map(found))
}

fn fetched_plain(conn: &Connection, id: &str) -> Result<Option<String>, ApiError> {
    Ok(cached(conn, id)?.and_then(|c| c.content))
}

pub struct Line {
    pub start: Option<i64>,
    pub text: String,
}

fn parse_stamp(tag: &str) -> Option<i64> {
    let (m, rest) = tag.split_once(':')?;
    let minutes: i64 = m.trim().parse().ok()?;
    let (s, frac) = match rest.split_once(['.', ':']) {
        Some((s, f)) => (s, f),
        None => (rest, ""),
    };
    let seconds: i64 = s.trim().parse().ok()?;
    let millis = if frac.is_empty() {
        0
    } else {
        let digits: String = frac.chars().filter(char::is_ascii_digit).take(3).collect();
        let scale = 10_i64.pow(3 - digits.len() as u32);
        digits.parse::<i64>().ok()? * scale
    };
    Some(minutes * 60_000 + seconds * 1000 + millis)
}

pub fn parse(text: &str) -> (bool, Vec<Line>) {
    let mut lines = Vec::new();
    let mut offset = 0_i64;
    let mut synced = false;
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        let mut meta = false;
        while let Some(inner) = rest.strip_prefix('[') {
            let Some(end) = inner.find(']') else { break };
            let tag = &inner[..end];
            if let Some(ms) = parse_stamp(tag) {
                stamps.push(ms);
            } else if let Some(v) = tag.strip_prefix("offset:") {
                offset = v.trim().parse().unwrap_or(0);
                meta = true;
            } else if tag.contains(':') {
                meta = true;
            } else {
                break;
            }
            rest = inner[end + 1..].trim_start();
        }
        if stamps.is_empty() {
            if !(meta && rest.is_empty()) {
                lines.push(Line {
                    start: None,
                    text: rest.to_string(),
                });
            }
            continue;
        }
        synced = true;
        for ms in stamps {
            lines.push(Line {
                start: Some((ms - offset).max(0)),
                text: rest.to_string(),
            });
        }
    }
    if synced {
        lines.retain(|l| l.start.is_some());
        lines.sort_by_key(|l| l.start);
    }
    (synced, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: &str, secs: f64, synced: bool) -> Track {
        Track {
            id: None,
            track_name: Some(title.into()),
            artist_name: Some(artist.into()),
            album_name: None,
            duration: Some(secs),
            instrumental: Some(false),
            plain_lyrics: Some("la".into()),
            synced_lyrics: synced.then(|| "[00:01.00] la".into()),
        }
    }

    fn query(artists: &[String]) -> Query<'_> {
        Query {
            title: "Jaded",
            artists,
            album: None,
            duration: 263,
        }
    }

    #[test]
    fn prefers_synced_and_close_duration() {
        let artists = vec!["Spiritbox".to_string()];
        let picked = rank(
            &query(&artists),
            vec![
                track("Jaded", "Spiritbox", 263.0, false),
                track("Jaded", "Spiritbox", 265.0, true),
            ],
        );
        assert!(picked[0].synced_lyrics.is_some());
    }

    #[test]
    fn rejects_wrong_artist_and_far_duration() {
        let artists = vec!["Spiritbox".to_string()];
        let picked = rank(
            &query(&artists),
            vec![
                track("Jaded", "Someone Else", 263.0, true),
                track("Jaded", "Spiritbox", 300.0, true),
            ],
        );
        assert!(picked.is_empty());
    }

    #[test]
    fn decorations_and_feat_credits() {
        assert!(alike("Jaded (Remastered)", "Jaded"));
        assert!(alike("Jaded - Live", "jaded"));
        assert!(!alike("Jad", "Jaded Heart"));
        assert!(artists_alike("Luude Feat. Colin Hay", "Colin Hay"));
        assert!(!artists_alike("Luude Feat. Colin Hay", "Someone Else"));
    }

    #[test]
    fn parses_synced_lrc() {
        let (synced, lines) = parse("[ar:Someone]\n[00:12.50]hello\n[01:00.00][01:05.5]again\n");
        assert!(synced);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].start, Some(12_500));
        assert_eq!(lines[0].text, "hello");
        assert_eq!(lines[2].start, Some(65_500));
    }

    #[test]
    fn plain_lyrics_stay_unsynced() {
        let (synced, lines) = parse("one\ntwo\n");
        assert!(!synced);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].start, None);
    }
}
