pub mod fingerprint;
pub mod grouping;
pub mod index;
pub mod tags;
pub mod watch;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use rusqlite::Connection;
use walkdir::WalkDir;

use crate::db::{Db, now, now_ms};
use crate::error::ApiError;
use crate::live::Live;
use index::FolderCtx;
use tags::{TrackInfo, is_audio, read_track};

const CHUNK: usize = 256;

#[derive(Clone)]
pub struct Scanner {
    db: Db,
    running: Arc<AtomicBool>,
    count: Arc<AtomicU64>,
    index_lock: Arc<Mutex<()>>,
    live: Live,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub scanning: bool,
    pub count: u64,
    pub folder_count: i64,
    pub last_scan: Option<i64>,
}

#[derive(Debug, Default)]
pub struct Summary {
    pub scanned: u64,
    pub pruned: usize,
    pub failed: usize,
}

struct Walked {
    path: PathBuf,
    size: i64,
    mtime: i64,
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|n| n.starts_with('.'))
}

fn walk(root: &Path) -> Vec<Walked> {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_hidden(e.file_name()))
        .filter_map(|e| match e {
            Ok(e) => Some(e),
            Err(err) => {
                tracing::warn!("walk error: {err}");
                None
            }
        })
        .filter(|e| e.file_type().is_file() && is_audio(e.path()))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let mtime = meta
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs() as i64;
            Some(Walked {
                path: e.into_path(),
                size: meta.len() as i64,
                mtime,
            })
        })
        .collect()
}

