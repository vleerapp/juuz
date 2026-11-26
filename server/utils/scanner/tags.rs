use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::tag::{Accessor, ItemKey, Tag};

use super::fingerprint::audio_fingerprint;

pub const AUDIO_EXTENSIONS: &[&str] = &[
    "aac", "aiff", "aif", "flac", "mp3", "mp4", "m4a", "mp4a", "ogg", "oga", "opus", "wav", "wv",
];

#[derive(Debug, Clone, Default)]
pub struct TrackInfo {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: i64,
    pub suffix: String,
    pub content_type: String,
    pub title: String,
    pub tagged_title: bool,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub album_artists: Vec<String>,
    pub compilation: bool,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub disc_subtitle: Option<String>,
    pub year: Option<i32>,
    pub date: Option<String>,
    pub original_date: Option<String>,
    pub genres: Vec<String>,
    pub composer: Option<String>,
    pub comment: Option<String>,
    pub isrc: Option<String>,
    pub upc: Option<String>,
    pub label: Option<String>,
    pub lyrics: Option<String>,
    pub bpm: Option<u32>,
    pub mbid_recording: Option<String>,
    pub mbid_track: Option<String>,
    pub mbid_release: Option<String>,
    pub rg_track_gain: Option<f64>,
    pub rg_track_peak: Option<f64>,
    pub rg_album_gain: Option<f64>,
    pub rg_album_peak: Option<f64>,
    pub duration_ms: u64,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub bit_depth: u8,
    pub audio_hash: String,
    pub cover: Option<Vec<u8>>,
}

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn content_type_for(suffix: &str) -> &'static str {
    match suffix {
        "flac" => "audio/flac",
        "mp3" => "audio/mpeg",
        "m4a" | "mp4" | "mp4a" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/x-wav",
        "aiff" | "aif" => "audio/aiff",
        "wv" => "audio/x-wavpack",
        _ => "application/octet-stream",
    }
}

pub fn normalize_isrc(raw: &str) -> Option<String> {
    let code: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase();
    let valid = code.len() == 12
        && code[..2].chars().all(|c| c.is_ascii_alphabetic())
        && code[2..5].chars().all(|c| c.is_ascii_alphanumeric())
        && code[5..].chars().all(|c| c.is_ascii_digit());
    valid.then_some(code)
}

pub fn normalize_upc(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    ((8..=14).contains(&digits.len()) && digits.chars().any(|c| c != '0')).then_some(digits)
}

pub fn clean_name(value: &str) -> Option<String> {
    let name = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = name.trim_matches(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '/' | '&'));
    let unmatched_close = |close: char, open: char| {
        name.ends_with(close) && name.matches(open).count() < name.matches(close).count()
    };
    let name = if unmatched_close(')', '(') || unmatched_close(']', '[') {
        &name[..name.len() - 1]
    } else {
        name
    };
    (!name.is_empty()).then(|| name.to_string())
}

fn unique_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    names
        .into_iter()
        .filter_map(clean_name)
        .filter(|name| seen.insert(name.to_lowercase()))
        .collect()
}

fn split_on(part: &str, delimiter: &str, rejoin_the: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for piece in part
        .split(delimiter)
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        match (rejoin_the, out.last_mut()) {
            (Some(joiner), Some(last)) if piece.to_lowercase().starts_with("the ") => {
                last.push_str(joiner);
                last.push_str(piece);
            }
            _ => out.push(piece.to_string()),
        }
    }
    out
}

pub fn split_credit(credit: &str) -> Vec<String> {
    let credit = credit.replace('\0', ";");
    let words: Vec<&str> = credit.split_whitespace().collect();
    let mut parts: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for (i, word) in words.iter().enumerate() {
        let marker = word.trim_start_matches(['(', '[']).to_lowercase();
        let featuring = matches!(
            marker.as_str(),
            "feat." | "feat" | "ft." | "ft" | "featuring"
        );
        let crossover = matches!(*word, "x" | "×") && i > 0 && i + 1 < words.len();
        if featuring || crossover {
            parts.push(current.join(" "));
            current.clear();
        } else {
            current.push(word);
        }
    }
    parts.push(current.join(" "));

    for (delimiter, rejoin_the) in [
        (";", None),
        (" / ", None),
        (",", Some(", ")),
        (" & ", Some(" & ")),
    ] {
        parts = parts
            .iter()
            .flat_map(|part| split_on(part, delimiter, rejoin_the))
            .collect();
    }
    parts
}

pub fn parse_artists(listed: &[&str], credited: &[&str]) -> Vec<String> {
    let split_list = |values: &[&str]| {
        unique_names(
            values
                .iter()
                .flat_map(|value| value.split(['\0', ';']).collect::<Vec<_>>()),
        )
    };
    let names = split_list(listed);
    if !names.is_empty() {
        return names;
    }
    match credited {
        [] => Vec::new(),
        [credit] => {
            let names = split_credit(credit);
            unique_names(names.iter().map(String::as_str))
        }
        many => split_list(many),
    }
}

pub fn split_genres(raw: &str) -> Vec<String> {
    raw.split([',', ';', '/', '\0'])
        .map(|g| g.trim().to_string())
        .filter(|g| !g.is_empty())
        .collect()
}

