use std::collections::HashMap;

use rusqlite::Connection;

use crate::api::model::album_artist_map;
use crate::db::events::{self, Session};
use crate::db::now;
use crate::db::users::{self, User};
use crate::error::ApiError;

const DAY: i64 = 86_400;
const MAX_MONTHS: i64 = 24;
const TOP: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Week,
    Month,
    Year,
    All,
}

impl Range {
    pub const EACH: [Range; 4] = [Range::Week, Range::Month, Range::Year, Range::All];

    pub fn parse(code: Option<&str>) -> Range {
        Range::EACH
            .into_iter()
            .find(|r| Some(r.code()) == code)
            .unwrap_or(Range::Week)
    }

    pub fn code(self) -> &'static str {
        match self {
            Range::Week => "7d",
            Range::Month => "30d",
            Range::Year => "1y",
            Range::All => "all",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Range::Week => "7 days",
            Range::Month => "30 days",
            Range::Year => "year",
            Range::All => "all time",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Range::Week | Range::Month => "day",
            Range::Year | Range::All => "month",
        }
    }

    fn since(self) -> i64 {
        match self {
            Range::Week => now() - 7 * DAY,
            Range::Month => now() - 30 * DAY,
            Range::Year => now() - 365 * DAY,
            Range::All => 0,
        }
    }
}

#[derive(Clone, Default)]
pub struct Library {
    pub songs: i64,
    pub albums: i64,
    pub artists: i64,
    pub users: i64,
    pub duration_ms: i64,
    pub bytes: i64,
}

#[derive(Clone)]
pub struct Ranked {
    pub id: String,
    pub name: String,
    pub sub: Vec<(String, String)>,
    pub plays: i64,
    pub listened_ms: i64,
}

#[derive(Clone)]
pub struct Recent {
    pub title: String,
    pub artists: Vec<(String, String)>,
    pub user: String,
    pub played_at: i64,
}

#[derive(Clone)]
pub struct Day {
    pub label: String,
    pub plays: i64,
}

#[derive(Clone)]
pub struct Playing {
    pub title: String,
    pub artists: Vec<(String, String)>,
    pub user: String,
    pub player: String,
}

#[derive(Clone)]
pub struct Stats {
    pub library: Library,
    pub plays: i64,
    pub listened_ms: i64,
    pub completed: i64,
    pub skipped: i64,
    pub hours: Vec<Day>,
    pub songs: Vec<Ranked>,
    pub albums: Vec<Ranked>,
    pub artists: Vec<Ranked>,
    pub recent: Vec<Recent>,
    pub days: Vec<Day>,
}

fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn day_month(days: i64) -> String {
    let (_, m, d) = civil(days);
    format!("{d} {}", MONTHS[(m - 1) as usize])
}

pub fn date_label(ts: i64, tz: i64) -> String {
    let days = (ts + tz).div_euclid(DAY);
    let (y, _, _) = civil(days);
    let (now_y, _, _) = civil((now() + tz).div_euclid(DAY));
    if y == now_y {
        day_month(days)
    } else {
        format!("{} '{:02}", day_month(days), y.rem_euclid(100))
    }
}

