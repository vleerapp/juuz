use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, named_params, params};
use xxhash_rust::xxh3::{xxh3_64, xxh3_128};

use super::grouping::{self, UNKNOWN_ARTIST, album_key};
use super::tags::TrackInfo;
use crate::db::{now, now_ms};
use crate::error::ApiError;
use crate::ids;

pub struct FolderCtx {
    pub root: PathBuf,
    pub generation: i64,
    pub library_id: i64,
}

#[derive(Default)]
pub struct Outcome {
    pub song_ids: Vec<String>,
    pub albums: HashSet<String>,
    pub album_moves: Vec<(String, String)>,
    pub skipped: usize,
}

pub fn fts_rowid(id: &str) -> i64 {
    (xxh3_64(id.as_bytes()) >> 1) as i64
}

pub fn fts_put(conn: &Connection, kind: &str, id: &str, text: &str) -> rusqlite::Result<()> {
    let rowid = fts_rowid(id);
    conn.execute("DELETE FROM search_fts WHERE rowid = ?1", [rowid])?;
    conn.execute(
        "INSERT OR IGNORE INTO search_fts(rowid, kind, id, text) VALUES (?1, ?2, ?3, ?4)",
        params![rowid, kind, id, text],
    )?;
    Ok(())
}

pub fn fts_del(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM search_fts WHERE rowid = ?1", [fts_rowid(id)])?;
    Ok(())
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:032x}", xxh3_128(bytes))
}

pub fn scoped_album_key(library_id: i64, rel_dir: &str, t: &TrackInfo) -> grouping::AlbumKey {
    let mut key = album_key(rel_dir, t);
    if library_id != crate::db::libraries::DEFAULT_ID {
        key.id = ids::album(&["library", &library_id.to_string(), &key.id]);
    }
    key
}

