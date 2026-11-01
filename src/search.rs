use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rusqlite::{Connection, params};

use crate::db::libraries::{artist_visible, visible};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Song,
    Plain,
}

pub struct Doc {
    pub id: String,
    pub title: String,
    pub haystack: String,
}

pub struct Query {
    text: String,
    pattern: Pattern,
}

struct Scratch {
    matcher: Matcher,
    buf: Vec<char>,
}

impl Scratch {
    fn new() -> Self {
        let mut config = Config::DEFAULT;
        config.prefer_prefix = true;
        Self {
            matcher: Matcher::new(config),
            buf: Vec::new(),
        }
    }

    fn score(&mut self, pattern: &Pattern, haystack: &str) -> Option<u32> {
        pattern.score(Utf32Str::new(haystack, &mut self.buf), &mut self.matcher)
    }
}

pub fn song_haystack(title: &str, artist: &str, album: &str) -> String {
    match (artist.is_empty(), album.is_empty()) {
        (true, true) => title.to_string(),
        (false, true) => format!("{title} {artist}"),
        _ => format!("{title} {artist} {album}"),
    }
}

pub fn album_haystack(title: &str, artist: &str) -> String {
    if artist.is_empty() {
        title.to_string()
    } else {
        format!("{title} {artist}")
    }
}

impl Query {
    pub fn parse(text: &str) -> Option<Query> {
        let text = text.trim();
        (!text.is_empty()).then(|| Query {
            text: text.to_string(),
            pattern: Pattern::parse(text, CaseMatching::Ignore, Normalization::Smart),
        })
    }

    fn score(&self, scratch: &mut Scratch, mode: Mode, title: &str, haystack: &str) -> Option<u32> {
        match mode {
            Mode::Song => {
                if title.trim().eq_ignore_ascii_case(&self.text) {
                    return Some(u32::MAX);
                }
                let by_title = scratch.score(&self.pattern, title);
                let by_haystack = scratch.score(&self.pattern, haystack);
                by_title.into_iter().chain(by_haystack).max()
            }
            Mode::Plain => scratch.score(&self.pattern, haystack),
        }
    }

    pub fn order<T>(
        &self,
        mode: Mode,
        items: Vec<T>,
        text: impl Fn(&T) -> (String, String),
    ) -> Vec<T> {
        let mut scratch = Scratch::new();
        let mut hits: Vec<(u32, usize)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let (title, haystack) = text(item);
                self.score(&mut scratch, mode, &title, &haystack)
                    .map(|s| (s, i))
            })
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
        hits.into_iter()
            .filter_map(|(_, i)| slots[i].take())
            .collect()
    }

    pub fn rank(&self, mode: Mode, docs: &[Doc]) -> Vec<String> {
        let mut scratch = Scratch::new();
        let mut hits: Vec<(u32, usize)> = docs
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                self.score(&mut scratch, mode, &d.title, &d.haystack)
                    .map(|s| (s, i))
            })
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        hits.into_iter().map(|(_, i)| docs[i].id.clone()).collect()
    }
}

pub fn song_docs(
    conn: &Connection,
    uid: &str,
    folder: Option<i64>,
    extra: &str,
) -> rusqlite::Result<Vec<Doc>> {
    let folder = folder
        .map(|f| format!(" AND s.library_id = {f}"))
        .unwrap_or_default();
    let sql = format!(
        "SELECT s.id, s.title, s.display_artist, al.title FROM songs s
         JOIN albums al ON al.id = s.album_id
         WHERE {}{folder} {extra} ORDER BY s.title COLLATE NOCASE",
        visible!("s.library_id")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![uid], |r| {
        let title: String = r.get(1)?;
        let artist: String = r.get(2)?;
        let album: String = r.get(3)?;
        Ok(Doc {
            id: r.get(0)?,
            haystack: song_haystack(&title, &artist, &album),
            title,
        })
    })?;
    rows.collect()
}