impl Scanner {
    pub fn new(db: Db, live: Live) -> Scanner {
        Scanner {
            db,
            live,
            running: Arc::new(AtomicBool::new(false)),
            count: Arc::new(AtomicU64::new(0)),
            index_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn status(&self) -> Result<Status, ApiError> {
        let conn = self.db.conn()?;
        let folder_count: i64 = conn.query_row("SELECT COUNT(*) FROM songs", [], |r| r.get(0))?;
        let last_scan: Option<i64> = conn.query_row(
            "SELECT last_scan_at FROM scan_state WHERE id = 1",
            [],
            |r| r.get(0),
        )?;
        Ok(Status {
            scanning: self.running.load(Ordering::Acquire),
            count: if self.running.load(Ordering::Acquire) {
                self.count.load(Ordering::Acquire)
            } else {
                folder_count as u64
            },
            folder_count,
            last_scan,
        })
    }

    pub fn start(&self, full: bool) -> bool {
        if self.running.load(Ordering::Acquire) {
            return false;
        }
        let scanner = self.clone();
        tokio::task::spawn_blocking(move || match scanner.scan_blocking(full) {
            Ok(Some(s)) => tracing::info!(
                "scan finished: {} scanned, {} pruned, {} failed",
                s.scanned,
                s.pruned,
                s.failed
            ),
            Ok(None) => {}
            Err(e) => tracing::error!("scan failed: {e}"),
        });
        true
    }

    pub fn scan_blocking(&self, full: bool) -> Result<Option<Summary>, ApiError> {
        if self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(None);
        }
        self.live.notify();
        let result = self.run_scan(full);
        self.running.store(false, Ordering::Release);
        self.live.notify();
        result.map(Some)
    }

    fn run_scan(&self, full: bool) -> Result<Summary, ApiError> {
        self.count.store(0, Ordering::Release);
        let mut conn = self.db.conn()?;
        let scan_start = now_ms();
        conn.execute("UPDATE scan_state SET gen = gen + 1 WHERE id = 1", [])?;
        let generation: i64 =
            conn.query_row("SELECT gen FROM scan_state WHERE id = 1", [], |r| r.get(0))?;

        let folders: Vec<(i64, String)> = {
            let mut stmt = conn.prepare("SELECT id, path FROM music_folders ORDER BY id")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };

        let mut summary = Summary::default();
        for (folder_id, root) in folders {
            let root = PathBuf::from(root);
            if !root.is_dir() {
                tracing::warn!(
                    "music folder {} is not accessible, skipping",
                    root.display()
                );
                continue;
            }
            let ctx = FolderCtx {
                id: folder_id,
                root: root.clone(),
                generation,
            };

            let files = walk(&root);
            let known = index::known_files(&conn, folder_id)?;
            if files.is_empty() && !known.is_empty() {
                tracing::warn!(
                    "no audio files found in {} but {} are indexed; skipping to avoid mass deletion",
                    root.display(),
                    known.len()
                );
                continue;
            }

            let mut unchanged = Vec::new();
            let mut changed = Vec::new();
            for f in &files {
                match known.get(f.path.to_string_lossy().as_ref()) {
                    Some((size, mtime, id)) if !full && *size == f.size && *mtime == f.mtime => {
                        unchanged.push(id.clone())
                    }
                    _ => changed.push(f),
                }
            }
            index::touch_seen(&mut conn, &unchanged, generation)?;
            self.count
                .fetch_add(unchanged.len() as u64, Ordering::AcqRel);

            let mut albums = HashSet::new();
            let mut moves = Vec::new();
            for chunk in changed.chunks(CHUNK) {
                let results: Vec<Result<TrackInfo, String>> = chunk
                    .par_iter()
                    .map(|f| {
                        read_track(&f.path).map_err(|e| format!("{}: {e:#}", f.path.display()))
                    })
                    .collect();
                let mut tracks = Vec::with_capacity(results.len());
                for r in results {
                    match r {
                        Ok(t) => tracks.push(t),
                        Err(e) => {
                            tracing::warn!("skipping unreadable file {e}");
                            summary.failed += 1;
                        }
                    }
                }
                let outcome = index::upsert_tracks(&mut conn, &ctx, &tracks)?;
                albums.extend(outcome.albums);
                moves.extend(outcome.album_moves);
                summary.scanned += tracks.len() as u64;
                self.count.fetch_add(chunk.len() as u64, Ordering::AcqRel);
                self.live.notify();
            }
            index::finalize_albums(&mut conn, &albums, &moves)?;
            summary.pruned += index::prune_folder(&mut conn, folder_id, generation, scan_start)?;
        }

        index::prune_orphans(&mut conn)?;
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM songs", [], |r| r.get(0))?;
        conn.execute(
            "UPDATE scan_state SET last_scan_at = ?1, last_scan_count = ?2 WHERE id = 1",
            rusqlite::params![now(), total],
        )?;
        Ok(summary)
    }

    fn folder_for(conn: &Connection, path: &Path) -> Result<Option<(i64, PathBuf)>, ApiError> {
        let mut stmt = conn.prepare("SELECT id, path FROM music_folders")?;
        let folders: Vec<(i64, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(folders
            .into_iter()
            .map(|(id, p)| (id, PathBuf::from(p)))
            .filter(|(_, root)| path.starts_with(root))
            .max_by_key(|(_, root)| root.components().count()))
    }

    pub fn index_file(&self, path: &Path) -> Result<String, ApiError> {
        let _guard = self.index_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut conn = self.db.conn()?;
        let (folder_id, root) = Self::folder_for(&conn, path)?
            .ok_or_else(|| ApiError::generic("file is outside every music folder"))?;
        let generation: i64 =
            conn.query_row("SELECT gen FROM scan_state WHERE id = 1", [], |r| r.get(0))?;
        let ctx = FolderCtx {
            id: folder_id,
            root,
            generation,
        };
        let track = read_track(path).map_err(|e| {
            tracing::warn!("index_file {}: {e:#}", path.display());
            ApiError::generic("file could not be read as audio")
        })?;
        let outcome = index::upsert_tracks(&mut conn, &ctx, &[track])?;
        index::finalize_albums(&mut conn, &outcome.albums, &outcome.album_moves)?;
        self.live.notify();
        outcome
            .song_ids
            .into_iter()
            .next()
            .ok_or_else(|| ApiError::generic("file already indexed from another path"))
    }

    pub fn predict_song_id(
        &self,
        folder_id: i64,
        root: &Path,
        target: &Path,
        t: &TrackInfo,
    ) -> String {
        let key = grouping::album_key(folder_id, &index::rel_dir(root, target), t);
        crate::ids::song(&t.audio_hash, &key.id)
    }

    pub fn refresh_albums(&self, albums: &HashSet<String>) -> Result<(), ApiError> {
        let _guard = self.index_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut conn = self.db.conn()?;
        index::finalize_albums(&mut conn, albums, &[])
    }

    pub fn index_path(&self, path: &Path) {
        if path.is_dir() {
            for f in walk(path) {
                if let Err(e) = self.index_file(&f.path) {
                    tracing::warn!("indexing {}: {e}", f.path.display());
                }
            }
        } else if is_audio(path)
            && path.is_file()
            && let Err(e) = self.index_file(path)
        {
            tracing::warn!("indexing {}: {e}", path.display());
        }
    }

    pub fn remove_path(&self, path: &Path, purge_user_rows: bool) -> Result<usize, ApiError> {
        let _guard = self.index_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut conn = self.db.conn()?;
        let exact = path.to_string_lossy().into_owned();
        let prefix = format!("{}/", exact.trim_end_matches('/'));
        let ids: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT id FROM songs WHERE path = ?1 OR substr(path, 1, length(?2)) = ?2",
            )?;
            stmt.query_map([&exact, &prefix], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        if ids.is_empty() {
            return Ok(0);
        }
        let tx = conn.transaction()?;
        for id in &ids {
            index::fts_del(&tx, id)?;
            tx.execute("DELETE FROM songs WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        index::prune_entities(&mut conn)?;
        self.live.notify();
        if purge_user_rows {
            index::prune_user_rows(&mut conn)?;
        }
        Ok(ids.len())
    }
}
