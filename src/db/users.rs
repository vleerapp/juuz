use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Value, json};

use super::now;
use crate::error::ApiError;
use crate::ids;

#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    pub name: String,
    pub password_hash: String,
    pub token_secret: Option<Vec<u8>>,
    pub email: String,
    pub admin: bool,
    pub settings: bool,
    pub stream: bool,
    pub download: bool,
    pub upload: bool,
    pub playlist: bool,
    pub cover_art: bool,
    pub comment: bool,
    pub podcast: bool,
    pub share: bool,
    pub jukebox: bool,
    pub scrobbling: bool,
    pub max_bit_rate: i64,
}

#[derive(Debug, Clone, Default)]
pub struct RoleUpdate {
    pub admin: Option<bool>,
    pub settings: Option<bool>,
    pub stream: Option<bool>,
    pub download: Option<bool>,
    pub upload: Option<bool>,
    pub playlist: Option<bool>,
    pub cover_art: Option<bool>,
    pub comment: Option<bool>,
    pub podcast: Option<bool>,
    pub share: Option<bool>,
    pub jukebox: Option<bool>,
    pub scrobbling: Option<bool>,
    pub max_bit_rate: Option<i64>,
    pub email: Option<String>,
}

const COLS: &str = "id, name, password_hash, token_secret, email, admin_role, settings_role, \
    stream_role, download_role, upload_role, playlist_role, cover_art_role, comment_role, \
    podcast_role, share_role, jukebox_role, scrobbling_enabled, max_bit_rate";

fn map(r: &Row) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(0)?,
        name: r.get(1)?,
        password_hash: r.get(2)?,
        token_secret: r.get(3)?,
        email: r.get(4)?,
        admin: r.get(5)?,
        settings: r.get(6)?,
        stream: r.get(7)?,
        download: r.get(8)?,
        upload: r.get(9)?,
        playlist: r.get(10)?,
        cover_art: r.get(11)?,
        comment: r.get(12)?,
        podcast: r.get(13)?,
        share: r.get(14)?,
        jukebox: r.get(15)?,
        scrobbling: r.get(16)?,
        max_bit_rate: r.get(17)?,
    })
}

impl User {
    pub fn can_upload(&self) -> bool {
        self.admin || self.upload
    }

    pub fn to_json(&self, folders: &[i64]) -> Value {
        json!({
            "username": self.name,
            "email": self.email,
            "scrobblingEnabled": self.scrobbling,
            "maxBitRate": self.max_bit_rate,
            "adminRole": self.admin,
            "settingsRole": self.settings || self.admin,
            "downloadRole": self.download || self.admin,
            "uploadRole": self.upload || self.admin,
            "playlistRole": self.playlist || self.admin,
            "coverArtRole": self.cover_art || self.admin,
            "commentRole": self.comment || self.admin,
            "podcastRole": self.podcast || self.admin,
            "streamRole": self.stream || self.admin,
            "jukeboxRole": self.jukebox || self.admin,
            "shareRole": self.share || self.admin,
            "videoConversionRole": false,
            "folder": folders,
        })
    }
}

pub fn by_name(conn: &Connection, name: &str) -> rusqlite::Result<Option<User>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM users WHERE name = ?1"),
        [name],
        map,
    )
    .optional()
}

pub fn by_api_key(conn: &Connection, key_hash: &str) -> rusqlite::Result<Option<User>> {
    let user = conn
        .query_row(
            &format!(
                "SELECT {} FROM users WHERE id = (SELECT user_id FROM api_keys WHERE key_hash = ?1)",
                COLS
            ),
            [key_hash],
            map,
        )
        .optional()?;
    if user.is_some() {
        conn.execute(
            "UPDATE api_keys SET last_used_at = ?1 WHERE key_hash = ?2",
            params![now(), key_hash],
        )?;
    }
    Ok(user)
}

pub fn list(conn: &Connection) -> rusqlite::Result<Vec<User>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM users ORDER BY name"))?;
    stmt.query_map([], map)?.collect()
}

pub fn count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
}

pub fn create(
    conn: &Connection,
    name: &str,
    password_hash: &str,
    token_secret: Option<&[u8]>,
    admin: bool,
) -> Result<String, ApiError> {
    if name.trim().is_empty() {
        return Err(ApiError::bad_param("username"));
    }
    if by_name(conn, name)?.is_some() {
        return Err(ApiError::new(0, "username already exists"));
    }
    let id = ids::random();
    conn.execute(
        "INSERT INTO users(id, name, password_hash, token_secret, admin_role, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, name, password_hash, token_secret, admin, now()],
    )?;
    Ok(id)
}

pub fn set_password(
    conn: &Connection,
    id: &str,
    password_hash: &str,
    token_secret: Option<&[u8]>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET password_hash = ?1, token_secret = ?2 WHERE id = ?3",
        params![password_hash, token_secret, id],
    )?;
    Ok(())
}

