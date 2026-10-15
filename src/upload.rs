use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::{Accessor, ItemKey, ItemValue, Tag, TagItem};

use crate::scanner::grouping::{UNKNOWN_ALBUM, UNKNOWN_ARTIST, VARIOUS_ARTISTS};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub title: String,
    pub artists: Vec<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub year: Option<i32>,
    pub date: Option<String>,
    pub genres: Vec<String>,
    pub isrc: Option<String>,
    pub upc: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Hint {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i32>,
    pub track: Option<u32>,
}

pub fn sanitize_component(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let collapsed = mapped.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let mut out = String::new();
    for c in trimmed.chars() {
        if out.len() + c.len_utf8() > 120 {
            break;
        }
        out.push(c);
    }
    let out = out
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string();
    if out.is_empty() { "_".to_string() } else { out }
}

fn strip_year_prefix(name: &str) -> (Option<i32>, String) {
    let t = name.trim();
    let bytes = t.as_bytes();
    let (open, start) = if bytes.first() == Some(&b'(') || bytes.first() == Some(&b'[') {
        (true, 1)
    } else {
        (false, 0)
    };
    if t.len() >= start + 4 && t[start..start + 4].chars().all(|c| c.is_ascii_digit()) {
        let year: i32 = t[start..start + 4].parse().unwrap_or(0);
        let mut rest = &t[start + 4..];
        if open {
            rest = rest.trim_start_matches([')', ']']);
        }
        let trimmed = rest
            .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '–' | '.' | '_'));
        if (1900..=2100).contains(&year) && !trimmed.is_empty() && trimmed.len() < rest.len() {
            return (Some(year), trimmed.to_string());
        }
    }
    (None, t.to_string())
}

fn strip_track_prefix(stem: &str) -> (Option<u32>, String) {
    let digits: String = stem.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 3 {
        return (None, stem.trim().to_string());
    }
    let rest = &stem[digits.len()..];
    let trimmed =
        rest.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '.' | '_' | ')'));
    if trimmed.len() < rest.len() && !trimmed.is_empty() {
        (digits.parse().ok(), trimmed.to_string())
    } else {
        (None, stem.trim().to_string())
    }
}

pub fn parse_hint(relative_path: &str, original_name: Option<&str>) -> Hint {
    let parts: Vec<&str> = relative_path
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .collect();
    let mut hint = Hint::default();

    let file = parts.last().copied().or(original_name);
    if let Some(file) = file {
        let stem = Path::new(file)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.to_string());
        let (track, title) = strip_track_prefix(&stem);
        hint.track = track;
        if !title.is_empty() {
            hint.title = Some(title);
        }
    }

    let dirs = if parts.len() > 1 {
        &parts[..parts.len() - 1]
    } else {
        &[][..]
    };
    match dirs {
        [] => {}
        [album] => {
            let (year, album) = strip_year_prefix(album);
            hint.year = year;
            hint.album = Some(album);
        }
        [.., artist, album] => {
            let (year, album) = strip_year_prefix(album);
            hint.year = year;
            hint.album = Some(album);
            hint.artist = Some((*artist).to_string());
        }
    }
    hint
}

pub fn target_relative(meta: &Meta, ext: &str) -> PathBuf {
    let va = meta.album_artist.as_deref() == Some(VARIOUS_ARTISTS);
    let artist_dir = meta
        .album_artist
        .clone()
        .or_else(|| meta.artists.first().cloned())
        .unwrap_or_else(|| UNKNOWN_ARTIST.to_string());
    let album = meta
        .album
        .clone()
        .unwrap_or_else(|| UNKNOWN_ALBUM.to_string());
    let album_dir = match meta.year {
        Some(y) => format!("{y} - {album}"),
        None => album,
    };

    let mut title = meta.title.clone();
    if va && let Some(a) = meta.artists.first() {
        title = format!("{a} - {title}");
    }
    let file_stem = match (meta.track, meta.disc) {
        (Some(t), Some(d)) if d > 1 => format!("{d}-{t:02} - {title}"),
        (Some(t), _) => format!("{t:02} - {title}"),
        (None, _) => title,
    };

    let mut p = PathBuf::new();
    p.push(sanitize_component(&artist_dir));
    p.push(sanitize_component(&album_dir));
    p.push(format!("{}.{ext}", sanitize_component(&file_stem)));
    p
}

pub fn unique_path(candidate: PathBuf) -> PathBuf {
    if !candidate.exists() {
        return candidate;
    }
    let stem = candidate
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = candidate
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    for n in 2..1000 {
        let next = candidate.with_file_name(format!("{stem} ({n}).{ext}"));
        if !next.exists() {
            return next;
        }
    }
    candidate
}

pub fn contained(root: &Path, target: &Path) -> bool {
    let normalized: Vec<_> = target.components().collect();
    target.starts_with(root)
        && !normalized
            .iter()
            .any(|c| matches!(c, std::path::Component::ParentDir))
}

fn set_multi(tag: &mut Tag, key: ItemKey, values: &[String]) {
    tag.remove_key(key);
    for v in values {
        tag.push(TagItem::new(key, ItemValue::Text(v.clone())));
    }
}

