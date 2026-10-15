pub mod users;

use std::path::Path;

use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};

use crate::error::ApiError;

#[derive(Clone)]
pub struct Db {
    pool: Pool<SqliteConnectionManager>,
}

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(include_str!("../../migrations/001_init.sql"))])
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let manager = SqliteConnectionManager::file(path).with_init(|c| {
            c.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = ON;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA busy_timeout = 15000;",
            )
        });
        let pool = Pool::builder()
            .max_size(16)
            .build(manager)
            .context("opening database pool")?;
        let mut conn = pool.get()?;
        migrations()
            .to_latest(&mut conn)
            .context("running migrations")?;
        Ok(Db { pool })
    }

    pub fn conn(&self) -> Result<r2d2::PooledConnection<SqliteConnectionManager>, ApiError> {
        self.pool.get().map_err(|e| {
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
