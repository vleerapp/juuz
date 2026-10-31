use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::now;
use crate::error::ApiError;

pub const DEFAULT_ID: i64 = 1;

#[derive(Debug, Clone)]
pub struct Library {
    pub id: i64,
    pub name: String,
    pub path: PathBuf,
    pub writable: bool,
    pub scan_interval: i64,
}

impl Library {
    pub fn online(&self) -> bool {
        self.path.is_dir()
    }
}

const COLS: &str = "id, name, path, writable, scan_interval";

fn map(r: &Row) -> rusqlite::Result<Library> {
    Ok(Library {
        id: r.get(0)?,
        name: r.get(1)?,
        path: PathBuf::from(r.get::<_, String>(2)?),
        writable: r.get(3)?,
        scan_interval: r.get(4)?,
    })
}

pub fn adopt_default(conn: &Connection, music_dir: &Path) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE libraries SET path = ?1 WHERE id = ?2",
        params![music_dir.to_string_lossy(), DEFAULT_ID],
    )?;
    Ok(())
}

pub fn list(conn: &Connection) -> rusqlite::Result<Vec<Library>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM libraries ORDER BY id"))?;
    stmt.query_map([], map)?.collect()
}

pub fn get(conn: &Connection, id: i64) -> rusqlite::Result<Option<Library>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM libraries WHERE id = ?1"),
        [id],
        map,
    )
    .optional()
}

pub fn roots(conn: &Connection) -> rusqlite::Result<Vec<PathBuf>> {
    let mut roots: Vec<PathBuf> = list(conn)?
        .into_iter()
        .map(|l| l.path)
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    roots.sort_by_key(|p| std::cmp::Reverse(p.as_os_str().len()));
    Ok(roots)
}

pub fn containing(conn: &Connection, path: &Path) -> rusqlite::Result<Option<Library>> {
    Ok(list(conn)?
        .into_iter()
        .filter(|l| !l.path.as_os_str().is_empty() && path.starts_with(&l.path))
        .max_by_key(|l| l.path.as_os_str().len()))
}

pub fn visible_to(conn: &Connection, user_id: &str) -> rusqlite::Result<Vec<Library>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM libraries l WHERE EXISTS (SELECT 1 FROM users u WHERE u.id = ?1 AND u.admin_role = 1)
            OR l.id IN (SELECT library_id FROM user_libraries WHERE user_id = ?1) ORDER BY l.id"
    ))?;
    stmt.query_map([user_id], map)?.collect()
}

pub fn can_access(conn: &Connection, user_id: &str, library_id: i64) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM users u WHERE u.id = ?1 AND u.admin_role = 1)
            OR EXISTS (SELECT 1 FROM user_libraries WHERE user_id = ?1 AND library_id = ?2)",
        params![user_id, library_id],
        |r| r.get(0),
    )
}

pub fn song_library(conn: &Connection, song_id: &str) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT library_id FROM songs WHERE id = ?1",
        [song_id],
        |r| r.get(0),
    )
    .optional()
}

pub fn can_access_song(conn: &Connection, user_id: &str, song_id: &str) -> rusqlite::Result<bool> {
    match song_library(conn, song_id)? {
        Some(lib) => can_access(conn, user_id, lib),
        None => Ok(false),
    }
}

pub fn can_access_album(
    conn: &Connection,
    user_id: &str,
    album_id: &str,
) -> rusqlite::Result<bool> {
    let lib: Option<i64> = conn
        .query_row(
            "SELECT library_id FROM albums WHERE id = ?1",
            [album_id],
            |r| r.get(0),
        )
        .optional()?;
    match lib {
        Some(lib) => can_access(conn, user_id, lib),
        None => Ok(false),
    }
}

pub fn can_access_artist(
    conn: &Connection,
    user_id: &str,
    artist_id: &str,
) -> rusqlite::Result<bool> {
    conn.query_row(
        concat!(
            "SELECT EXISTS (SELECT 1 FROM artists ar WHERE ar.id = ?2 AND ",
            artist_visible!("ar.id"),
            ")"
        ),
        params![user_id, artist_id],
        |r| r.get(0),
    )
}

#[derive(Debug)]
pub enum Invalid {
    Name,
    Path,
    Overlap,
    Exists,
}