pub fn write_tags(path: &Path, new: &Meta, old: &Meta) -> Result<()> {
    let mut tagged = lofty::read_from_path(path).context("reading file for tag write")?;
    if tagged.primary_tag().is_none() && tagged.first_tag().is_none() {
        let kind = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(kind));
    }
    let tag = match tagged.primary_tag_mut() {
        Some(t) => t,
        None => tagged
            .first_tag_mut()
            .ok_or_else(|| anyhow!("no writable tag"))?,
    };

    if new.title != old.title {
        tag.set_title(new.title.clone());
    }
    if new.artists != old.artists {
        tag.remove_key(ItemKey::TrackArtist);
        tag.remove_key(ItemKey::TrackArtists);
        if !new.artists.is_empty() {
            tag.insert_text(ItemKey::TrackArtist, new.artists.join(", "));
            if new.artists.len() > 1 {
                set_multi(tag, ItemKey::TrackArtists, &new.artists);
            }
        }
    }
    if new.album_artist != old.album_artist {
        tag.remove_key(ItemKey::AlbumArtist);
        if let Some(a) = &new.album_artist {
            tag.insert_text(ItemKey::AlbumArtist, a.clone());
        }
    }
    if new.album != old.album {
        match &new.album {
            Some(a) => tag.set_album(a.clone()),
            None => tag.remove_key(ItemKey::AlbumTitle),
        }
    }
    if new.track != old.track
        && let Some(t) = new.track
    {
        tag.set_track(t);
    }
    if new.disc != old.disc
        && let Some(d) = new.disc
    {
        tag.set_disk(d);
    }
    if (new.year, &new.date) != (old.year, &old.date) {
        tag.remove_key(ItemKey::RecordingDate);
        tag.remove_key(ItemKey::Year);
        if let Some(d) = new.date.clone().or_else(|| new.year.map(|y| y.to_string())) {
            tag.insert_text(ItemKey::RecordingDate, d);
        }
    }
    if new.genres != old.genres {
        set_multi(tag, ItemKey::Genre, &new.genres);
    }
    if new.isrc != old.isrc
        && let Some(isrc) = &new.isrc
    {
        tag.remove_key(ItemKey::Isrc);
        tag.insert_text(ItemKey::Isrc, isrc.clone());
    }
    if new.upc != old.upc
        && let Some(upc) = &new.upc
    {
        tag.remove_key(ItemKey::Barcode);
        tag.insert_text(ItemKey::Barcode, upc.clone());
    }

    tagged
        .save_to_path(path, WriteOptions::default())
        .context("writing tags")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_hostile_components() {
        assert_eq!(sanitize_component("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize_component(".."), "_");
        assert_eq!(sanitize_component("  AC/DC: Live?  "), "AC_DC_ Live_");
        assert_eq!(sanitize_component("name. "), "name");
        assert_eq!(sanitize_component(""), "_");
        assert!(sanitize_component(&"x".repeat(500)).len() <= 120);
        assert!(!sanitize_component("a\u{0}b\nc").contains(['\0', '\n']));
    }

    #[test]
    fn hint_from_peer_path() {
        let h = parse_hint(
            "Music\\Daft Punk\\2001 - Discovery\\03 - Digital Love.flac",
            None,
        );
        assert_eq!(h.artist.as_deref(), Some("Daft Punk"));
        assert_eq!(h.album.as_deref(), Some("Discovery"));
        assert_eq!(h.year, Some(2001));
        assert_eq!(h.track, Some(3));
        assert_eq!(h.title.as_deref(), Some("Digital Love"));
    }

    #[test]
    fn hint_traversal_is_dropped() {
        let h = parse_hint("../../x/../y.mp3", None);
        assert_eq!(h.title.as_deref(), Some("y"));
        assert!(h.artist.is_none());
    }

    #[test]
    fn hint_filename_only() {
        let h = parse_hint("", Some("07. Some Song.mp3"));
        assert_eq!(h.track, Some(7));
        assert_eq!(h.title.as_deref(), Some("Some Song"));
    }

    #[test]
    fn layout_matches_library() {
        let m = Meta {
            title: "Way Up".into(),
            artists: vec!["Nbhd Nick".into()],
            album: Some("Scoring".into()),
            year: Some(2019),
            track: Some(2),
            ..Default::default()
        };
        assert_eq!(
            target_relative(&m, "flac"),
            PathBuf::from("Nbhd Nick/2019 - Scoring/02 - Way Up.flac")
        );
    }

    #[test]
    fn layout_multi_disc_and_various() {
        let m = Meta {
            title: "T".into(),
            artists: vec!["A".into()],
            album_artist: Some(VARIOUS_ARTISTS.into()),
            album: Some("Hits".into()),
            track: Some(4),
            disc: Some(2),
            ..Default::default()
        };
        assert_eq!(
            target_relative(&m, "mp3"),
            PathBuf::from("Various Artists/Hits/2-04 - A - T.mp3")
        );
    }

    #[test]
    fn layout_unknowns() {
        let m = Meta {
            title: "x".into(),
            ..Default::default()
        };
        assert_eq!(
            target_relative(&m, "mp3"),
            PathBuf::from("[Unknown Artist]/[Unknown Album]/x.mp3")
        );
    }

    #[test]
    fn containment() {
        let root = Path::new("/music");
        assert!(contained(root, Path::new("/music/a/b.flac")));
        assert!(!contained(root, Path::new("/other/a.flac")));
        assert!(!contained(root, Path::new("/music/../etc/x")));
    }
}
