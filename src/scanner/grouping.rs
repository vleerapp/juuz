use super::tags::TrackInfo;
use crate::ids::{self, fold_key};

pub const VARIOUS_ARTISTS: &str = "Various Artists";
pub const UNKNOWN_ARTIST: &str = "[Unknown Artist]";
pub const UNKNOWN_ALBUM: &str = "[Unknown Album]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumKey {
    pub id: String,
    pub tier: u8,
    pub title: String,
}

pub fn is_disc_dir(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    let rest = ["disc", "disk", "cd"]
        .iter()
        .find_map(|p| lower.strip_prefix(p));
    let Some(rest) = rest else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '.', '_', '-']);
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    match rest[digits..].chars().next() {
        None => true,
        Some(c) => !c.is_alphanumeric(),
    }
}

pub fn dirscope(rel_dir: &str) -> String {
    let mut parts: Vec<&str> = rel_dir
        .split(['/', '\\'])
        .filter(|p| !p.is_empty())
        .collect();
    if parts.last().is_some_and(|last| is_disc_dir(last)) {
        parts.pop();
    }
    parts
        .iter()
        .map(|p| fold_key(p))
        .collect::<Vec<_>>()
        .join("/")
}

pub fn album_key(folder_id: i64, rel_dir: &str, t: &TrackInfo) -> AlbumKey {
    let scope = dirscope(rel_dir);
    let folder = folder_id.to_string();

    let Some(title) = t.album.as_deref() else {
        let artist = t
            .album_artists
            .first()
            .or_else(|| t.artists.first())
            .map(|a| fold_key(a))
            .unwrap_or_default();
        return AlbumKey {
            id: ids::album(&["unknown", &folder, &scope, &artist]),
            tier: 4,
            title: UNKNOWN_ALBUM.to_string(),
        };
    };

    if let Some(mbid) = t.mbid_release.as_deref() {
        return AlbumKey {
            id: ids::album(&["mbid", &mbid.to_lowercase()]),
            tier: 1,
            title: title.to_string(),
        };
    }

    if !t.album_artists.is_empty() {
        let artists = t
            .album_artists
            .iter()
            .map(|a| fold_key(a))
            .collect::<Vec<_>>()
            .join("\x1e");
        return AlbumKey {
            id: ids::album(&["aa", &fold_key(title), &artists]),
            tier: 2,
            title: title.to_string(),
        };
    }

    AlbumKey {
        id: ids::album(&["dir", &folder, &fold_key(title), &scope]),
        tier: 3,
        title: title.to_string(),
    }
}

pub fn derive_album_artists(primary: &[String], compilation: bool) -> Vec<String> {
    let mut unique: Vec<&String> = Vec::new();
    for name in primary {
        if !unique.iter().any(|u| fold_key(u) == fold_key(name)) {
            unique.push(name);
        }
    }
    match unique.as_slice() {
        [single] if !compilation => vec![(*single).clone()],
        [] => vec![UNKNOWN_ARTIST.to_string()],
        _ => vec![VARIOUS_ARTISTS.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(album: Option<&str>, artist: &str) -> TrackInfo {
        TrackInfo {
            album: album.map(str::to_string),
            artists: vec![artist.to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn disc_dirs() {
        for name in ["CD1", "cd 2", "Disc 02", "Disc 1 - Bonus", "disk.3"] {
            assert!(is_disc_dir(name), "{name}");
        }
        for name in ["Discovery", "CDs", "Disco 2", "Album"] {
            assert!(!is_disc_dir(name), "{name}");
        }
    }

    #[test]
    fn multi_disc_dirs_share_scope() {
        assert_eq!(
            dirscope("Artist/2020 - Album/CD1"),
            dirscope("Artist/2020 - Album/CD2")
        );
        assert_ne!(
            dirscope("Artist/2020 - Album"),
            dirscope("Artist/2021 - Other")
        );
    }

    #[test]
    fn compilation_in_one_dir_is_one_album() {
        let a = album_key(1, "Mixes/Hits", &track(Some("Hits"), "A"));
        let b = album_key(1, "Mixes/Hits", &track(Some("Hits"), "B"));
        assert_eq!(a.id, b.id);
        assert_eq!(a.tier, 3);
    }

    #[test]
    fn same_title_different_dirs_split() {
        let a = album_key(1, "A/Greatest Hits", &track(Some("Greatest Hits"), "A"));
        let b = album_key(1, "B/Greatest Hits", &track(Some("Greatest Hits"), "B"));
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn mbid_beats_everything() {
        let mut t = track(Some("X"), "A");
        t.mbid_release = Some("ABC".into());
        t.album_artists = vec!["A".into()];
        let k = album_key(1, "one", &t);
        t.artists = vec!["B".into()];
        let k2 = album_key(1, "two", &t);
        assert_eq!(k.id, k2.id);
        assert_eq!(k.tier, 1);
    }

    #[test]
    fn albumartist_tier() {
        let mut t = track(Some("X"), "A");
        t.album_artists = vec!["Main".into()];
        let k = album_key(1, "one", &t);
        t.artists = vec!["Guest".into()];
        assert_eq!(k.id, album_key(1, "other", &t).id);
        assert_eq!(k.tier, 2);
    }

    #[test]
    fn untagged_album_is_per_artist() {
        let a = album_key(1, "", &track(None, "A"));
        let b = album_key(1, "", &track(None, "B"));
        assert_ne!(a.id, b.id);
        assert_eq!(a.title, UNKNOWN_ALBUM);
    }

    #[test]
    fn derives_artist_or_various() {
        assert_eq!(
            derive_album_artists(&["A".into(), "a".into()], false),
            vec!["A"]
        );
        assert_eq!(
            derive_album_artists(&["A".into(), "B".into()], false),
            vec![VARIOUS_ARTISTS]
        );
        assert_eq!(
            derive_album_artists(&["A".into()], true),
            vec![VARIOUS_ARTISTS]
        );
        assert_eq!(derive_album_artists(&[], false), vec![UNKNOWN_ARTIST]);
    }
}
