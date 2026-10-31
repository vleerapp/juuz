use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};

use super::Scanner;
use super::tags::is_audio;
use crate::db::{Db, libraries};

const CHECK_INTERVAL: Duration = Duration::from_secs(15);

pub type Watcher = Debouncer<notify::RecommendedWatcher, RecommendedCache>;

fn hidden_below(roots: &[PathBuf], path: &Path) -> bool {
    roots
        .iter()
        .find_map(|root| path.strip_prefix(root).ok())
        .map(|rel| {
            rel.components()
                .any(|c| c.as_os_str().to_str().is_some_and(|n| n.starts_with('.')))
        })
        .unwrap_or(false)
}

fn handle(scanner: &Scanner, roots: &[PathBuf], result: DebounceEventResult) {
    let events = match result {
        Ok(events) => events,
        Err(errors) => {
            for e in errors {
                tracing::warn!("watch error: {e}");
            }
            return;
        }
    };

    let mut paths: HashSet<PathBuf> = HashSet::new();
    for ev in events {
        if matches!(ev.event.kind, EventKind::Access(_)) {
            continue;
        }
        for p in &ev.event.paths {
            if hidden_below(roots, p) {
                continue;
            }
            paths.insert(p.clone());
        }
    }

    process_paths(scanner, paths);
}

pub fn process_paths(scanner: &Scanner, paths: HashSet<PathBuf>) {
    let (present, gone): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| p.exists());
    for path in present {
        scanner.index_path(&path);
    }
    for path in gone {
        if is_audio(&path) || path.extension().is_none() {
            match scanner.remove_path(&path, false) {
                Ok(0) => {}
                Ok(n) => tracing::info!("removed {n} song(s) under {}", path.display()),
                Err(e) => tracing::warn!("removing {}: {e}", path.display()),
            }
        }
    }
}

pub struct Supervisor {
    stop: Arc<AtomicBool>,
    thread: std::thread::Thread,
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

pub fn spawn(scanner: Scanner, db: Db, watch: bool, art_dir: PathBuf) -> Result<Supervisor> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = std::thread::Builder::new()
        .name("juuz-libraries".into())
        .spawn(move || supervise(scanner, db, watch, flag, art_dir))?;
    Ok(Supervisor {
        stop,
        thread: handle.thread().clone(),
    })
}

fn supervise(scanner: Scanner, db: Db, watch: bool, stop: Arc<AtomicBool>, art_dir: PathBuf) {
    let warming = Arc::new(AtomicBool::new(false));
    let mut warmed: Option<i64> = None;
    let mut debouncer: Option<Watcher> = watch
        .then(|| {
            let handler_scanner = scanner.clone();
            let handler_db = db.clone();
            new_debouncer(
                Duration::from_secs(2),
                None,
                move |result: DebounceEventResult| {
                    handle(&handler_scanner, &handler_db.roots(), result)
                },
            )
            .inspect_err(|e| tracing::warn!("file watcher disabled: {e:#}"))
            .ok()
        })
        .flatten();
    let mut watched: HashSet<PathBuf> = HashSet::new();
    let mut last_scan: HashMap<i64, Instant> = HashMap::new();

    while !stop.load(Ordering::Acquire) {
        let libraries = db
            .conn()
            .ok()
            .and_then(|conn| libraries::list(&conn).ok())
            .unwrap_or_default();
        for library in libraries {
            let online = library.online();
            if !online {
                watched.remove(&library.path);
                continue;
            }
            if let Some(d) = debouncer.as_mut()
                && !watched.contains(&library.path)
            {
                match d.watch(&library.path, RecursiveMode::Recursive) {
                    Ok(()) => {
                        tracing::info!("watching {}", library.path.display());
                        watched.insert(library.path.clone());
                    }
                    Err(e) => tracing::warn!("watching {}: {e}", library.path.display()),
                }
            }
            if library.scan_interval > 0 {
                let due = last_scan.get(&library.id).is_none_or(|t| {
                    t.elapsed() >= Duration::from_secs(library.scan_interval as u64)
                });
                if due {
                    last_scan.insert(library.id, Instant::now());
                    if let Err(e) = scanner.scan_library_blocking(Some(library.id), false) {
                        tracing::warn!("scheduled scan of '{}' failed: {e}", library.name);
                    }
                }
            }
        }
        let scanned: Option<i64> = db.conn().ok().and_then(|conn| {
            conn.query_row(
                "SELECT last_scan_at FROM scan_state WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .ok()
            .flatten()
        });
        if scanned.is_some()
            && scanned != warmed
            && !scanner.status().is_ok_and(|s| s.scanning)
            && !warming.swap(true, Ordering::AcqRel)
        {
            warmed = scanned;
            let (db, dir, flag) = (db.clone(), art_dir.clone(), warming.clone());
            std::thread::spawn(move || {
                crate::api::media::warm_thumbnails(&db, &dir, &[128, 480]);
                flag.store(false, Ordering::Release);
            });
        }
        std::thread::park_timeout(CHECK_INTERVAL);
    }
}
