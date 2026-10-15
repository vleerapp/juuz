use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};

use super::Scanner;
use super::tags::is_audio;

pub type Watcher = Debouncer<notify::RecommendedWatcher, RecommendedCache>;

fn hidden_below(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
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
            if roots.iter().any(|r| hidden_below(r, p)) {
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

pub fn spawn(scanner: Scanner, roots: Vec<PathBuf>) -> Result<Watcher> {
    let handler_roots = roots.clone();
    let mut debouncer = new_debouncer(
        Duration::from_secs(2),
        None,
        move |result: DebounceEventResult| handle(&scanner, &handler_roots, result),
    )?;
    for root in &roots {
        debouncer.watch(root, RecursiveMode::Recursive)?;
        tracing::info!("watching {}", root.display());
    }
    Ok(debouncer)
}
