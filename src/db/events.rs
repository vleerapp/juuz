use std::collections::HashMap;

use rusqlite::{Connection, params};

use crate::ids;

pub const KINDS: [&str; 5] = ["PLAY", "STOP", "PAUSE", "RESUME", "SEEK"];

const SKIP_BELOW_MS: i64 = 30_000;
const END_SLACK_MS: i64 = 5_000;
const FALLBACK_CAP_MS: i64 = 600_000;

pub fn insert(
    conn: &Connection,
    user_id: &str,
    kind: &str,
    song_id: &str,
    playlist_id: Option<&str>,
    position: Option<i64>,
    at_ms: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO play_events(id, user_id, event_type, song_id, playlist_id, position, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, STRFTIME('%Y-%m-%d %H:%M:%f', ?7 / 1000.0, 'unixepoch'))",
        params![ids::random(), user_id, kind, song_id, playlist_id, position, at_ms],
    )?;
    Ok(())
}

pub fn insert_listen(
    conn: &Connection,
    user_id: &str,
    song_id: &str,
    at_ms: i64,
) -> rusqlite::Result<()> {
    let duration: i64 = conn
        .query_row(
            "SELECT duration_ms FROM songs WHERE id = ?1",
            [song_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    insert(conn, user_id, "PLAY", song_id, None, Some(0), at_ms)?;
    insert(
        conn,
        user_id,
        "STOP",
        song_id,
        None,
        Some(duration),
        at_ms + duration,
    )
}

#[derive(Clone, Debug)]
pub struct Session {
    pub user_id: String,
    pub song_id: String,
    pub started: i64,
    pub listened_ms: i64,
    pub duration_ms: i64,
    pub completed: bool,
    pub skipped: bool,
}

struct Open {
    song_id: String,
    started: i64,
    last: i64,
    playing: bool,
    listened: i64,
    duration: i64,
}

impl Open {
    fn advance(&mut self, at: i64) {
        if self.playing {
            let cap = if self.duration > 0 {
                self.duration
            } else {
                FALLBACK_CAP_MS
            };
            self.listened += (at - self.last).clamp(0, cap);
        }
        self.last = at;
    }

    fn close(mut self, user: &str, at: i64, position: Option<i64>, out: &mut Vec<Session>) {
        self.advance(at);
        let reached_end =
            position.is_some_and(|p| self.duration > 0 && p + END_SLACK_MS >= self.duration);
        let mostly = self.duration > 0 && self.listened * 10 >= self.duration * 9;
        let completed = reached_end || mostly;
        out.push(Session {
            user_id: user.to_string(),
            song_id: self.song_id,
            started: self.started,
            listened_ms: self.listened,
            duration_ms: self.duration,
            completed,
            skipped: !completed && self.listened < SKIP_BELOW_MS,
        });
    }
}

pub fn sessions(
    conn: &Connection,
    user: Option<&str>,
    since_ms: i64,
) -> rusqlite::Result<Vec<Session>> {
    let mut stmt = conn.prepare(
        "SELECT e.user_id, e.event_type, e.song_id, e.position,
                CAST(ROUND((julianday(e.timestamp) - 2440587.5) * 86400000) AS INTEGER),
                COALESCE(s.duration_ms, 0)
         FROM play_events e LEFT JOIN songs s ON s.id = e.song_id
         WHERE (?1 IS NULL OR e.user_id = ?1)
           AND e.timestamp >= STRFTIME('%Y-%m-%d %H:%M:%f', ?2 / 1000.0, 'unixepoch')
         ORDER BY e.user_id, e.timestamp, e.rowid",
    )?;
    let rows = stmt.query_map(params![user, since_ms], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
        ))
    })?;

    let mut out = Vec::new();
    let mut open: HashMap<String, Open> = HashMap::new();
    for row in rows {
        let (uid, kind, song, position, at, duration) = row?;
        if kind == "PLAY" {
            if let Some(prev) = open.remove(&uid) {
                prev.close(&uid, at, None, &mut out);
            }
            open.insert(
                uid,
                Open {
                    song_id: song,
                    started: at,
                    last: at,
                    playing: true,
                    listened: 0,
                    duration,
                },
            );
            continue;
        }
        let Some(current) = open.get_mut(&uid) else {
            continue;
        };
        if current.song_id != song {
            continue;
        }
        match kind.as_str() {
            "PAUSE" => {
                current.advance(at);
                current.playing = false;
            }
            "RESUME" => {
                current.advance(at);
                current.playing = true;
            }
            "SEEK" => current.advance(at),
            "STOP" => {
                if let Some(done) = open.remove(&uid) {
                    done.close(&uid, at, position, &mut out);
                }
            }
            _ => {}
        }
    }
    for (uid, rest) in open {
        let at = rest.last;
        rest.close(&uid, at, None, &mut out);
    }
    out.sort_by_key(|s| s.started);
    Ok(out)
}
