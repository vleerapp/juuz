pub mod settings;
pub mod users;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::error::ApiError;

const MAX_CONNECTIONS: usize = 16;
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(rust_embed::RustEmbed)]
#[folder = "migrations"]
#[include = "*.sql"]
struct MigrationFiles;

struct Shared {
    path: PathBuf,
    music_dir: PathBuf,
    state: Mutex<PoolState>,
    available: Condvar,
}

fn lock(m: &Mutex<PoolState>) -> MutexGuard<'_, PoolState> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct PoolState {
    idle: Vec<Connection>,
    open: usize,
}

#[derive(Clone)]
pub struct Db {
    shared: Arc<Shared>,
}

pub struct Conn {
    conn: Option<Connection>,
    shared: Arc<Shared>,
}

impl Deref for Conn {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("connection present until drop")
    }
}

impl DerefMut for Conn {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("connection present until drop")
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            lock(&self.shared.state).idle.push(conn);
            self.shared.available.notify_one();
        }
    }
}

fn open_connection(path: &Path, music_dir: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    let root = music_dir.to_path_buf();
    conn.create_scalar_function(
        "music_rel",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        move |ctx| {
            let path = ctx.get::<String>(0)?;
            Ok(Path::new(&path)
                .strip_prefix(&root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(path))
        },
    )?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 15000;",
    )?;
    Ok(conn)
}

fn run_migrations(conn: &mut Connection) -> Result<()> {
    let mut files: Vec<(String, String)> = MigrationFiles::iter()
        .filter_map(|name| {
            let file = MigrationFiles::get(&name)?;
            let text = std::str::from_utf8(file.data.as_ref()).ok()?.to_owned();
            Some((name.into_owned(), text))
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    if files.is_empty() {
        bail!("no migrations found");
    }

    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current as usize > files.len() {
        bail!(
            "database schema version {current} is newer than known migrations ({})",
            files.len()
        );
    }
    let tx = conn.transaction()?;
    for (i, (name, sql)) in files.iter().enumerate().skip(current as usize) {
        tx.execute_batch(sql)
            .map_err(|e| anyhow::anyhow!("migration {name} failed: {e}"))?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
    }
    tx.commit()?;
    Ok(())
}

impl Db {
    pub fn open(path: &Path, music_dir: PathBuf) -> Result<Db> {
        let mut first = open_connection(path, &music_dir).context("opening database")?;
        run_migrations(&mut first).context("running migrations")?;
        Ok(Db {
            shared: Arc::new(Shared {
                path: path.to_path_buf(),
                music_dir,
                state: Mutex::new(PoolState {
                    idle: vec![first],
                    open: 1,
                }),
                available: Condvar::new(),
            }),
        })
    }

    fn acquire(&self) -> Result<Conn> {
        let shared = &self.shared;
        let mut state = lock(&shared.state);
        loop {
            if let Some(conn) = state.idle.pop() {
                return Ok(Conn {
                    conn: Some(conn),
                    shared: shared.clone(),
                });
            }
            if state.open < MAX_CONNECTIONS {
                state.open += 1;
                drop(state);
                return match open_connection(&shared.path, &shared.music_dir) {
                    Ok(conn) => Ok(Conn {
                        conn: Some(conn),
                        shared: shared.clone(),
                    }),
                    Err(e) => {
                        lock(&shared.state).open -= 1;
                        shared.available.notify_one();
                        Err(e)
                    }
                };
            }
            let (guard, res) = shared
                .available
                .wait_timeout(state, ACQUIRE_TIMEOUT)
                .unwrap_or_else(|e| e.into_inner());
            state = guard;
            if res.timed_out() && state.idle.is_empty() {
                bail!("timed out waiting for a database connection");
            }
        }
    }

    pub fn conn(&self) -> Result<Conn, ApiError> {
        self.acquire().map_err(|e| {
            tracing::error!("pool error: {e}");
            ApiError::generic("database unavailable")
        })
    }

    pub async fn run<T, F>(&self, f: F) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, ApiError> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.conn()?;
            f(&mut conn)
        })
        .await
        .map_err(|e| {
            tracing::error!("blocking task failed: {e}");
            ApiError::generic("internal server error")
        })?
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