pub fn album_docs(conn: &Connection, uid: &str, folder: Option<i64>) -> rusqlite::Result<Vec<Doc>> {
    let folder = folder
        .map(|f| format!(" AND al.library_id = {f}"))
        .unwrap_or_default();
    let sql = format!(
        "SELECT al.id, al.title,
            COALESCE((SELECT group_concat(a.name, ' ') FROM album_artists aa
             JOIN artists a ON a.id = aa.artist_id WHERE aa.album_id = al.id), '')
         FROM albums al WHERE {}{folder} ORDER BY al.title COLLATE NOCASE",
        visible!("al.library_id")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![uid], |r| {
        let title: String = r.get(1)?;
        let artist: String = r.get(2)?;
        Ok(Doc {
            id: r.get(0)?,
            haystack: album_haystack(&title, &artist),
            title,
        })
    })?;
    rows.collect()
}

pub fn artist_docs(
    conn: &Connection,
    uid: &str,
    folder: Option<i64>,
) -> rusqlite::Result<Vec<Doc>> {
    let folder = folder
        .map(|f| {
            format!(
                " AND EXISTS (SELECT 1 FROM album_artists fa JOIN albums fl ON fl.id = fa.album_id
                    WHERE fa.artist_id = ar.id AND fl.library_id = {f})"
            )
        })
        .unwrap_or_default();
    let sql = format!(
        "SELECT ar.id, ar.name FROM artists ar WHERE {}{folder} ORDER BY ar.name COLLATE NOCASE",
        artist_visible!("ar.id")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![uid], |r| {
        let name: String = r.get(1)?;
        Ok(Doc {
            id: r.get(0)?,
            haystack: name.clone(),
            title: name,
        })
    })?;
    rows.collect()
}

pub fn json_ids(ids: &[String]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn songs(items: &[(&str, &str, &str)]) -> Vec<Doc> {
        items
            .iter()
            .enumerate()
            .map(|(i, (t, a, al))| Doc {
                id: i.to_string(),
                title: t.to_string(),
                haystack: song_haystack(t, a, al),
            })
            .collect()
    }

    const LIBRARY: &[(&str, &str, &str)] = &[
        ("Digital Love", "Daft Punk", "Discovery"),
        ("One More Time", "Daft Punk", "Discovery"),
        ("Get Lucky", "Daft Punk", "Random Access Memories"),
        ("Bohemian Rhapsody", "Queen", "A Night at the Opera"),
        ("Love", "Someone", "Single"),
        ("Love Story", "Taylor Swift", "Fearless"),
    ];

    fn ids(query: &str) -> Vec<usize> {
        let q = Query::parse(query).unwrap();
        q.rank(Mode::Song, &songs(LIBRARY))
            .into_iter()
            .map(|i| i.parse().unwrap())
            .collect()
    }

    #[test]
    fn title_artist_and_album_in_one_query() {
        assert_eq!(ids("digital daft discovery")[0], 0);
        assert_eq!(ids("queen opera"), vec![3]);
        let hits = ids("daft discovery");
        assert!(hits.contains(&0) && hits.contains(&1) && !hits.contains(&2));
    }

    #[test]
    fn fuzzy_prefix_and_case() {
        assert_eq!(ids("bohem")[0], 3);
        assert_eq!(ids("BOHEMIAN")[0], 3);
        assert_eq!(ids("bhmn")[0], 3);
    }

    #[test]
    fn exact_title_wins() {
        assert_eq!(ids("love")[0], 4);
    }

    #[test]
    fn nothing_for_unrelated_queries() {
        assert!(ids("zzzzzz").is_empty());
        assert!(Query::parse("   ").is_none());
    }

    #[test]
    fn plain_mode_orders_by_score() {
        let q = Query::parse("abba").unwrap();
        let items = vec!["Mamma Mia", "ABBA", "Abbey Road"];
        let out = q.order(Mode::Plain, items, |s| (s.to_string(), s.to_string()));
        assert_eq!(out[0], "ABBA");
    }
}