pub fn rel_dir(root: &Path, file: &Path) -> String {
    file.parent()
        .and_then(|p| p.strip_prefix(root).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn upsert_artist(conn: &Connection, name: &str) -> rusqlite::Result<String> {
    let id = ids::artist(name);
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO artists(id, name, created_at) VALUES (?1, ?2, ?3)",
        params![id, name, now()],
    )?;
    if inserted > 0 {
        fts_put(conn, "artist", &id, name)?;
    }
    Ok(id)
}

fn upsert_genre(conn: &Connection, name: &str) -> rusqlite::Result<String> {
    let id = ids::genre(name);
    conn.execute(
        "INSERT OR IGNORE INTO genres(id, name) VALUES (?1, ?2)",
        params![id, name],
    )?;
    Ok(id)
}

pub fn repoint(conn: &Connection, old: &str, new: &str) -> rusqlite::Result<()> {
    let stmts = [
        "UPDATE OR IGNORE starred SET item_id = ?2 WHERE item_id = ?1",
        "UPDATE OR IGNORE ratings SET item_id = ?2 WHERE item_id = ?1",
        "UPDATE OR IGNORE play_stats SET song_id = ?2 WHERE song_id = ?1",
        "UPDATE OR IGNORE playlist_songs SET song_id = ?2 WHERE song_id = ?1",
        "UPDATE OR IGNORE play_queue_songs SET song_id = ?2 WHERE song_id = ?1",
        "UPDATE OR IGNORE bookmarks SET song_id = ?2 WHERE song_id = ?1",
        "UPDATE OR IGNORE share_entries SET item_id = ?2 WHERE item_id = ?1",
        "UPDATE scrobbles SET song_id = ?2 WHERE song_id = ?1",
        "UPDATE play_queue SET current_id = ?2 WHERE current_id = ?1",
    ];
    for sql in stmts {
        conn.execute(sql, params![old, new])?;
    }
    Ok(())
}

pub fn upsert_tracks(
    conn: &mut Connection,
    ctx: &FolderCtx,
    tracks: &[TrackInfo],
) -> Result<Outcome, ApiError> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut out = Outcome::default();
    let ts = now();

    for t in tracks {
        let key = scoped_album_key(ctx.library_id, &rel_dir(&ctx.root, &t.path), t);
        let album_id = key.id.clone();
        let song_id = ids::song(&t.audio_hash, &album_id);
        let path_str = t.path.to_string_lossy().into_owned();

        let existing_path: Option<String> = tx
            .query_row("SELECT path FROM songs WHERE id = ?1", [&song_id], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(p) = &existing_path
            && *p != path_str
            && Path::new(p).exists()
        {
            out.skipped += 1;
            continue;
        }

        let old_row: Option<(String, String)> = tx
            .query_row(
                "SELECT id, album_id FROM songs WHERE path = ?1",
                [&path_str],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((old_id, old_album)) = old_row
            && old_id != song_id
        {
            repoint(&tx, &old_id, &song_id)?;
            tx.execute("DELETE FROM songs WHERE id = ?1", [&old_id])?;
            fts_del(&tx, &old_id)?;
            if old_album != album_id {
                out.album_moves.push((old_album.clone(), album_id.clone()));
            }
            out.albums.insert(old_album);
        }

        if existing_path.is_none() {
            let moved: Vec<(String, String, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT id, path, album_id FROM songs WHERE audio_hash = ?1 AND id != ?2 AND path != ?3 AND library_id = ?4",
                )?;
                stmt.query_map(
                    params![t.audio_hash, song_id, path_str, ctx.library_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?
                .collect::<rusqlite::Result<_>>()?
            };
            for (old_id, old_path, old_album) in moved {
                if Path::new(&old_path).exists() {
                    continue;
                }
                repoint(&tx, &old_id, &song_id)?;
                tx.execute("DELETE FROM songs WHERE id = ?1", [&old_id])?;
                fts_del(&tx, &old_id)?;
                if old_album != album_id {
                    out.album_moves.push((old_album.clone(), album_id.clone()));
                }
                out.albums.insert(old_album);
            }
        }

        let mut artists = t.artists.clone();
        if artists.is_empty() {
            artists = t.album_artists.clone();
        }
        if artists.is_empty() {
            artists = vec![UNKNOWN_ARTIST.to_string()];
        }

        let is_va_tag = t.compilation as i64;
        tx.execute(
            "INSERT INTO albums(id, title, group_tier, mbid, upc, label, year, date, original_date, is_compilation, derived_artist, created_at, library_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(id) DO UPDATE SET
                mbid = COALESCE(albums.mbid, excluded.mbid),
                upc = COALESCE(albums.upc, excluded.upc),
                label = COALESCE(albums.label, excluded.label),
                year = COALESCE(albums.year, excluded.year),
                date = COALESCE(albums.date, excluded.date),
                original_date = COALESCE(albums.original_date, excluded.original_date),
                is_compilation = MAX(albums.is_compilation, excluded.is_compilation)",
            params![
                album_id,
                key.title,
                key.tier,
                t.mbid_release,
                t.upc,
                t.label,
                t.year,
                t.date,
                t.original_date,
                is_va_tag,
                t.album_artists.is_empty() as i64,
                ts,
                ctx.library_id
            ],
        )?;
        out.albums.insert(album_id.clone());

        if !t.album_artists.is_empty() {
            tx.execute("DELETE FROM album_artists WHERE album_id = ?1", [&album_id])?;
            for (i, name) in t.album_artists.iter().enumerate() {
                let artist_id = upsert_artist(&tx, name)?;
                tx.execute(
                    "INSERT OR IGNORE INTO album_artists(album_id, artist_id, position) VALUES (?1, ?2, ?3)",
                    params![album_id, artist_id, i as i64],
                )?;
            }
            tx.execute(
                "UPDATE albums SET derived_artist = 0 WHERE id = ?1",
                [&album_id],
            )?;
        }

        let cover_hash = t.cover.as_deref().map(hash_bytes);
        let display = artists.join(", ");

        tx.execute(
            "INSERT INTO songs(id, path, album_id, title, track, disc, disc_subtitle, year, date, original_date,
                duration_ms, bitrate, sample_rate, channels, bit_depth, size, mtime, suffix, content_type, audio_hash,
                mbid, mbid_track, isrc, bpm, comment, composer, lyrics, rg_track_gain, rg_track_peak, rg_album_gain,
                rg_album_peak, cover_hash, display_artist, seen_gen, indexed_at, created_at, library_id)
             VALUES (:id, :path, :album, :title, :track, :disc, :disc_subtitle, :year, :date, :original_date,
                :duration, :bitrate, :sample_rate, :channels, :bit_depth, :size, :mtime, :suffix, :content_type, :audio_hash,
                :mbid, :mbid_track, :isrc, :bpm, :comment, :composer, :lyrics, :rg_tg, :rg_tp, :rg_ag,
                :rg_ap, :cover, :display, :generation, :now_ms, :now, :library)
             ON CONFLICT(id) DO UPDATE SET
                path = excluded.path, album_id = excluded.album_id,
                title = excluded.title, track = excluded.track, disc = excluded.disc,
                disc_subtitle = excluded.disc_subtitle, year = excluded.year, date = excluded.date,
                original_date = excluded.original_date, duration_ms = excluded.duration_ms,
                bitrate = excluded.bitrate, sample_rate = excluded.sample_rate, channels = excluded.channels,
                bit_depth = excluded.bit_depth, size = excluded.size, mtime = excluded.mtime,
                suffix = excluded.suffix, content_type = excluded.content_type, audio_hash = excluded.audio_hash,
                mbid = excluded.mbid, mbid_track = excluded.mbid_track, isrc = excluded.isrc, bpm = excluded.bpm,
                comment = excluded.comment, composer = excluded.composer, lyrics = excluded.lyrics,
                rg_track_gain = excluded.rg_track_gain, rg_track_peak = excluded.rg_track_peak,
                rg_album_gain = excluded.rg_album_gain, rg_album_peak = excluded.rg_album_peak,
                cover_hash = excluded.cover_hash, display_artist = excluded.display_artist,
                seen_gen = excluded.seen_gen, indexed_at = excluded.indexed_at, library_id = excluded.library_id",
            named_params! {
                ":id": song_id,
                ":path": path_str,
                ":album": album_id,
                ":title": t.title,
                ":track": t.track,
                ":disc": t.disc,
                ":disc_subtitle": t.disc_subtitle,
                ":year": t.year,
                ":date": t.date,
                ":original_date": t.original_date,
                ":duration": t.duration_ms as i64,
                ":bitrate": t.bitrate,
                ":sample_rate": t.sample_rate,
                ":channels": t.channels,
                ":bit_depth": t.bit_depth,
                ":size": t.size as i64,
                ":mtime": t.mtime,
                ":suffix": t.suffix,
                ":content_type": t.content_type,
                ":audio_hash": t.audio_hash,
                ":mbid": t.mbid_recording,
                ":mbid_track": t.mbid_track,
                ":isrc": t.isrc,
                ":bpm": t.bpm,
                ":comment": t.comment,
                ":composer": t.composer,
                ":lyrics": t.lyrics,
                ":rg_tg": t.rg_track_gain,
                ":rg_tp": t.rg_track_peak,
                ":rg_ag": t.rg_album_gain,
                ":rg_ap": t.rg_album_peak,
                ":cover": cover_hash,
                ":display": display,
                ":generation": ctx.generation,
                ":library": ctx.library_id,
                ":now": ts,
                ":now_ms": now_ms(),
            },
        )?;

        tx.execute("DELETE FROM song_artists WHERE song_id = ?1", [&song_id])?;
        for (i, name) in artists.iter().enumerate() {
            let artist_id = upsert_artist(&tx, name)?;
            tx.execute(
                "INSERT OR IGNORE INTO song_artists(song_id, artist_id, position) VALUES (?1, ?2, ?3)",
                params![song_id, artist_id, i as i64],
            )?;
        }

        tx.execute("DELETE FROM song_genres WHERE song_id = ?1", [&song_id])?;
        for (i, name) in t.genres.iter().enumerate() {
            let genre_id = upsert_genre(&tx, name)?;
            tx.execute(
                "INSERT OR IGNORE INTO song_genres(song_id, genre_id, position) VALUES (?1, ?2, ?3)",
                params![song_id, genre_id, i as i64],
            )?;
        }

        let fts_text = format!("{} {} {}", t.title, display, key.title);
        fts_put(&tx, "song", &song_id, &fts_text)?;
        out.song_ids.push(song_id);
    }

    tx.commit()?;
    Ok(out)
}

const COVER_NAMES: &[&str] = &["cover", "folder", "front", "album", "albumart"];
const COVER_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp"];

pub fn find_folder_cover(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(usize, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let (Some(stem), Some(ext)) = (
            path.file_stem().and_then(|s| s.to_str()),
            path.extension().and_then(|s| s.to_str()),
        ) else {
            continue;
        };
        let (stem, ext) = (stem.to_lowercase(), ext.to_lowercase());
        if !COVER_EXTS.contains(&ext.as_str()) {
            continue;
        }
        if let Some(rank) = COVER_NAMES.iter().position(|n| *n == stem)
            && best.as_ref().is_none_or(|(r, _)| rank < *r)
        {
            best = Some((rank, path));
        }
    }
    best.map(|(_, p)| p)
}

pub fn finalize_albums(
    conn: &mut Connection,
    albums: &HashSet<String>,
    moves: &[(String, String)],
) -> Result<(), ApiError> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    for (old, new) in moves {
        let remaining: i64 = tx.query_row(
            "SELECT COUNT(*) FROM songs WHERE album_id = ?1",
            [old],
            |r| r.get(0),
        )?;
        if remaining == 0 {
            repoint(&tx, old, new)?;
        }
    }

    for album_id in albums {
        let exists: bool = tx
            .query_row("SELECT 1 FROM albums WHERE id = ?1", [album_id], |_| {
                Ok(true)
            })
            .optional()?
            .unwrap_or(false);
        if !exists {
            continue;
        }
        let song_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM songs WHERE album_id = ?1",
            [album_id],
            |r| r.get(0),
        )?;
        if song_count == 0 {
            tx.execute("DELETE FROM albums WHERE id = ?1", [album_id])?;
            fts_del(&tx, album_id)?;
            continue;
        }

        let (derived, tagged_compilation): (bool, bool) = tx.query_row(
            "SELECT derived_artist, is_compilation FROM albums WHERE id = ?1",
            [album_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;

        if derived {
            let primary: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT DISTINCT a.name FROM song_artists sa
                     JOIN artists a ON a.id = sa.artist_id
                     JOIN songs s ON s.id = sa.song_id
                     WHERE sa.position = 0 AND s.album_id = ?1",
                )?;
                stmt.query_map([album_id], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            let names = grouping::derive_album_artists(&primary, tagged_compilation);
            tx.execute("DELETE FROM album_artists WHERE album_id = ?1", [album_id])?;
            for (i, name) in names.iter().enumerate() {
                let artist_id = upsert_artist(&tx, name)?;
                tx.execute(
                    "INSERT OR IGNORE INTO album_artists(album_id, artist_id, position) VALUES (?1, ?2, ?3)",
                    params![album_id, artist_id, i as i64],
                )?;
            }
            if names.iter().any(|n| n == grouping::VARIOUS_ARTISTS) {
                tx.execute(
                    "UPDATE albums SET is_compilation = 1 WHERE id = ?1",
                    [album_id],
                )?;
            }
        }

        tx.execute(
            "UPDATE albums SET year = COALESCE(year, (SELECT MIN(year) FROM songs WHERE album_id = albums.id)),
                 date = COALESCE(date, (SELECT MIN(date) FROM songs WHERE album_id = albums.id))
             WHERE id = ?1",
            [album_id],
        )?;

        let first_path: Option<String> = tx
            .query_row(
                "SELECT path FROM songs WHERE album_id = ?1 ORDER BY disc, track LIMIT 1",
                [album_id],
                |r| r.get(0),
            )
            .optional()?;
        let folder_cover = first_path
            .as_deref()
            .and_then(|p| Path::new(p).parent())
            .and_then(find_folder_cover)
            .and_then(|p| std::fs::read(&p).ok().map(|b| (hash_bytes(&b), p)));
        let (mut cover, cover_path) = match folder_cover {
            Some((hash, path)) => (Some(hash), Some(path.to_string_lossy().into_owned())),
            None => (None, None),
        };
        if cover.is_none() {
            cover = tx
                .query_row(
                    "SELECT cover_hash FROM songs WHERE album_id = ?1 AND cover_hash IS NOT NULL
                     ORDER BY disc, track LIMIT 1",
                    [album_id],
                    |r| r.get(0),
                )
                .optional()?;
        }
        let local = cover.is_some();
        tx.execute(
            "UPDATE albums SET
                 cover_hash = CASE WHEN ?4 THEN ?2 WHEN cover_source = 'omm' THEN cover_hash END,
                 cover_path = CASE WHEN ?4 THEN ?3 WHEN cover_source = 'omm' THEN cover_path END,
                 cover_source = CASE WHEN ?4 THEN 'local' WHEN cover_source = 'omm' THEN 'omm' END
             WHERE id = ?1",
            params![album_id, cover, cover_path, local],
        )?;

        let (title, artists): (String, Option<String>) = tx.query_row(
            "SELECT al.title, (SELECT GROUP_CONCAT(a.name, ' ') FROM album_artists aa
                 JOIN artists a ON a.id = aa.artist_id WHERE aa.album_id = al.id)
             FROM albums al WHERE al.id = ?1",
            [album_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        fts_put(
            &tx,
            "album",
            album_id,
            &format!("{} {}", title, artists.unwrap_or_default()),
        )?;
    }

    tx.commit()?;
    Ok(())
}

pub fn prune_orphans(conn: &mut Connection) -> Result<(), ApiError> {
    prune_entities(conn)?;
    prune_user_rows(conn)
}

pub fn prune_entities(conn: &mut Connection) -> Result<(), ApiError> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let orphan_albums: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT id FROM albums WHERE NOT EXISTS (SELECT 1 FROM songs WHERE album_id = albums.id)",
        )?;
        stmt.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for id in &orphan_albums {
        fts_del(&tx, id)?;
        tx.execute("DELETE FROM albums WHERE id = ?1", [id])?;
    }

    let orphan_artists: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT id FROM artists
             WHERE NOT EXISTS (SELECT 1 FROM song_artists WHERE artist_id = artists.id)
               AND NOT EXISTS (SELECT 1 FROM album_artists WHERE artist_id = artists.id)",
        )?;
        stmt.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for id in &orphan_artists {
        fts_del(&tx, id)?;
        tx.execute("DELETE FROM artists WHERE id = ?1", [id])?;
    }

    tx.execute(
        "DELETE FROM genres WHERE NOT EXISTS (SELECT 1 FROM song_genres WHERE genre_id = genres.id)",
        [],
    )?;
    tx.execute(
        "DELETE FROM lyrics WHERE song_id NOT IN (SELECT id FROM songs)",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn prune_user_rows(conn: &mut Connection) -> Result<(), ApiError> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for (table, col) in [
        ("starred", "item_id"),
        ("ratings", "item_id"),
        ("play_stats", "song_id"),
        ("playlist_songs", "song_id"),
        ("play_queue_songs", "song_id"),
        ("bookmarks", "song_id"),
        ("share_entries", "item_id"),
    ] {
        tx.execute(
            &format!(
                "DELETE FROM {table} WHERE {col} NOT IN (SELECT id FROM songs UNION SELECT id FROM albums UNION SELECT id FROM artists UNION SELECT id FROM playlists)"
            ),
            [],
        )?;
    }

    tx.commit()?;
    Ok(())
}

pub fn repair_albums(conn: &mut Connection) -> Result<usize, ApiError> {
    let incomplete: HashSet<String> = {
        let mut stmt = conn.prepare(
            "SELECT al.id FROM albums al WHERE
                 NOT EXISTS (SELECT 1 FROM album_artists aa WHERE aa.album_id = al.id)
              OR (al.cover_hash IS NULL AND EXISTS (
                     SELECT 1 FROM songs s WHERE s.album_id = al.id AND s.cover_hash IS NOT NULL))",
        )?;
        stmt.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    if incomplete.is_empty() {
        return Ok(0);
    }
    finalize_albums(conn, &incomplete, &[])?;
    Ok(incomplete.len())
}

pub fn prune_stale(
    conn: &mut Connection,
    library_id: i64,
    generation: i64,
    scan_start: i64,
) -> Result<usize, ApiError> {
    let stale: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM songs WHERE library_id = ?3 AND seen_gen < ?1 AND indexed_at < ?2",
        )?;
        stmt.query_map(params![generation, scan_start, library_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    if stale.is_empty() {
        return Ok(0);
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for id in &stale {
        fts_del(&tx, id)?;
        tx.execute("DELETE FROM songs WHERE id = ?1", [id])?;
    }
    tx.commit()?;
    prune_orphans(conn)?;
    Ok(stale.len())
}

pub fn known_files(
    conn: &Connection,
    library_id: i64,
) -> rusqlite::Result<HashMap<String, (i64, i64, String)>> {
    let mut stmt = conn.prepare("SELECT path, size, mtime, id FROM songs WHERE library_id = ?1")?;
    stmt.query_map([library_id], |r| {
        Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
    })?
    .collect()
}

pub fn touch_seen(conn: &mut Connection, ids: &[String], generation: i64) -> Result<(), ApiError> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    {
        let mut stmt = tx.prepare("UPDATE songs SET seen_gen = ?1 WHERE id = ?2")?;
        for id in ids {
            stmt.execute(params![generation, id])?;
        }
    }
    tx.commit()?;
    Ok(())
}
