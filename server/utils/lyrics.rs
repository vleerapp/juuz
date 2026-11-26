use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;

use crate::server::utils::db::{Db, now};
use crate::server::utils::error::ApiError;

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
    for ext in ["lrc", "txt"] {
        if let Ok(text) = std::fs::read_to_string(Path::new(&info.path).with_extension(ext))
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

#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub start: i64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub start: Option<i64>,
    pub text: String,
    pub end: Option<i64>,
    pub words: Vec<Word>,
    pub secondary: Option<String>,
    pub gap: bool,
}

impl Line {
    fn plain(start: Option<i64>, text: String) -> Line {
        Line {
            start,
            text,
            end: None,
            words: Vec::new(),
            secondary: None,
            gap: false,
        }
    }

    fn at(&self) -> i64 {
        self.start.unwrap_or(0)
    }
}

const MIN_GAP: i64 = 4000;
const SUNG_BASE: i64 = 2000;
const SUNG_PER_CHAR: i64 = 120;
const INFERRED_GAP: i64 = 5000;

fn parse_stamp(tag: &str) -> Option<i64> {
    let (m, rest) = tag.split_once(':')?;
    let m = m.trim();
    if m.is_empty() || !m.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let minutes: i64 = m.parse().ok()?;
    let (s, frac) = match rest.split_once(['.', ':']) {
        Some((s, f)) => (s, f),
        None => (rest, ""),
    };
    let seconds: i64 = s.trim().parse().ok()?;
    let millis = if frac.is_empty() {
        0
    } else {
        if !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let digits: String = frac.chars().take(3).collect();
        let scale = 10_i64.pow(3 - digits.len() as u32);
        digits.parse::<i64>().ok()? * scale
    };
    Some(minutes * 60_000 + seconds * 1000 + millis)
}

struct Timed {
    text: String,
    words: Vec<Word>,
    end: Option<i64>,
}

fn parse_words(rest: &str, line_at: i64) -> Timed {
    let mut pieces: Vec<(Option<i64>, String)> = vec![(None, String::new())];
    let mut cursor = rest;
    while let Some(open) = cursor.find('<') {
        let (before, after) = cursor.split_at(open);
        let stamped = after
            .find('>')
            .and_then(|close| parse_stamp(&after[1..close]).map(|at| (at, close)));
        let Some(current) = pieces.last_mut() else {
            break;
        };
        current.1.push_str(before);
        match stamped {
            Some((at, close)) => {
                pieces.push((Some(at), String::new()));
                cursor = &after[close + 1..];
            }
            None => {
                current.1.push('<');
                cursor = &after[1..];
            }
        }
    }
    if let Some(current) = pieces.last_mut() {
        current.1.push_str(cursor);
    }
    if pieces.len() == 1 {
        return Timed {
            text: pieces.remove(0).1.trim().to_string(),
            words: Vec::new(),
            end: None,
        };
    }
    let last = pieces.len() - 1;
    let mut words: Vec<Word> = Vec::new();
    let mut end = None;
    for (i, (at, text)) in pieces.into_iter().enumerate() {
        let at = at.unwrap_or(line_at);
        if text.trim().is_empty() {
            if let Some(previous) = words.last_mut() {
                previous.text.push_str(&text);
            }
            if i == last {
                end = Some(at);
            }
            continue;
        }
        words.push(Word { start: at, text });
    }
    if let Some(first) = words.first_mut() {
        first.text = first.text.trim_start().to_string();
    }
    if let Some(final_word) = words.last_mut() {
        final_word.text = final_word.text.trim_end().to_string();
    }
    let text = words.iter().map(|w| w.text.as_str()).collect();
    Timed { text, words, end }
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
                lines.push(Line::plain(None, rest.to_string()));
            }
            continue;
        }
        synced = true;
        let single = stamps.len() == 1;
        let timed = parse_words(rest, stamps[0]);
        for ms in stamps {
            let mut line = Line::plain(Some(ms), timed.text.clone());
            if single {
                line.words = timed.words.clone();
                line.end = timed.end;
            }
            lines.push(line);
        }
    }
    if synced {
        let shift = |at: i64| (at - offset).max(0);
        lines.retain(|l| l.start.is_some());
        for line in &mut lines {
            line.start = line.start.map(shift);
            line.end = line.end.map(shift);
            for word in &mut line.words {
                word.start = shift(word.start);
            }
        }
        lines.sort_by_key(|l| l.start);
    }
    (synced, lines)
}

