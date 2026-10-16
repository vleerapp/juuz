use std::collections::HashMap;

use rusqlite::{Connection, params};

use crate::db::now;
use crate::db::users::{self, User};
use crate::error::ApiError;

const DAY: i64 = 86_400;
const CHART_DAYS: i64 = 14;
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
            .unwrap_or(Range::Month)
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
    pub name: String,
    pub sub: String,
    pub plays: i64,
}

#[derive(Clone)]
pub struct Recent {
    pub title: String,
    pub artist: String,
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
    pub artist: String,
    pub user: String,
    pub player: String,
}

#[derive(Clone)]
pub struct Stats {
    pub library: Library,
    pub plays: i64,
    pub listened_ms: i64,
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

pub fn date_label(ts: i64) -> String {
    let (y, m, d) = civil(ts.div_euclid(DAY));
    format!("{y:04}-{m:02}-{d:02}")
}

fn ranked(
    c: &Connection,
    sql: &str,
    user: Option<&str>,
    since: i64,
) -> rusqlite::Result<Vec<Ranked>> {
    let mut stmt = c.prepare(sql)?;
    stmt.query_map(params![user, since, TOP as i64], |r| {
        Ok(Ranked {
            name: r.get(0)?,
            sub: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            plays: r.get(2)?,
        })
    })?
    .collect()
}

const SCOPE: &str = "(?1 IS NULL OR sc.user_id = ?1) AND sc.played_at >= ?2";

fn library(c: &Connection) -> rusqlite::Result<Library> {
    let (songs, duration_ms, bytes) = c.query_row(
        "SELECT COUNT(*), COALESCE(SUM(duration_ms), 0), COALESCE(SUM(size), 0) FROM songs",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(Library {
        songs,
        duration_ms,
        bytes,
        albums: c.query_row("SELECT COUNT(*) FROM albums", [], |r| r.get(0))?,
        artists: c.query_row("SELECT COUNT(*) FROM artists", [], |r| r.get(0))?,
        users: users::count(c)?,
    })
}

fn chart(c: &Connection, user: Option<&str>, tz: i64) -> rusqlite::Result<Vec<Day>> {
    let today = (now() + tz).div_euclid(DAY);
    let first = today - (CHART_DAYS - 1);
    let mut stmt = c.prepare(
        "SELECT (played_at + ?3) / 86400, COUNT(*) FROM scrobbles
         WHERE (?1 IS NULL OR user_id = ?1) AND played_at >= ?2 GROUP BY 1",
    )?;
    let counts: HashMap<i64, i64> = stmt
        .query_map(params![user, first * DAY - tz, tz], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok((first..=today)
        .map(|d| Day {
            label: date_label(d * DAY)[5..].to_string(),
            plays: counts.get(&d).copied().unwrap_or(0),
        })
        .collect())
}

pub fn stats(c: &Connection, user: Option<&str>, range: Range, tz: i64) -> Result<Stats, ApiError> {
    let since = range.since();
    let (plays, listened_ms) = c.query_row(
        &format!(
            "SELECT COUNT(*), COALESCE(SUM(s.duration_ms), 0) FROM scrobbles sc
             JOIN songs s ON s.id = sc.song_id WHERE {SCOPE}"
        ),
        params![user, since],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let songs = ranked(
        c,
        &format!(
            "SELECT s.title, s.display_artist, COUNT(*) AS n FROM scrobbles sc
             JOIN songs s ON s.id = sc.song_id WHERE {SCOPE}
             GROUP BY s.id ORDER BY n DESC, MAX(sc.played_at) DESC LIMIT ?3"
        ),
        user,
        since,
    )?;
    let albums = ranked(
        c,
        &format!(
            "SELECT al.title,
                    (SELECT a.name FROM album_artists aa JOIN artists a ON a.id = aa.artist_id
                       WHERE aa.album_id = al.id ORDER BY aa.position LIMIT 1),
                    COUNT(*) AS n
             FROM scrobbles sc JOIN songs s ON s.id = sc.song_id
             JOIN albums al ON al.id = s.album_id WHERE {SCOPE}
             GROUP BY al.id ORDER BY n DESC, MAX(sc.played_at) DESC LIMIT ?3"
        ),
        user,
        since,
    )?;
    let artists = ranked(
        c,
        &format!(
            "SELECT a.name, NULL, COUNT(*) AS n FROM scrobbles sc
             JOIN song_artists sa ON sa.song_id = sc.song_id
             JOIN artists a ON a.id = sa.artist_id WHERE {SCOPE}
             GROUP BY a.id ORDER BY n DESC, a.name LIMIT ?3"
        ),
        user,
        since,
    )?;
    let mut stmt = c.prepare(
        "SELECT s.title, s.display_artist, u.name, sc.played_at FROM scrobbles sc
         JOIN songs s ON s.id = sc.song_id JOIN users u ON u.id = sc.user_id
         WHERE (?1 IS NULL OR sc.user_id = ?1)
         ORDER BY sc.played_at DESC, sc.id DESC LIMIT ?2",
    )?;
    let recent = stmt
        .query_map(params![user, TOP as i64], |r| {
            Ok(Recent {
                title: r.get(0)?,
                artist: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                user: r.get(2)?,
                played_at: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Stats {
        library: library(c)?,
        plays,
        listened_ms,
        songs,
        albums,
        artists,
        recent,
        days: chart(c, user, tz)?,
    })
}

pub fn playing(c: &Connection, entries: Vec<(String, String, String)>) -> Vec<Playing> {
    entries
        .into_iter()
        .filter_map(|(song_id, user, player)| {
            c.query_row(
                "SELECT title, display_artist FROM songs WHERE id = ?1",
                [&song_id],
                |r| {
                    Ok(Playing {
                        title: r.get(0)?,
                        artist: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        user: user.clone(),
                        player: player.clone(),
                    })
                },
            )
            .ok()
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
}

#[derive(Clone)]
pub struct Admin {
    pub members: Vec<Member>,
}

pub fn admin(c: &Connection) -> Result<Admin, ApiError> {
    let mut stmt = c.prepare("SELECT user_id, COUNT(*) FROM scrobbles GROUP BY user_id")?;
    let plays: HashMap<String, i64> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let members = users::list(c)?
        .into_iter()
        .map(|user| Member {
            plays: plays.get(&user.id).copied().unwrap_or(0),
            user,
        })
        .collect();
    Ok(Admin { members })
}