pub fn rename(conn: &Connection, id: &str, name: &str) -> Result<bool, ApiError> {
    if name.trim().is_empty() {
        return Err(ApiError::bad_param("username"));
    }
    if let Some(other) = by_name(conn, name)?
        && other.id != id
    {
        return Ok(false);
    }
    match conn.execute(
        "UPDATE users SET name = ?1 WHERE id = ?2",
        params![name, id],
    ) {
        Ok(_) => Ok(true),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

pub fn set_token_secret(conn: &Connection, id: &str, secret: &[u8]) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE users SET token_secret = ?1 WHERE id = ?2",
        params![secret, id],
    )?;
    Ok(())
}

pub fn update_roles(conn: &Connection, id: &str, u: &RoleUpdate) -> rusqlite::Result<()> {
    let flags: [(&str, Option<bool>); 12] = [
        ("admin_role", u.admin),
        ("settings_role", u.settings),
        ("stream_role", u.stream),
        ("download_role", u.download),
        ("upload_role", u.upload),
        ("playlist_role", u.playlist),
        ("cover_art_role", u.cover_art),
        ("comment_role", u.comment),
        ("podcast_role", u.podcast),
        ("share_role", u.share),
        ("jukebox_role", u.jukebox),
        ("scrobbling_enabled", u.scrobbling),
    ];
    for (col, v) in flags {
        if let Some(v) = v {
            conn.execute(
                &format!("UPDATE users SET {col} = ?1 WHERE id = ?2"),
                params![v, id],
            )?;
        }
    }
    if let Some(v) = u.max_bit_rate {
        conn.execute(
            "UPDATE users SET max_bit_rate = ?1 WHERE id = ?2",
            params![v, id],
        )?;
    }
    if let Some(v) = &u.email {
        conn.execute("UPDATE users SET email = ?1 WHERE id = ?2", params![v, id])?;
    }
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM users WHERE id = ?1", [id])
}

pub fn create_api_key(
    conn: &Connection,
    user_id: &str,
    name: &str,
    hash: &str,
) -> rusqlite::Result<String> {
    let id = ids::random();
    conn.execute(
        "INSERT INTO api_keys(id, user_id, name, key_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, user_id, name, hash, now()],
    )?;
    Ok(id)
}

pub fn list_api_keys(
    conn: &Connection,
    user_id: &str,
) -> rusqlite::Result<Vec<(String, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, created_at FROM api_keys WHERE user_id = ?1 ORDER BY created_at",
    )?;
    stmt.query_map([user_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect()
}

pub fn delete_api_key(conn: &Connection, user_id: &str, id: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM api_keys WHERE id = ?1 AND user_id = ?2",
        params![id, user_id],
    )
}

pub fn lastfm_session(
    conn: &Connection,
    user_id: &str,
) -> rusqlite::Result<Option<(Vec<u8>, String)>> {
    let row: Option<(Option<Vec<u8>>, Option<String>)> = conn
        .query_row(
            "SELECT lastfm_session, lastfm_name FROM users WHERE id = ?1",
            [user_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(session, name)| Some((session?, name.unwrap_or_default()))))
}

pub fn set_lastfm(
    conn: &Connection,
    user_id: &str,
    session: Option<(&[u8], &str)>,
) -> rusqlite::Result<()> {
    let (blob, name) = match session {
        Some((b, n)) => (Some(b), Some(n)),
        None => (None, None),
    };
    conn.execute(
        "UPDATE users SET lastfm_session = ?2, lastfm_name = ?3 WHERE id = ?1",
        params![user_id, blob, name],
    )?;
    Ok(())
}

pub fn by_session(
    conn: &Connection,
    token_hash: &[u8],
    now: i64,
) -> rusqlite::Result<Option<User>> {
    conn.query_row(
        &format!(
            "SELECT {COLS} FROM users WHERE id = \
             (SELECT user_id FROM web_sessions WHERE token_hash = ?1 AND expires_at > ?2)"
        ),
        params![token_hash, now],
        map,
    )
    .optional()
}

pub fn create_session(
    conn: &Connection,
    token_hash: &[u8],
    user_id: &str,
    expires_at: i64,
) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM web_sessions WHERE expires_at <= ?1", [now()])?;
    conn.execute(
        "INSERT OR REPLACE INTO web_sessions(token_hash, user_id, expires_at) VALUES (?1, ?2, ?3)",
        params![token_hash, user_id, expires_at],
    )?;
    Ok(())
}

pub fn delete_session(conn: &Connection, token_hash: &[u8]) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM web_sessions WHERE token_hash = ?1",
        [token_hash],
    )
}

pub fn delete_sessions_except(
    conn: &Connection,
    user_id: &str,
    keep: &[u8],
) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM web_sessions WHERE user_id = ?1 AND token_hash <> ?2",
        params![user_id, keep],
    )
}

pub fn delete_sessions(conn: &Connection, user_id: &str) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM web_sessions WHERE user_id = ?1", [user_id])
}