fn library(c: &Connection, user: Option<&str>) -> rusqlite::Result<Library> {
    use crate::db::libraries::{artist_visible, visible};
    let (songs, duration_ms, bytes, albums, artists) = match user {
        Some(uid) => {
            let (songs, duration_ms, bytes) = c.query_row(
                concat!(
                    "SELECT COUNT(*), COALESCE(SUM(duration_ms), 0), COALESCE(SUM(size), 0) FROM songs WHERE ",
                    visible!("library_id")
                ),
                [uid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let albums = c.query_row(
                concat!("SELECT COUNT(*) FROM albums WHERE ", visible!("library_id")),
                [uid],
                |r| r.get(0),
            )?;
            let artists = c.query_row(
                concat!(
                    "SELECT COUNT(*) FROM artists ar WHERE ",
                    artist_visible!("ar.id")
                ),
                [uid],
                |r| r.get(0),
            )?;
            (songs, duration_ms, bytes, albums, artists)
        }
        None => {
            let (songs, duration_ms, bytes) = c.query_row(
                "SELECT COUNT(*), COALESCE(SUM(duration_ms), 0), COALESCE(SUM(size), 0) FROM songs",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            (
                songs,
                duration_ms,
                bytes,
                c.query_row("SELECT COUNT(*) FROM albums", [], |r| r.get(0))?,
                c.query_row("SELECT COUNT(*) FROM artists", [], |r| r.get(0))?,
            )
        }
    };
    Ok(Library {
        songs,
        duration_ms,
        bytes,
        albums,
        artists,
        users: users::count(c)?,
    })
}

fn month_index(days: i64) -> i64 {
    let (y, m, _) = civil(days);
    y * 12 + m - 1
}

fn month_label(index: i64) -> String {
    format!(
        "{} '{:02}",
        MONTHS[index.rem_euclid(12) as usize],
        index.div_euclid(12).rem_euclid(100)
    )
}

fn day_of(started_ms: i64, tz: i64) -> i64 {
    (started_ms / 1000 + tz).div_euclid(DAY)
}

fn daily(sessions: &[Session], tz: i64, count: i64) -> Vec<Day> {
    let today = (now() + tz).div_euclid(DAY);
    let first = today - (count - 1);
    let mut counts: HashMap<i64, i64> = HashMap::new();
    for s in sessions {
        *counts.entry(day_of(s.started, tz)).or_default() += 1;
    }
    (first..=today)
        .map(|d| Day {
            label: day_month(d),
            plays: counts.get(&d).copied().unwrap_or(0),
        })
        .collect()
}

fn monthly(sessions: &[Session], tz: i64, count: i64) -> Vec<Day> {
    let last = month_index((now() + tz).div_euclid(DAY));
    let mut counts: HashMap<i64, i64> = HashMap::new();
    for s in sessions {
        *counts
            .entry(month_index(day_of(s.started, tz)))
            .or_default() += 1;
    }
    (last - (count - 1)..=last)
        .map(|index| Day {
            plays: counts.get(&index).copied().unwrap_or(0),
            label: month_label(index),
        })
        .collect()
}

fn chart(sessions: &[Session], tz: i64, range: Range) -> Vec<Day> {
    match range {
        Range::Week => daily(sessions, tz, 7),
        Range::Month => daily(sessions, tz, 30),
        Range::Year => monthly(sessions, tz, 12),
        Range::All => {
            let this = month_index((now() + tz).div_euclid(DAY));
            let span = sessions
                .iter()
                .map(|s| month_index(day_of(s.started, tz)))
                .min()
                .map_or(1, |first| this - first + 1);
            monthly(sessions, tz, span.clamp(6, MAX_MONTHS))
        }
    }
}

fn hours(sessions: &[Session], tz: i64) -> Vec<Day> {
    let mut counts = [0i64; 24];
    for s in sessions {
        let hour = ((s.started / 1000 + tz).rem_euclid(DAY) / 3600) as usize;
        counts[hour] += 1;
    }
    counts
        .iter()
        .enumerate()
        .map(|(h, plays)| Day {
            label: format!("{h:02}:00"),
            plays: *plays,
        })
        .collect()
}

struct SongMeta {
    title: String,
    artist: String,
    album_id: String,
    album: String,
}

fn song_meta(c: &Connection, ids: &[String]) -> rusqlite::Result<HashMap<String, SongMeta>> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(200) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut stmt = c.prepare(&format!(
            "SELECT s.id, s.title, s.display_artist, al.id, al.title
             FROM songs s JOIN albums al ON al.id = s.album_id WHERE s.id IN ({marks})"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                SongMeta {
                    title: r.get(1)?,
                    artist: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    album_id: r.get(3)?,
                    album: r.get(4)?,
                },
            ))
        })?;
        for row in rows {
            let (id, meta) = row?;
            out.insert(id, meta);
        }
    }
    Ok(out)
}

fn song_artists(
    c: &Connection,
    ids: &[String],
) -> rusqlite::Result<HashMap<String, Vec<(String, String)>>> {
    let mut out: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for chunk in ids.chunks(200) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut stmt = c.prepare(&format!(
            "SELECT sa.song_id, a.id, a.name FROM song_artists sa
             JOIN artists a ON a.id = sa.artist_id WHERE sa.song_id IN ({marks})
             ORDER BY sa.position"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (song, id, name) = row?;
            out.entry(song).or_default().push((id, name));
        }
    }
    Ok(out)
}

#[derive(Default)]
struct Tally {
    id: String,
    name: String,
    sub: Vec<(String, String)>,
    plays: i64,
    listened_ms: i64,
    last: i64,
}