fn text(tag: &Tag, key: ItemKey) -> Option<String> {
    tag.get_string(key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_gain(raw: &str) -> Option<f64> {
    raw.trim()
        .trim_end_matches("dB")
        .trim_end_matches("db")
        .trim()
        .parse()
        .ok()
}

fn parse_peak(raw: &str) -> Option<f64> {
    raw.trim().parse().ok()
}

fn year_of(raw: &str) -> Option<i32> {
    let digits: String = raw.chars().take(4).collect();
    (digits.len() == 4)
        .then(|| digits.parse::<i32>().ok())
        .flatten()
        .filter(|y| *y > 0)
}

fn is_true(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes"
    )
}

fn pick_cover(tag: &Tag) -> Option<Vec<u8>> {
    let pics = tag.pictures();
    pics.iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| {
            pics.iter().find(|p| {
                !matches!(
                    p.pic_type(),
                    PictureType::LeadArtist | PictureType::Artist | PictureType::Band
                )
            })
        })
        .map(|p| p.data().to_vec())
        .filter(|d| !d.is_empty())
}

pub fn read_cover(path: &Path) -> Option<Vec<u8>> {
    let tagged = lofty::read_from_path(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    pick_cover(tag)
}

pub fn read_track(path: &Path) -> Result<TrackInfo> {
    let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let tagged =
        lofty::read_from_path(path).with_context(|| format!("reading tags {}", path.display()))?;
    let props = tagged.properties();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let suffix = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    let mut info = TrackInfo {
        path: path.to_path_buf(),
        size: meta.len(),
        mtime,
        content_type: content_type_for(&suffix).to_string(),
        suffix,
        duration_ms: props.duration().as_millis() as u64,
        bitrate: props
            .audio_bitrate()
            .or(props.overall_bitrate())
            .unwrap_or(0),
        sample_rate: props.sample_rate().unwrap_or(0),
        channels: props.channels().unwrap_or(0),
        bit_depth: props.bit_depth().unwrap_or(0),
        ..Default::default()
    };

    if info.bitrate == 0 && info.duration_ms > 0 {
        info.bitrate = (info.size * 8 / info.duration_ms) as u32;
    }

    if let Some(tag) = tag {
        let listed: Vec<&str> = tag.get_strings(ItemKey::TrackArtists).collect();
        let credited: Vec<&str> = tag.get_strings(ItemKey::TrackArtist).collect();
        info.artists = parse_artists(&listed, &credited);

        let album_listed: Vec<&str> = tag.get_strings(ItemKey::AlbumArtists).collect();
        let album_credited: Vec<&str> = tag.get_strings(ItemKey::AlbumArtist).collect();
        info.album_artists = parse_artists(&album_listed, &album_credited);

        info.title = tag
            .title()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        info.tagged_title = !info.title.is_empty();
        info.album = tag
            .album()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        info.track = tag.track();
        info.disc = tag.disk();
        info.disc_subtitle = text(tag, ItemKey::SetSubtitle);

        info.date = text(tag, ItemKey::RecordingDate)
            .or_else(|| text(tag, ItemKey::ReleaseDate))
            .or_else(|| text(tag, ItemKey::Year));
        info.original_date = text(tag, ItemKey::OriginalReleaseDate);
        info.year = info
            .date
            .as_deref()
            .and_then(year_of)
            .or_else(|| info.original_date.as_deref().and_then(year_of));

        info.genres = tag
            .get_strings(ItemKey::Genre)
            .flat_map(split_genres)
            .collect();
        info.composer = text(tag, ItemKey::Composer);
        info.comment = text(tag, ItemKey::Comment);
        info.isrc = text(tag, ItemKey::Isrc).as_deref().and_then(normalize_isrc);
        info.upc = text(tag, ItemKey::Barcode)
            .as_deref()
            .and_then(normalize_upc);
        info.label = text(tag, ItemKey::Label);
        info.lyrics = text(tag, ItemKey::Lyrics).or_else(|| text(tag, ItemKey::UnsyncLyrics));
        info.bpm = text(tag, ItemKey::IntegerBpm)
            .or_else(|| text(tag, ItemKey::Bpm))
            .and_then(|s| s.split('.').next().and_then(|n| n.trim().parse().ok()));
        info.compilation = text(tag, ItemKey::FlagCompilation).is_some_and(|v| is_true(&v));
        info.mbid_recording = text(tag, ItemKey::MusicBrainzRecordingId);
        info.mbid_track = text(tag, ItemKey::MusicBrainzTrackId);
        info.mbid_release = text(tag, ItemKey::MusicBrainzReleaseId);
        info.rg_track_gain = text(tag, ItemKey::ReplayGainTrackGain).and_then(|s| parse_gain(&s));
        info.rg_track_peak = text(tag, ItemKey::ReplayGainTrackPeak).and_then(|s| parse_peak(&s));
        info.rg_album_gain = text(tag, ItemKey::ReplayGainAlbumGain).and_then(|s| parse_gain(&s));
        info.rg_album_peak = text(tag, ItemKey::ReplayGainAlbumPeak).and_then(|s| parse_peak(&s));
        info.cover = pick_cover(tag);
    }

    if info.title.is_empty() {
        info.title = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Unknown".into());
    }

    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    info.audio_hash = audio_fingerprint(&mut BufReader::new(file))
        .with_context(|| format!("fingerprinting {}", path.display()))?;

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_feat_and_ampersand() {
        assert_eq!(
            split_credit("Alok feat. Bruno Martini"),
            vec!["Alok", "Bruno Martini"]
        );
        assert_eq!(split_credit("A & B"), vec!["A", "B"]);
        assert_eq!(
            split_credit("Earth, Wind & Fire"),
            vec!["Earth", "Wind", "Fire"]
        );
        assert_eq!(split_credit("Tom, The Band"), vec!["Tom, The Band"]);
    }

    #[test]
    fn parses_gain() {
        assert_eq!(parse_gain("-9.80 dB"), Some(-9.8));
        assert_eq!(parse_gain("3.1"), Some(3.1));
    }

    #[test]
    fn extracts_year() {
        assert_eq!(year_of("2019-10-18"), Some(2019));
        assert_eq!(year_of("19"), None);
    }
}