pub fn create(conn: &Connection, name: &str, path: &Path, writable: bool) -> Result<i64, Invalid> {
    let name = name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(Invalid::Name);
    }
    if !path.is_absolute() || !path.is_dir() {
        return Err(Invalid::Path);
    }
    let path = std::fs::canonicalize(path).map_err(|_| Invalid::Path)?;
    let existing = list(conn).map_err(|_| Invalid::Path)?;
    if existing.iter().any(|l| l.name.eq_ignore_ascii_case(name)) {
        return Err(Invalid::Exists);
    }
    if existing.iter().any(|l| {
        !l.path.as_os_str().is_empty() && (path.starts_with(&l.path) || l.path.starts_with(&path))
    }) {
        return Err(Invalid::Overlap);
    }
    conn.execute(
        "INSERT INTO libraries(name, path, writable, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![name, path.to_string_lossy(), writable, now()],
    )
    .map_err(|_| Invalid::Exists)?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO user_libraries(user_id, library_id) SELECT id, ?1 FROM users",
        [id],
    )
    .map_err(|_| Invalid::Exists)?;
    Ok(id)
}

pub fn update(
    conn: &Connection,
    id: i64,
    name: &str,
    writable: bool,
    scan_interval: i64,
) -> Result<bool, Invalid> {
    let name = name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(Invalid::Name);
    }
    match conn.execute(
        "UPDATE libraries SET name = ?2, writable = ?3, scan_interval = ?4 WHERE id = ?1",
        params![id, name, writable, scan_interval.max(0)],
    ) {
        Ok(n) => Ok(n > 0),
        Err(_) => Err(Invalid::Exists),
    }
}

pub fn delete(conn: &Connection, id: i64) -> Result<bool, ApiError> {
    if id == DEFAULT_ID {
        return Err(ApiError::forbidden("the default library cannot be removed"));
    }
    let ids: Vec<String> = {
        let mut stmt = conn.prepare("SELECT id FROM songs WHERE library_id = ?1")?;
        stmt.query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for song in &ids {
        crate::scanner::index::fts_del(conn, song)?;
    }
    conn.execute("DELETE FROM songs WHERE library_id = ?1", [id])?;
    conn.execute("DELETE FROM albums WHERE library_id = ?1", [id])?;
    conn.execute(
        "UPDATE users SET upload_library_id = NULL WHERE upload_library_id = ?1",
        [id],
    )?;
    Ok(conn.execute("DELETE FROM libraries WHERE id = ?1", [id])? > 0)
}

pub fn granted(conn: &Connection, user_id: &str) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn
        .prepare("SELECT library_id FROM user_libraries WHERE user_id = ?1 ORDER BY library_id")?;
    stmt.query_map([user_id], |r| r.get(0))?.collect()
}

pub fn set_access(conn: &Connection, user_id: &str, library_ids: &[i64]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM user_libraries WHERE user_id = ?1", [user_id])?;
    for id in library_ids {
        conn.execute(
            "INSERT OR IGNORE INTO user_libraries(user_id, library_id)
             SELECT ?1, id FROM libraries WHERE id = ?2",
            params![user_id, id],
        )?;
    }
    conn.execute(
        "UPDATE users SET upload_library_id = NULL WHERE id = ?1
           AND upload_library_id NOT IN (SELECT library_id FROM user_libraries WHERE user_id = ?1)",
        [user_id],
    )?;
    Ok(())
}

macro_rules! visible {
    ($col:expr) => {
        concat!(
            "(",
            $col,
            " IN (SELECT library_id FROM user_libraries WHERE user_id = ?1)
              OR EXISTS (SELECT 1 FROM users vu WHERE vu.id = ?1 AND vu.admin_role = 1))"
        )
    };
}

macro_rules! artist_visible {
    ($col:expr) => {
        concat!(
            "(EXISTS (SELECT 1 FROM album_artists vaa JOIN albums val ON val.id = vaa.album_id
                      WHERE vaa.artist_id = ",
            $col,
            " AND ",
            visible!("val.library_id"),
            ") OR EXISTS (SELECT 1 FROM song_artists vsa JOIN songs vs ON vs.id = vsa.song_id
                      WHERE vsa.artist_id = ",
            $col,
            " AND ",
            visible!("vs.library_id"),
            "))"
        )
    };
}

pub(crate) use artist_visible;
pub(crate) use visible;