fn top(tallies: HashMap<String, Tally>) -> Vec<Ranked> {
    let mut list: Vec<Tally> = tallies.into_values().collect();
    list.sort_by(|a, b| {
        b.plays
            .cmp(&a.plays)
            .then(b.listened_ms.cmp(&a.listened_ms))
            .then(b.last.cmp(&a.last))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    list.into_iter()
        .take(TOP)
        .map(|t| Ranked {
            id: t.id,
            name: t.name,
            sub: t.sub,
            plays: t.plays,
            listened_ms: t.listened_ms,
        })
        .collect()
}

fn credited(artists: Option<&Vec<(String, String)>>, display: &str) -> Vec<(String, String)> {
    match artists {
        Some(list) if !list.is_empty() => list.clone(),
        _ if display.is_empty() => Vec::new(),
        _ => vec![(String::new(), display.to_string())],
    }
}

fn bump(
    map: &mut HashMap<String, Tally>,
    key: &str,
    name: &str,
    sub: Vec<(String, String)>,
    s: &Session,
) {
    let t = map.entry(key.to_string()).or_default();
    if t.name.is_empty() {
        t.id = key.to_string();
        t.name = name.to_string();
        t.sub = sub;
    }
    t.plays += 1;
    t.listened_ms += s.listened_ms;
    t.last = t.last.max(s.started);
}

pub fn stats(c: &Connection, user: Option<&str>, range: Range, tz: i64) -> Result<Stats, ApiError> {
    let sessions = events::sessions(c, user, range.since() * 1000)?;
    let plays = sessions.len() as i64;
    let listened_ms: i64 = sessions.iter().map(|s| s.listened_ms).sum();
    let completed = sessions.iter().filter(|s| s.completed).count() as i64;
    let skipped = sessions.iter().filter(|s| s.skipped).count() as i64;

    let mut ids: Vec<String> = sessions.iter().map(|s| s.song_id.clone()).collect();
    ids.sort();
    ids.dedup();
    let meta = song_meta(c, &ids)?;
    let artists_of = song_artists(c, &ids)?;
    let mut album_ids: Vec<String> = meta.values().map(|m| m.album_id.clone()).collect();
    album_ids.sort();
    album_ids.dedup();
    let album_credits = album_artist_map(c, &album_ids)?;

    let mut songs: HashMap<String, Tally> = HashMap::new();
    let mut albums: HashMap<String, Tally> = HashMap::new();
    let mut artists: HashMap<String, Tally> = HashMap::new();
    for s in &sessions {
        let Some(m) = meta.get(&s.song_id) else {
            continue;
        };
        bump(
            &mut songs,
            &s.song_id,
            &m.title,
            credited(artists_of.get(&s.song_id), &m.artist),
            s,
        );
        bump(
            &mut albums,
            &m.album_id,
            &m.album,
            credited(album_credits.get(&m.album_id), ""),
            s,
        );
        for (id, name) in artists_of.get(&s.song_id).into_iter().flatten() {
            bump(&mut artists, id, name, Vec::new(), s);
        }
    }

    let mut user_names: HashMap<String, String> = HashMap::new();
    let mut stmt = c.prepare("SELECT id, name FROM users")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (id, name) = row?;
        user_names.insert(id, name);
    }
    let recent = sessions
        .iter()
        .rev()
        .filter_map(|s| {
            let m = meta.get(&s.song_id)?;
            Some(Recent {
                title: m.title.clone(),
                artists: credited(artists_of.get(&s.song_id), &m.artist),
                user: user_names.get(&s.user_id).cloned().unwrap_or_default(),
                played_at: s.started / 1000,
            })
        })
        .take(TOP)
        .collect();

    Ok(Stats {
        library: library(c, user)?,
        plays,
        listened_ms,
        completed,
        skipped,
        hours: hours(&sessions, tz),
        songs: top(songs),
        albums: top(albums),
        artists: top(artists),
        recent,
        days: chart(&sessions, tz, range),
    })
}

pub fn playing(c: &Connection, entries: Vec<(String, String, String)>) -> Vec<Playing> {
    entries
        .into_iter()
        .filter_map(|(song_id, user, player)| {
            let (title, display): (String, Option<String>) = c
                .query_row(
                    "SELECT title, display_artist FROM songs WHERE id = ?1",
                    [&song_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .ok()?;
            let artists_of = song_artists(c, std::slice::from_ref(&song_id)).ok()?;
            Some(Playing {
                title,
                artists: credited(artists_of.get(&song_id), &display.unwrap_or_default()),
                user,
                player,
            })
        })
        .collect()
}

#[derive(Clone)]
pub struct Key {
    pub id: String,
    pub name: String,
    pub created: i64,
}

#[derive(Clone)]
pub struct Account {
    pub keys: Vec<Key>,
    pub lastfm: Option<String>,
}

pub fn account(c: &Connection, user: &User) -> Result<Account, ApiError> {
    Ok(Account {
        keys: users::list_api_keys(c, &user.id)?
            .into_iter()
            .map(|(id, name, created)| Key { id, name, created })
            .collect(),
        lastfm: users::lastfm_session(c, &user.id)?.map(|(_, name)| name),
    })
}

#[derive(Clone)]
pub struct Member {
    pub user: User,
    pub plays: i64,
    pub granted: Vec<i64>,
}

#[derive(Clone)]
pub struct Admin {
    pub members: Vec<Member>,
    pub libraries: Vec<crate::db::libraries::Library>,
}

pub fn admin(c: &Connection) -> Result<Admin, ApiError> {
    let mut stmt = c.prepare("SELECT user_id, COUNT(*) FROM scrobbles GROUP BY user_id")?;
    let plays: HashMap<String, i64> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut members = Vec::new();
    for user in users::list(c)? {
        members.push(Member {
            plays: plays.get(&user.id).copied().unwrap_or(0),
            granted: crate::db::libraries::granted(c, &user.id)?,
            user,
        });
    }
    Ok(Admin {
        members,
        libraries: crate::db::libraries::list(c)?,
    })
}