pub fn parse_rich(text: &str) -> (bool, Vec<Line>) {
    let (synced, lines) = parse(text);
    if !synced {
        return (false, lines);
    }
    let mut merged: Vec<Line> = Vec::with_capacity(lines.len());
    for line in lines {
        match merged.last_mut() {
            Some(last)
                if last.start == line.start && !last.text.is_empty() && !line.text.is_empty() =>
            {
                match &mut last.secondary {
                    Some(secondary) => {
                        secondary.push('\n');
                        secondary.push_str(&line.text);
                    }
                    None => last.secondary = Some(line.text),
                }
            }
            _ => merged.push(line),
        }
    }
    let mut cleaned: Vec<Line> = Vec::with_capacity(merged.len());
    for line in merged {
        let blank = line.text.is_empty();
        let prev_blank = cleaned.last().is_none_or(|l| l.text.is_empty());
        if blank && prev_blank {
            continue;
        }
        cleaned.push(line);
    }
    let mut out: Vec<Line> = Vec::with_capacity(cleaned.len());
    for (i, line) in cleaned.iter().enumerate() {
        out.push(line.clone());
        let Some(next) = cleaned.get(i + 1) else {
            continue;
        };
        if line.text.is_empty() || next.text.is_empty() {
            continue;
        }
        let ends = line
            .end
            .unwrap_or(line.at() + SUNG_BASE + line.text.chars().count() as i64 * SUNG_PER_CHAR);
        if next.at() - ends >= INFERRED_GAP {
            out.push(Line::plain(Some(ends), String::new()));
        }
    }
    if out.first().is_some_and(|l| l.at() >= MIN_GAP) {
        out.insert(0, Line::plain(Some(0), String::new()));
    }
    for i in 0..out.len() {
        out[i].gap = out[i].text.is_empty()
            && out
                .get(i + 1)
                .is_some_and(|next| next.at() - out[i].at() >= MIN_GAP);
    }
    (true, out)
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
    fn word_stamps_are_stripped_from_the_line_text() {
        let (synced, lines) =
            parse("[00:01.00]<00:01.00>Hel<00:01.40>lo <00:01.80>world<00:02.50>\n[00:05.00]next");
        assert!(synced);
        assert_eq!(lines[0].text, "Hello world");
        let words: Vec<(&str, i64)> = lines[0]
            .words
            .iter()
            .map(|w| (w.text.as_str(), w.start))
            .collect();
        assert_eq!(words, [("Hel", 1000), ("lo ", 1400), ("world", 1800)]);
        assert_eq!(lines[0].end, Some(2500));
        assert!(lines[1].words.is_empty());
    }

    #[test]
    fn offset_shifts_lines_and_words() {
        let (_, lines) = parse("[offset:+500]\n[00:02.00]<00:02.00>hel<00:02.40>lo");
        assert_eq!(lines[0].start, Some(1500));
        assert_eq!(lines[0].words[1].start, 1900);
    }

    #[test]
    fn rich_merges_same_stamp_lines_into_a_secondary() {
        let (_, lines) = parse_rich(
            "[00:01.00]いつか\n[00:01.00]itsuka\n[00:03.50]愛で\n[00:03.50]ai de\n[00:05.00]end",
        );
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].secondary.as_deref(), Some("itsuka"));
        assert_eq!(lines[1].secondary.as_deref(), Some("ai de"));
        let (_, flat) = parse(
            "[00:01.00]いつか\n[00:01.00]itsuka\n[00:03.50]愛で\n[00:03.50]ai de\n[00:05.00]end",
        );
        assert_eq!(flat.len(), 5);
    }

    #[test]
    fn rich_collapses_blanks_and_marks_gaps() {
        let (_, lines) = parse_rich("[00:01.00]\n[00:02.00]a\n[00:03.00]\n[00:04.00]\n[00:12.00]b");
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["a", "", "b"]);
        assert!(lines[1].gap);
        let (_, short) = parse_rich("[00:01.00]a\n[00:03.00]\n[00:05.00]b");
        assert!(!short[1].gap);
    }

    #[test]
    fn rich_adds_intro_and_inferred_gaps() {
        let (_, intro) = parse_rich("[00:10.00]a");
        assert_eq!(intro.len(), 2);
        assert!(intro[0].gap);
        let (_, silence) = parse_rich("[00:02.00]hello\n[00:40.00]world");
        assert_eq!(silence.len(), 3);
        assert!(silence[1].gap);
        let (_, close) = parse_rich("[00:02.00]hello there friend\n[00:09.00]world");
        assert_eq!(close.len(), 2);
    }

    #[test]
    fn plain_lyrics_stay_unsynced() {
        let (synced, lines) = parse("one\ntwo\n");
        assert!(!synced);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].start, None);
    }
}
