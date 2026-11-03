use std::collections::HashMap;

use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::icon::icon;
use topcoat::router::RouterBuilder;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::{HeaderName, HeaderValue, header, page, query_params, route};
use topcoat::view::{View, component, view};

use super::auth::{fail, require_role, state};
use super::nav::Page;
use super::notice::{Notice, back};
use super::page::{anchor, field, jump, notice_of, shell};
use super::playlists::{
    self, Playlist, Shelf, adding_note, playlist_create, playlist_head, playlist_tile,
};
use crate::api::model::{
    AlbumRow, SongRow, album_artist_map, album_genre_map, albums_by_ids, albums_where,
    artists_by_ids, artists_where, load_extras, songs_by_ids, songs_where, sv,
};
use crate::api::playlists as store;
use crate::db::libraries::{self, artist_visible, visible};
use crate::db::users::User;
use crate::omm::{self, SongRes};
use crate::search;

pub const SCRIPT: &str = include_str!("player.js");
const CHUNK: i64 = 60;
const MAX_CHUNK: i64 = 200;
const CANDIDATES: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum View_ {
    Albums,
    Songs,
    Artists,
    Playlists,
}

type Names = Vec<(String, String)>;

#[query_params(error = bad_request)]
struct Query {
    q: Option<String>,
    view: Option<String>,
    status: Option<String>,
    album: Option<String>,
    artist: Option<String>,
    playlist: Option<String>,
    add: Option<String>,
    sort: Option<String>,
    dir: Option<String>,
    offset: Option<i64>,
    limit: Option<i64>,
}

#[derive(Clone)]
struct Song {
    id: String,
    album_id: String,
    cover: bool,
    title: String,
    artist: String,
    artists: Names,
    album: String,
    length: String,
    gain: String,
    omm_id: Option<String>,
    number: String,
    year: String,
    format: String,
}

#[derive(Clone)]
struct Album {
    id: String,
    cover: bool,
    title: String,
    artists: Names,
    detail: String,
    genres: String,
}

#[derive(Clone)]
struct Artist {
    id: String,
    name: String,
    detail: String,
    image: bool,
}

#[derive(Clone)]
struct Counts {
    total: i64,
    matched: i64,
}

#[derive(Clone)]
struct Board {
    me: User,
    q: String,
    albums_view: bool,
    artists_view: bool,
    lists_view: bool,
    status: String,
    sort: String,
    desc: bool,
    total: i64,
    offset: i64,
    chunk: i64,
    heading: Option<Album>,
    person: Option<Artist>,
    songs: Vec<Song>,
    albums: Vec<Album>,
    people: Vec<Artist>,
    shelf: Shelf,
    adding: Option<Playlist>,
    counts: Counts,
    lookup: bool,
}

pub fn script_version() -> u64 {
    xxhash_rust::xxh3::xxh3_64(SCRIPT.as_bytes())
}

pub(super) fn clock(ms: i64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

pub(super) fn escape(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn song(row: SongRow, mut artists: Names) -> Song {
    if artists.is_empty() && !row.display_artist.is_empty() {
        artists.push((String::new(), row.display_artist.clone()));
    }
    Song {
        length: clock(row.duration_ms),
        gain: row
            .rg_track_gain
            .map(|g| format!("{g:.2}"))
            .unwrap_or_default(),
        cover: row.album_cover.is_some() || row.cover_hash.is_some(),
        id: row.id,
        album_id: row.album_id,
        title: row.title,
        artist: row.display_artist,
        artists,
        album: row.album,
        omm_id: row.omm_id,
        number: row.track.map(|t| t.to_string()).unwrap_or_default(),
        year: row.year.map(|y| y.to_string()).unwrap_or_default(),
        format: if row.bitrate > 0 {
            format!("{} {}k", row.suffix.to_uppercase(), row.bitrate)
        } else {
            row.suffix.to_uppercase()
        },
    }
}

fn album(row: &AlbumRow, artists: &HashMap<String, Vec<(String, String)>>) -> Album {
    let year = row.year.map(|y| format!("{y} - ")).unwrap_or_default();
    Album {
        id: row.id.clone(),
        cover: row.cover_hash.is_some(),
        title: row.title.clone(),
        artists: artists.get(&row.id).cloned().unwrap_or_default(),
        detail: format!("{year}{} songs", row.song_count),
        genres: String::new(),
    }
}

struct Link<'a> {
    q: &'a str,
    view: &'a str,
    status: &'a str,
    album: &'a str,
    sort: &'a str,
    dir: &'a str,
}

impl Link<'_> {
    fn href(&self) -> String {
        let mut out = format!("/library?view={}&status={}", self.view, self.status);
        if !self.q.is_empty() {
            out.push_str("&q=");
            out.push_str(&escape(self.q));
        }
        if !self.album.is_empty() {
            out.push_str("&album=");
            out.push_str(&escape(self.album));
        }
        if !self.sort.is_empty() {
            out.push_str("&sort=");
            out.push_str(self.sort);
            if !self.dir.is_empty() {
                out.push_str("&dir=");
                out.push_str(self.dir);
            }
        }
        out
    }
}

#[page("/library")]
async fn library_page(cx: &Cx) -> Result<impl View> {
    let me = require_role(cx, |u| u.stream || u.admin || u.can_catalogue()).await?;
    let notice = notice_of(cx)?;
    let query = query_params::<Query>(cx)?;
    let q: String = query
        .q
        .clone()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(100)
        .collect();
    let status = match query.status.as_deref() {
        Some("matched") if me.can_catalogue() => "matched",
        Some("unmatched") if me.can_catalogue() => "unmatched",
        _ => "all",
    }
    .to_string();
    let album_id = query.album.clone().unwrap_or_default();
    let artist_id = if album_id.is_empty() {
        query.artist.clone().unwrap_or_default()
    } else {
        String::new()
    };
    let view = match query.view.as_deref() {
        Some("songs") => View_::Songs,
        Some("artists") => View_::Artists,
        Some("playlists") => View_::Playlists,
        _ => View_::Albums,
    };
    let album_view = !album_id.is_empty();
    let artist_view = !artist_id.is_empty();
    let allowed: &[&str] = if album_view || artist_view {
        &["title", "album", "year", "format", "time"]
    } else {
        match view {
            View_::Albums => &["title", "year", "added"],
            View_::Songs => &["title", "album", "year", "format", "time"],
            View_::Artists | View_::Playlists => &[],
        }
    };
    let sort = match query.sort.as_deref() {
        Some(k) if allowed.contains(&k) => k.to_string(),
        _ => String::new(),
    };
    let desc = query.dir.as_deref() == Some("desc");
    let limit = query.limit.unwrap_or(CHUNK).clamp(1, MAX_CHUNK);
    let offset = query.offset.unwrap_or(0).max(0);
    let st = state(cx);
    let uid = me.id.clone();
    let adding = match query.add.clone() {
        Some(id)
            if view == View_::Songs
                && album_id.is_empty()
                && artist_id.is_empty()
                && (me.playlist || me.admin) =>
        {
            let owner = me.clone();
            st.db
                .run(move |c| {
                    Ok(store::owned(c, &id, &owner)
                        .ok()
                        .map(|r| playlists::playlist(&r, &owner)))
                })
                .await
                .map_err(fail)?
        }
        _ => None,
    };
    let (search, filter, wanted, sorting) = (q.clone(), status.clone(), album_id, sort.clone());
    let credited = artist_id;
    let opened = query.playlist.clone().unwrap_or_default();
    let viewer = me.clone();
    let (songs, albums, heading, person, people, shelf, counts, total, named) = st
        .db
        .run(move |c| {
            let fuzzy = search::Query::parse(&search);
            let mut songs = Vec::new();
            let mut albums = Vec::new();
            let mut heading = None;
            let mut person = None;
            let mut people = Vec::new();
            let mut shelf = Shelf::default();
            let mut total = 0;
            let dir = if desc { "DESC" } else { "ASC" };
            let song_text = |s: &SongRow| {
                (
                    s.title.clone(),
                    search::song_haystack(&s.title, &s.display_artist, &s.album),
                )
            };
            if !wanted.is_empty() {
                let found = albums_by_ids(c, &uid, std::slice::from_ref(&wanted))?;
                if let Some(a) = found.first() {
                    let artists = album_artist_map(c, std::slice::from_ref(&a.id))?;
                    let mut head = album(a, &artists);
                    head.genres = album_genre_map(c, std::slice::from_ref(&a.id))?
                        .remove(&a.id)
                        .unwrap_or_default()
                        .join(", ");
                    heading = Some(head);
                    songs = songs_where(
                        c,
                        &uid,
                        "WHERE s.album_id = ? ORDER BY s.disc, s.track, s.title COLLATE NOCASE",
                        vec![sv(&wanted)],
                    )?;
                    if let Some(q) = &fuzzy {
                        songs = q.order(search::Mode::Song, songs, song_text);
                    }
                    total = songs.len() as i64;
                }
            } else if !credited.is_empty() {
                let found = artists_by_ids(c, &uid, std::slice::from_ref(&credited))?;
                if let Some(a) = found.first() {
                    let mut rows = albums_where(
                        c,
                        &uid,
                        "WHERE (EXISTS (SELECT 1 FROM album_artists aa
                            WHERE aa.album_id = al.id AND aa.artist_id = ?)
                        OR EXISTS (SELECT 1 FROM songs sx JOIN song_artists sa ON sa.song_id = sx.id
                            WHERE sx.album_id = al.id AND sa.artist_id = ?))
                         ORDER BY al.year DESC, al.title COLLATE NOCASE",
                        vec![sv(&credited), sv(&credited)],
                    )?;
                    songs = songs_where(
                        c,
                        &uid,
                        "WHERE EXISTS (SELECT 1 FROM song_artists sa
                            WHERE sa.song_id = s.id AND sa.artist_id = ?)
                         ORDER BY al.year DESC, al.title COLLATE NOCASE, s.disc, s.track, s.title COLLATE NOCASE",
                        vec![sv(&credited)],
                    )?;
                    if let Some(q) = &fuzzy {
                        rows = q.order(search::Mode::Plain, rows, |r| {
                            (r.title.clone(), r.title.clone())
                        });
                        songs = q.order(search::Mode::Song, songs, song_text);
                    }
                    let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
                    let artists = album_artist_map(c, &ids)?;
                    albums = rows.iter().map(|r| album(r, &artists)).collect();
                    person = Some(Artist {
                        id: a.id.clone(),
                        name: a.name.clone(),
                        detail: format!("{} albums - {} songs", albums.len(), songs.len()),
                        image: a.has_image,
                    });
                    total = (albums.len() + songs.len()) as i64;
                }
            } else if view == View_::Playlists {
                let row = if opened.is_empty() {
                    None
                } else {
                    store::fetch(c, &opened, &viewer).ok()
                };
                if let Some(row) = row {
                    let ids = store::song_ids(c, &row.id)?;
                    let rows = songs_by_ids(c, &uid, &ids)?;
                    let present: std::collections::HashSet<&str> =
                        rows.iter().map(|r| r.id.as_str()).collect();
                    let slots: Vec<usize> = ids
                        .iter()
                        .enumerate()
                        .filter(|(_, id)| present.contains(id.as_str()))
                        .map(|(i, _)| i)
                        .collect();
                    let mut paired: Vec<(SongRow, usize)> = rows.into_iter().zip(slots).collect();
                    if let Some(q) = &fuzzy {
                        paired = q.order(search::Mode::Song, paired, |(s, _)| song_text(s));
                    }
                    let (kept, slots): (Vec<SongRow>, Vec<usize>) = paired.into_iter().unzip();
                    total = kept.len() as i64;
                    songs = kept;
                    shelf.slots = slots;
                    shelf.open = Some(playlists::playlist(&row, &viewer));
                } else {
                    let mut stmt = c.prepare(&format!(
                        "{} WHERE (p.owner_id = ?1 OR p.public = 1 OR ?2 = 1)
                         ORDER BY p.name COLLATE NOCASE",
                        store::SELECT
                    ))?;
                    let mut rows = stmt
                        .query_map(rusqlite::params![uid, viewer.admin], store::map)?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    if let Some(q) = &fuzzy {
                        rows = q.order(search::Mode::Plain, rows, |r| {
                            (r.name.clone(), r.name.clone())
                        });
                    }
                    shelf.lists = rows.iter().map(|r| playlists::playlist(r, &viewer)).collect();
                    total = shelf.lists.len() as i64;
                }
            } else if view == View_::Artists {
                let rows = match &fuzzy {
                    Some(q) => {
                        let ranked = q.rank(search::Mode::Plain, &search::artist_docs(c, &uid, None)?);
                        total = ranked.len() as i64;
                        let page: Vec<String> = ranked
                            .into_iter()
                            .skip(offset as usize)
                            .take(limit as usize)
                            .collect();
                        artists_by_ids(c, &uid, &page)?
                    }
                    None => {
                        total = c.query_row(
                            concat!(
                                "SELECT COUNT(*) FROM artists ar WHERE ",
                                artist_visible!("ar.id")
                            ),
                            [&uid],
                            |r| r.get(0),
                        )?;
                        artists_where(
                            c,
                            &uid,
                            &format!("ORDER BY ar.name COLLATE NOCASE LIMIT {limit} OFFSET {offset}"),
                            vec![],
                        )?
                    }
                };
                let ids: Vec<String> = rows.iter().map(|a| a.id.clone()).collect();
                let tally = song_counts(c, &uid, &ids)?;
                people = rows
                    .iter()
                    .map(|a| Artist {
                        id: a.id.clone(),
                        name: a.name.clone(),
                        detail: format!(
                            "{} albums - {} songs",
                            a.album_count,
                            tally.get(&a.id).copied().unwrap_or(0)
                        ),
                        image: a.has_image,
                    })
                    .collect();
            } else if view == View_::Songs {
                let status = match filter.as_str() {
                    "matched" => " AND s.omm_id IS NOT NULL",
                    "unmatched" => " AND s.omm_id IS NULL",
                    _ => "",
                };
                let ranked = match &fuzzy {
                    Some(q) => Some(q.rank(search::Mode::Song, &search::song_docs(c, &uid, None, status)?)),
                    None => None,
                };
                match ranked {
                    Some(ids) if sorting.is_empty() => {
                        total = ids.len() as i64;
                        let page: Vec<String> = ids
                            .into_iter()
                            .skip(offset as usize)
                            .take(limit as usize)
                            .collect();
                        songs = songs_by_ids(c, &uid, &page)?;
                    }
                    ranked => {
                        let mut wheres: Vec<&str> = Vec::new();
                        let mut args = Vec::new();
                        match filter.as_str() {
                            "matched" => wheres.push("s.omm_id IS NOT NULL"),
                            "unmatched" => wheres.push("s.omm_id IS NULL"),
                            _ => {}
                        }
                        if let Some(ids) = &ranked {
                            wheres.push("s.id IN (SELECT value FROM json_each(?))");
                            args.push(sv(&search::json_ids(ids)));
                        }
                        let clause = if wheres.is_empty() {
                            String::new()
                        } else {
                            format!("WHERE {}", wheres.join(" AND "))
                        };
                        let order = match sorting.as_str() {
                            "title" => format!(
                                "s.title COLLATE NOCASE {dir}, al.title COLLATE NOCASE, s.disc, s.track"
                            ),
                            "album" => format!(
                                "al.title COLLATE NOCASE {dir}, s.disc {dir}, s.track {dir}, s.title COLLATE NOCASE"
                            ),
                            "year" => format!(
                                "s.year {dir}, al.title COLLATE NOCASE, s.disc, s.track"
                            ),
                            "format" => format!("s.suffix {dir}, s.bitrate {dir}, s.title COLLATE NOCASE"),
                            "time" => format!("s.duration_ms {dir}, s.title COLLATE NOCASE"),
                            _ => "al.title COLLATE NOCASE, s.disc, s.track, s.title COLLATE NOCASE"
                                .to_string(),
                        };
                        let tail = format!("{clause} ORDER BY {order} LIMIT {limit} OFFSET {offset}");
                        songs = songs_where(c, &uid, &tail, args.clone())?;
                        let counting = format!(
                            "SELECT COUNT(*) FROM songs s JOIN albums al ON al.id = s.album_id WHERE {}{}",
                            visible!("s.library_id"),
                            if wheres.is_empty() {
                                String::new()
                            } else {
                                format!(" AND {}", wheres.join(" AND "))
                            }
                        );
                        let mut all = vec![sv(&uid)];
                        all.extend(args);
                        total = c.query_row(&counting, rusqlite::params_from_iter(all), |r| r.get(0))?;
                    }
                }
            } else {
                let ranked = match &fuzzy {
                    Some(q) => Some(q.rank(search::Mode::Plain, &search::album_docs(c, &uid, None)?)),
                    None => None,
                };
                let rows = match ranked {
                    Some(ids) if sorting.is_empty() => {
                        total = ids.len() as i64;
                        let page: Vec<String> = ids
                            .into_iter()
                            .skip(offset as usize)
                            .take(limit as usize)
                            .collect();
                        albums_by_ids(c, &uid, &page)?
                    }
                    ranked => {
                        let (clause, args) = match &ranked {
                            Some(ids) => (
                                "WHERE al.id IN (SELECT value FROM json_each(?))".to_string(),
                                vec![sv(&search::json_ids(ids))],
                            ),
                            None => (String::new(), vec![]),
                        };
                        let order = match sorting.as_str() {
                            "year" => "al.year DESC, al.title COLLATE NOCASE",
                            "added" => "al.created_at DESC, al.title COLLATE NOCASE",
                            _ => "al.title COLLATE NOCASE",
                        };
                        let tail = format!("{clause} ORDER BY {order} LIMIT {limit} OFFSET {offset}");
                        let rows = albums_where(c, &uid, &tail, args.clone())?;
                        let counting = format!(
                            "SELECT COUNT(*) FROM albums al WHERE {}{}",
                            visible!("al.library_id"),
                            if args.is_empty() {
                                ""
                            } else {
                                " AND al.id IN (SELECT value FROM json_each(?))"
                            }
                        );
                        let mut all = vec![sv(&uid)];
                        all.extend(args);
                        total = c.query_row(&counting, rusqlite::params_from_iter(all), |r| r.get(0))?;
                        rows
                    }
                };
                let ids: Vec<String> = rows.iter().map(|a| a.id.clone()).collect();
                let artists = album_artist_map(c, &ids)?;
                albums = rows.iter().map(|a| album(a, &artists)).collect();
            }
            let counts = c.query_row(
                concat!(
                    "SELECT
                       (SELECT COUNT(*) FROM songs s WHERE ", visible!("s.library_id"), ")
                     + (SELECT COUNT(*) FROM albums al WHERE ", visible!("al.library_id"), ")
                     + (SELECT COUNT(*) FROM artists ar WHERE ", artist_visible!("ar.id"), "),
                       (SELECT COUNT(*) FROM songs s WHERE s.omm_id IS NOT NULL AND ", visible!("s.library_id"), ")
                     + (SELECT COUNT(*) FROM albums al WHERE al.omm_id IS NOT NULL AND ", visible!("al.library_id"), ")
                     + (SELECT COUNT(*) FROM artists ar WHERE ar.omm_id IS NOT NULL AND ", artist_visible!("ar.id"), ")"
                ),
                [&uid],
                |r| {
                    Ok(Counts {
                        total: r.get(0)?,
                        matched: r.get(1)?,
                    })
                },
            )?;
            let named = load_extras(c, &songs)?.artists;
            Ok((songs, albums, heading, person, people, shelf, counts, total, named))
        })
        .await
        .map_err(fail)?;
    let board = Board {
        me,
        q,
        albums_view: view == View_::Albums && !album_view && !artist_view,
        artists_view: view == View_::Artists && !album_view && !artist_view,
        lists_view: view == View_::Playlists && !album_view && !artist_view && shelf.open.is_none(),
        status,
        sort,
        desc,
        total,
        offset,
        chunk: limit,
        heading,
        person,
        songs: songs
            .into_iter()
            .map(|row| {
                let artists = named.get(&row.id).cloned().unwrap_or_default();
                song(row, artists)
            })
            .collect(),
        albums,
        people,
        shelf,
        adding,
        counts,
        lookup: st.omm.is_some(),
    };
    Ok(view! { library_view(board: board, notice: notice) })
}

fn song_counts(
    c: &rusqlite::Connection,
    uid: &str,
    ids: &[String],
) -> rusqlite::Result<HashMap<String, i64>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let marks = vec!["?"; ids.len()].join(",");
    let mut stmt = c.prepare(&format!(
        "SELECT sa.artist_id, COUNT(*) FROM song_artists sa JOIN songs s ON s.id = sa.song_id
         WHERE {} AND sa.artist_id IN ({marks}) GROUP BY sa.artist_id",
        visible!("s.library_id")
    ))?;
    let mut all = vec![uid.to_string()];
    all.extend(ids.iter().cloned());
    let rows = stmt.query_map(rusqlite::params_from_iter(all), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    rows.collect()
}

pub(super) fn artist_href(id: &str) -> String {
    format!("/library?view=artists&artist={}", escape(id))
}

pub(super) fn album_href(id: &str) -> String {
    format!("/library?view=songs&status=all&album={}", escape(id))
}

#[component]
pub(super) async fn artist_link(id: &str, name: &str, #[default] off: bool) -> Result<impl View> {
    let href = artist_href(id);
    Ok(view! {
        if off || id.is_empty() {
            (name)
        } else {
            anchor(href: href, class: "credit", label: name, (name))
        }
    })
}

#[component]
pub(super) async fn album_link(id: &str, title: &str, #[default] off: bool) -> Result<impl View> {
    let href = album_href(id);
    Ok(view! {
        if off || id.is_empty() {
            (title)
        } else {
            anchor(href: href, class: "credit", label: title, (title))
        }
    })
}

#[component]
pub(super) async fn credits(list: &[(String, String)], #[default] off: bool) -> Result<impl View> {
    Ok(view! {
        for (i, pair) in list.iter().enumerate() {
            if i > 0 { ", " }
            artist_link(id: pair.0.as_str(), name: pair.1.as_str(), off: off)
        }
    })
}

#[component]
async fn cover(id: &str, has: bool) -> Result<impl View> {
    let src = format!("/art?id={}&size=128", escape(id));
    Ok(view! {
        if has {
            <img class="cover" src=(src.as_str()) alt="" loading="lazy">
        } else {
            <span class="cover"></span>
        }
    })
}

#[component]
async fn candidate_cover(src: &str) -> Result<impl View> {
    Ok(view! {
        if src.is_empty() {
            <span class="cover"></span>
        } else {
            <img class="cover" src=(src) alt="" loading="lazy" referrerpolicy="no-referrer">
        }
    })
}

fn full_id(input: &str, kind: &str) -> String {
    let input = input.trim();
    if !input.is_empty() && input.bytes().all(|b| b.is_ascii_digit()) {
        format!("omm:{kind}:{input}")
    } else {
        input.to_string()
    }
}

fn short_id(id: &str) -> &str {
    id.rsplit(':').next().unwrap_or(id)
}

fn at(q: &str, view: &str, status: &str, album: &str) -> String {
    at_sorted(q, view, status, album, "", "")
}

fn at_sorted(q: &str, view: &str, status: &str, album: &str, sort: &str, dir: &str) -> String {
    Link {
        q,
        view,
        status,
        album,
        sort,
        dir,
    }
    .href()
}

fn next_sort(board: &Board, key: &str) -> (String, String) {
    if board.sort != key {
        (key.to_string(), "asc".to_string())
    } else if !board.desc {
        (key.to_string(), "desc".to_string())
    } else {
        (String::new(), String::new())
    }
}

#[component]
async fn sort_head(
    href: String,
    label: &str,
    active: bool,
    desc: bool,
    clock: bool,
) -> Result<impl View> {
    let class = if active { "sort active" } else { "sort" };
    Ok(view! {
        anchor(href: href, class: class, label: label,
            if clock {
                icon(data: super::icons::DURATION_ICON)
            } else {
                (label)
            }
            if active {
                if desc {
                    icon(data: super::icons::ARROW_DOWN_ICON)
                } else {
                    icon(data: super::icons::ARROW_UP_ICON)
                }
            }
        )
    })
}

#[component]
async fn library_view(board: Board, notice: Option<Notice>) -> Result<impl View> {
    let in_artist = board.person.is_some();
    let in_list = board.shelf.open.is_some();
    let view = if board.shelf.open.is_some() || board.lists_view {
        "playlists"
    } else if board.artists_view || in_artist {
        "artists"
    } else if board.albums_view {
        "albums"
    } else {
        "songs"
    };
    let artist = board
        .person
        .as_ref()
        .map(|a| a.id.clone())
        .unwrap_or_default();
    let album = board
        .heading
        .as_ref()
        .map(|a| a.id.clone())
        .unwrap_or_default();
    let in_album = board.heading.is_some();
    let q = board.q.clone();
    let dir = if board.sort.is_empty() {
        ""
    } else if board.desc {
        "desc"
    } else {
        "asc"
    };
    let albums_href = at(&q, "albums", "all", "");
    let songs_href = at(&q, "songs", &board.status, "");
    let artists_href = at(&q, "artists", "all", "");
    let playlists_href = at(&q, "playlists", "all", "");
    let list_id = board
        .shelf
        .open
        .as_ref()
        .map(|p| p.id.clone())
        .unwrap_or_default();
    let manage = board.me.playlist || board.me.admin;
    let plain_songs = !board.albums_view
        && !board.artists_view
        && !board.lists_view
        && !in_list
        && !in_album
        && !in_artist;
    let filters = [
        ("all", at_sorted(&q, "songs", "all", "", &board.sort, dir)),
        (
            "matched",
            at_sorted(&q, "songs", "matched", "", &board.sort, dir),
        ),
        (
            "unmatched",
            at_sorted(&q, "songs", "unmatched", "", &board.sort, dir),
        ),
    ];
    let orders = [
        ("title", at_sorted(&q, "albums", "all", "", "title", "")),
        ("year", at_sorted(&q, "albums", "all", "", "year", "")),
        ("added", at_sorted(&q, "albums", "all", "", "added", "")),
    ];
    let note = format!(
        "{} items, {} with metadata",
        board.counts.total, board.counts.matched
    );
    let can_play = board.me.stream || board.me.admin;
    let edit = in_list && manage && board.shelf.open.as_ref().is_some_and(|p| p.mine);
    let list_edit = if edit { list_id.clone() } else { String::new() };
    let catalogue = board.me.can_catalogue() && board.lookup;
    let show_filters = board.me.can_catalogue() && plain_songs;
    let add_id = board
        .adding
        .as_ref()
        .map(|p| p.id.clone())
        .unwrap_or_default();
    let with_add = |href: String| {
        if add_id.is_empty() {
            href
        } else {
            format!("{href}&add={}", escape(&add_id))
        }
    };
    let heads: Vec<(String, &str, bool, bool)> = ["title", "album", "year", "format", "time"]
        .iter()
        .map(|key| {
            let (sort, dir) = next_sort(&board, key);
            (
                with_add(at_sorted(&q, "songs", &board.status, "", &sort, &dir)),
                *key,
                board.sort == *key,
                board.desc,
            )
        })
        .collect();
    let src = with_add(at_sorted(&q, view, &board.status, &album, &board.sort, dir));
    let total_text = board.total.to_string();
    let chunk_text = board.chunk.to_string();
    let offset_text = board.offset.to_string();
    Ok(view! {
        shell(
            page: Some(Page::Library),
            name: board.me.name.as_str(),
            me: Some(board.me.clone()),
            notice: notice,
            <section class="library wide">
                <div class="toolbar">
                    <nav class="segments" data-swap="nav">
                        jump(href: albums_href, label: "albums", on: board.albums_view || in_album)
                        jump(href: songs_href, label: "songs", on: plain_songs)
                        jump(href: artists_href, label: "artists", on: board.artists_view || in_artist)
                        jump(href: playlists_href, label: "playlists", on: board.lists_view || in_list)
                        if show_filters {
                            <span class="divider"></span>
                            for (name, href) in filters.iter() {
                                jump(href: href.clone(), label: *name, on: board.status == *name)
                            }
                        }
                        if board.albums_view {
                            <span class="divider"></span>
                            for (name, href) in orders.iter() {
                                jump(href: href.clone(), label: *name, on: board.sort == *name || (board.sort.is_empty() && *name == "title"))
                            }
                        }
                    </nav>
                    <form class="searchbar" method="get" action="/library" data-search="">
                        <label class="search">
                            <input name="q" type="text" placeholder="search" autocomplete="off" value=(board.q.as_str())>
                            <i class="loader"></i>
                        </label>
                        <input type="hidden" name="view" value=(view)>
                        <input type="hidden" name="status" value=(board.status.as_str())>
                        <input type="hidden" name="album" value=(album.as_str())>
                        <input type="hidden" name="artist" value=(artist.as_str())>
                        <input type="hidden" name="playlist" value=(list_id.as_str())>
                        <input type="hidden" name="add" value=(add_id.as_str())>
                        <input type="hidden" name="sort" value=(board.sort.as_str())>
                        <input type="hidden" name="dir" value=(dir)>
                    </form>
                </div>
                <div class="summary">
                    <span class="muted" data-live="counts" data-topic="1">(note.as_str())</span>
                </div>
                match &board.adding {
                    Some(p) => adding_note(list: p),
                    None => "",
                }
                <div class="results" data-swap="results">
                    <div class="live" data-live="heading" data-topic="1">
                        match &board.heading {
                            Some(a) => album_head(album: a, play: can_play, catalogue: catalogue),
                            None => "",
                        }
                        match &board.person {
                            Some(a) => artist_head(artist: a, play: can_play, catalogue: catalogue),
                            None => "",
                        }
                    </div>
                    match &board.shelf.open {
                        Some(p) => playlist_head(list: p, play: can_play, edit: manage),
                        None => "",
                    }
                    if board.lists_view && manage {
                        playlist_create()
                    }
                    if board.albums_view {
                        <div class="albums" data-more="" data-total=(total_text.as_str()) data-src=(src.as_str()) data-chunk=(chunk_text.as_str())>
                            for a in board.albums.iter() {
                                album_tile(album: a, href: at(&q, "songs", "all", &a.id), play: can_play)
                            }
                        </div>
                    } else if in_album {
                        <div class="list">
                            <div class="row head">
                                <span class="c-num">"#"</span>
                                <span class="main">"title"</span>
                                if catalogue { <span class="c-meta wide-only">"metadata"</span> }
                                <span class="c-time"><span class="muted">icon(data: super::icons::DURATION_ICON)</span></span>
                            </div>
                            <div class="rows" data-queue="">
                                for (i, s) in board.songs.iter().enumerate() {
                                    track_row(song: s, play: can_play, index: i + 1, catalogue: catalogue)
                                }
                            </div>
                        </div>
                    } else if in_artist {
                        if !board.albums.is_empty() {
                            <span class="caption">"albums"</span>
                            <div class="albums">
                                for a in board.albums.iter() {
                                    album_tile(album: a, href: at(&q, "songs", "all", &a.id), play: can_play)
                                }
                            </div>
                        }
                        if !board.songs.is_empty() {
                            <span class="caption">"songs"</span>
                            <div class="list">
                                <div class="row head">
                                    <span class="c-cover">"#"</span>
                                    <span class="main">"title"</span>
                                    <span class="c-album wide-only">"album"</span>
                                    <span class="c-year wide-only">"year"</span>
                                    <span class="c-format wide-only">"format"</span>
                                    if catalogue { <span class="c-meta wide-only">"metadata"</span> }
                                    <span class="c-time"><span class="muted">icon(data: super::icons::DURATION_ICON)</span></span>
                                </div>
                                <div class="rows" data-queue="">
                                    for s in board.songs.iter() {
                                        song_row(song: s, play: can_play, catalogue: catalogue)
                                    }
                                </div>
                            </div>
                        }
                    } else if in_list {
                        <div class="list">
                            <div class="row head">
                                <span class="c-cover">"#"</span>
                                <span class="main">"title"</span>
                                <span class="c-album wide-only">"album"</span>
                                <span class="c-year wide-only">"year"</span>
                                <span class="c-format wide-only">"format"</span>
                                <span class="c-time"><span class="muted">icon(data: super::icons::DURATION_ICON)</span></span>
                                if edit { <span class="c-act"></span> }
                            </div>
                            <div class="rows" data-queue="">
                                for (i, s) in board.songs.iter().enumerate() {
                                    song_row(song: s, play: can_play, catalogue: false, list: list_edit.as_str(), slot: board.shelf.slots.get(i).copied().unwrap_or(0))
                                }
                            </div>
                        </div>
                    } else if board.lists_view {
                        <div class="albums">
                            for p in board.shelf.lists.iter() {
                                playlist_tile(list: p, play: can_play)
                            }
                        </div>
                    } else if board.artists_view {
                        <div class="albums" data-more="" data-total=(total_text.as_str()) data-src=(src.as_str()) data-chunk=(chunk_text.as_str())>
                            for a in board.people.iter() {
                                artist_tile(artist: a)
                            }
                        </div>
                    } else {
                        <div class="list">
                            <div class="row head">
                                if !add_id.is_empty() { <span class="c-pick"></span> }
                                <span class="c-cover">"#"</span>
                                <span class="main">sort_head(href: heads[0].0.clone(), label: "title", active: heads[0].2, desc: heads[0].3, clock: false)</span>
                                <span class="c-album wide-only">sort_head(href: heads[1].0.clone(), label: "album", active: heads[1].2, desc: heads[1].3, clock: false)</span>
                                <span class="c-year wide-only">sort_head(href: heads[2].0.clone(), label: "year", active: heads[2].2, desc: heads[2].3, clock: false)</span>
                                <span class="c-format wide-only">sort_head(href: heads[3].0.clone(), label: "format", active: heads[3].2, desc: heads[3].3, clock: false)</span>
                                if catalogue { <span class="c-meta wide-only">"metadata"</span> }
                                <span class="c-time">sort_head(href: heads[4].0.clone(), label: "duration", active: heads[4].2, desc: heads[4].3, clock: true)</span>
                            </div>
                            <div class="rows virtual" data-queue="" data-virtual="" data-total=(total_text.as_str()) data-src=(src.as_str()) data-chunk=(chunk_text.as_str()) data-offset=(offset_text.as_str())>
                                for s in board.songs.iter() {
                                    song_row(song: s, play: can_play, catalogue: catalogue, adding: add_id.as_str())
                                }
                            </div>
                        </div>
                    }
                    if board.total == 0 {
                        <p class="muted">"nothing here"</p>
                    }
                </div>
            </section>
        )
    })
}

#[component]
async fn album_head(album: &Album, play: bool, #[default] catalogue: bool) -> Result<impl View> {
    let big = format!("/art?id={}&size=480", escape(&album.id));
    let target = format!("/library/candidates/album?id={}", escape(&album.id));
    Ok(view! {
        <div class="album-head">
            if play {
                <button class="thumb big" data-playall="" aria-label="play album">
                    if album.cover {
                        <img class="cover" src=(big.as_str()) alt="">
                    } else {
                        <span class="cover"></span>
                    }
                    <span class="glyph">icon(data: super::icons::PLAY_ICON)</span>
                </button>
            } else {
                <span class="thumb big">
                    if album.cover {
                        <img class="cover" src=(big.as_str()) alt="">
                    } else {
                        <span class="cover"></span>
                    }
                </span>
            }
            <div class="text">
                <strong class="title">(album.title.as_str())</strong>
                <span class="muted">credits(list: &album.artists)</span>
                <span class="muted">(album.detail.as_str())</span>
                if !album.genres.is_empty() {
                    <span class="muted">(album.genres.as_str())</span>
                }
                <nav class="acts">
                    if play { <button data-playall="">"play all"</button> }
                    if catalogue { jump(href: target.clone(), label: "match") }
                </nav>
            </div>
        </div>
    })
}

#[component]
async fn album_tile(album: &Album, href: String, play: bool) -> Result<impl View> {
    let open = href.clone();
    let big = format!("/art?id={}&size=480", escape(&album.id));
    Ok(view! {
        <div class="album-tile">
            if play {
                <button class="thumb big" data-play-album=(album.id.as_str()) aria-label="play album">
                    if album.cover {
                        <img class="cover" src=(big.as_str()) alt="" loading="lazy">
                    } else {
                        <span class="cover"></span>
                    }
                    <span class="glyph">icon(data: super::icons::PLAY_ICON)</span>
                </button>
            } else {
                anchor(href: open, class: "thumb big", label: "open album",
                    if album.cover {
                        <img class="cover" src=(big.as_str()) alt="" loading="lazy">
                    } else {
                        <span class="cover"></span>
                    }
                )
            }
            <div class="text">
                <strong>jump(href: href, label: album.title.as_str())</strong>
                <span class="muted">credits(list: &album.artists)</span>
            </div>
        </div>
    })
}

#[component]
async fn artist_head(artist: &Artist, play: bool, #[default] catalogue: bool) -> Result<impl View> {
    let big = format!("/art?id={}&size=480", escape(&artist.id));
    let target = format!("/library/candidates/artist?id={}", escape(&artist.id));
    Ok(view! {
        <div class="album-head">
            if play {
                <button class="thumb big round" data-playall="" aria-label="play artist">
                    if artist.image {
                        <img class="cover" src=(big.as_str()) alt="">
                    } else {
                        <span class="cover"></span>
                    }
                    <span class="glyph">icon(data: super::icons::PLAY_ICON)</span>
                </button>
            } else {
                <span class="thumb big round">
                    if artist.image {
                        <img class="cover" src=(big.as_str()) alt="">
                    } else {
                        <span class="cover"></span>
                    }
                </span>
            }
            <div class="text">
                <strong class="title">(artist.name.as_str())</strong>
                <span class="muted">(artist.detail.as_str())</span>
                <nav class="acts">
                    if play { <button data-playall="">"play all"</button> }
                    if catalogue { jump(href: target.clone(), label: "match") }
                </nav>
            </div>
        </div>
    })
}

#[component]
async fn artist_tile(artist: &Artist) -> Result<impl View> {
    let href = artist_href(&artist.id);
    let big = format!("/art?id={}&size=480", escape(&artist.id));
    Ok(view! {
        <div class="album-tile">
            anchor(href: href.clone(), class: "thumb big round", label: "open artist",
                if artist.image {
                    <img class="cover" src=(big.as_str()) alt="" loading="lazy">
                } else {
                    <span class="cover"></span>
                }
            )
            <div class="text">
                <strong>jump(href: href, label: artist.name.as_str())</strong>
                <span class="muted">(artist.detail.as_str())</span>
            </div>
        </div>
    })
}

#[component]
async fn song_row(
    song: &Song,
    play: bool,
    catalogue: bool,
    #[default] list: &str,
    #[default] slot: usize,
    #[default] adding: &str,
) -> Result<impl View> {
    let index = slot.to_string();
    let place = if list.is_empty() {
        String::new()
    } else {
        index.clone()
    };
    let target = format!("/library/candidates?id={}", escape(&song.id));
    let status = match &song.omm_id {
        Some(id) => short_id(id).to_string(),
        None => "none".to_string(),
    };
    let cover_flag = if song.cover { "1" } else { "" };
    Ok(view! {
        <div class="row" data-slot=(place.as_str()) data-song=(song.id.as_str())>
            if !adding.is_empty() {
                <span class="c-pick">
                    <button type="button" class="pick" data-pick=(song.id.as_str()) aria-pressed="false" aria-label="select"><span class="n"></span></button>
                </span>
            }
            <span class="c-cover">
                if play {
                    <button class="thumb" data-play=(song.id.as_str()) data-title=(song.title.as_str()) data-artist=(song.artist.as_str()) data-album=(song.album.as_str()) data-art=(song.album_id.as_str()) data-cover=(cover_flag) data-gain=(song.gain.as_str()) aria-label="play">
                        cover(id: song.album_id.as_str(), has: song.cover)
                        <span class="glyph">icon(data: super::icons::PLAY_ICON)</span>
                    </button>
                } else {
                    <span class="thumb">cover(id: song.album_id.as_str(), has: song.cover)</span>
                }
            </span>
            <span class="main">
                <strong>(song.title.as_str())</strong>
                <span class="muted">credits(list: &song.artists) <span class="inline-album">" - " album_link(id: song.album_id.as_str(), title: song.album.as_str())</span></span>
            </span>
            <span class="c-album wide-only muted">album_link(id: song.album_id.as_str(), title: song.album.as_str())</span>
            <span class="c-year wide-only muted">(song.year.as_str())</span>
            <span class="c-format wide-only muted">(song.format.as_str())</span>
            if catalogue {
                <span class="c-meta wide-only"><nav class="choose">jump(href: target, label: "match")</nav><span class="muted">(status.as_str())</span></span>
            }
            <span class="c-time muted">(song.length.as_str())</span>
            if !list.is_empty() {
                <span class="c-act">
                    <form method="post" action="/library/playlists/remove">
                        <input type="hidden" name="id" value=(list)>
                        <input type="hidden" name="song" value=(song.id.as_str())>
                        <input type="hidden" name="index" value=(index.as_str())>
                        <button class="ghost danger" aria-label="remove from playlist">"drop"</button>
                    </form>
                </span>
            }
        </div>
    })
}

#[component]
async fn track_row(song: &Song, play: bool, index: usize, catalogue: bool) -> Result<impl View> {
    let number = if song.number.is_empty() {
        index.to_string()
    } else {
        song.number.clone()
    };
    let cover_flag = if song.cover { "1" } else { "" };
    let target = format!("/library/candidates?id={}", escape(&song.id));
    let status = match &song.omm_id {
        Some(id) => short_id(id).to_string(),
        None => "none".to_string(),
    };
    Ok(view! {
        <div class="row">
            <span class="c-num">
                if play {
                    <button class="number" data-play=(song.id.as_str()) data-title=(song.title.as_str()) data-artist=(song.artist.as_str()) data-album=(song.album.as_str()) data-art=(song.album_id.as_str()) data-cover=(cover_flag) data-gain=(song.gain.as_str()) aria-label="play">
                        <span class="n">(number.as_str())</span>
                        <span class="glyph">icon(data: super::icons::PLAY_ICON)</span>
                    </button>
                } else {
                    <span class="muted">(number.as_str())</span>
                }
            </span>
            <span class="main">
                <strong>(song.title.as_str())</strong>
                <span class="muted">credits(list: &song.artists)</span>
            </span>
            if catalogue {
                <span class="c-meta wide-only"><nav class="choose">jump(href: target, label: "match")</nav><span class="muted">(status.as_str())</span></span>
            }
            <span class="c-time muted">(song.length.as_str())</span>
        </div>
    })
}

#[query_params(error = bad_request)]
struct CandidateQuery {
    id: String,
}

#[derive(Clone)]
struct Candidate {
    id: String,
    short: String,
    art: String,
    name: String,
    artist: String,
    album: String,
    year: String,
    number: String,
    genre: String,
    isrc: String,
    length: String,
    current: bool,
}

#[derive(Clone)]
struct Choice {
    me: User,
    facts: Vec<(String, String)>,
    song: Song,
    candidates: Vec<Candidate>,
}

fn candidate(res: &SongRes, current: Option<&str>) -> Candidate {
    let attrs = &res.attributes;
    let year = attrs
        .release_date
        .as_deref()
        .and_then(|d| d.get(..4))
        .unwrap_or("");
    let length = attrs.duration_ms.map(clock).unwrap_or_default();
    Candidate {
        id: res.id.clone(),
        short: short_id(&res.id).to_string(),
        name: attrs.name.clone(),
        artist: joined(&attrs.artist_names),
        art: omm::related_album(res, attrs.album_name.as_deref())
            .and_then(|a| a.attributes.artwork_url.as_deref())
            .map(|u| u.replace("{w}", "128").replace("{h}", "128"))
            .unwrap_or_default(),
        album: attrs.album_name.clone().unwrap_or_default(),
        year: year.to_string(),
        number: match (attrs.disc_number, attrs.track_number) {
            (Some(d), Some(t)) => format!("{d}-{t}"),
            (None, Some(t)) => t.to_string(),
            _ => String::new(),
        },
        genre: joined(&attrs.genres),
        isrc: attrs.isrc.clone().unwrap_or_default(),
        length,
        current: current == Some(res.id.as_str()),
    }
}

#[page("/library/candidates")]
async fn candidates_page(cx: &Cx) -> Result<impl View> {
    let me = require_role(cx, User::can_catalogue).await?;
    let notice = notice_of(cx)?;
    let query = query_params::<CandidateQuery>(cx)?;
    let st = state(cx);
    let Some(client) = st.omm.clone() else {
        return Err(back(Page::Library, Notice::CatalogueOff).into());
    };
    let (uid, id) = (me.id.clone(), query.id.clone());
    let (rows, named, facts) = st
        .db
        .run(move |c| {
            let rows = songs_by_ids(c, &uid, std::slice::from_ref(&id))?;
            let named = load_extras(c, &rows)?.artists;
            let facts = if rows.is_empty() {
                Vec::new()
            } else {
                song_facts(c, &id)?
            };
            Ok((rows, named, facts))
        })
        .await
        .map_err(fail)?;
    let Some(row) = rows.into_iter().next() else {
        return Err(back(Page::Library, Notice::Invalid).into());
    };
    let (title, artist, album, duration, isrc, current) = (
        row.title.clone(),
        row.display_artist.clone(),
        row.album.clone(),
        row.duration_ms,
        row.isrc.clone(),
        row.omm_id.clone(),
    );
    let live = topcoat::router::request::headers(cx).contains_key("x-live");
    let found = if live {
        Vec::new()
    } else {
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<SongRes>> {
            let mut found = client.search_songs(
                &title,
                &artist,
                Some(&album),
                duration,
                CANDIDATES,
                omm::Scope::default(),
            )?;
            if let Some(isrc) = isrc.as_deref().filter(|i| !i.is_empty()) {
                for (_, songs) in client.songs_by_isrc(&[isrc.to_string()])? {
                    found.extend(songs);
                }
            }
            if let Some(id) = current.as_deref()
                && let Some(song) = client.catalog_song(id)?
            {
                found.push(song);
            }
            let mut seen = std::collections::HashSet::new();
            found.retain(|s| seen.insert(s.id.clone()));
            Ok(found)
        })
        .await
        .map_err(|_| fail(crate::error::ApiError::generic("internal server error")))?
        .unwrap_or_default()
    };
    let current = row.omm_id.clone();
    let mine = named.get(&row.id).cloned().unwrap_or_default();
    let choice = Choice {
        me,
        facts,
        candidates: found
            .iter()
            .map(|s| candidate(s, current.as_deref()))
            .collect(),
        song: song(row, mine),
    };
    Ok(view! { candidates_view(choice: choice, notice: notice) })
}

#[component]
async fn candidates_view(choice: Choice, notice: Option<Notice>) -> Result<impl View> {
    let song = choice.song.clone();
    let big = format!("/art?id={}&size=480", escape(&song.album_id));
    let detail = [
        song.year.as_str(),
        song.format.as_str(),
        song.length.as_str(),
    ]
    .iter()
    .filter(|p| !p.is_empty())
    .copied()
    .collect::<Vec<_>>()
    .join(" - ");
    let matched = match &song.omm_id {
        Some(id) => format!("matched: {}", short_id(id)),
        None => "not matched".to_string(),
    };
    Ok(view! {
        shell(
            page: Some(Page::Library),
            name: choice.me.name.as_str(),
            me: Some(choice.me.clone()),
            notice: notice,
            <section class="panel wide">
                <div class="live" data-live="match" data-topic="1">
                <div class="album-head">
                    <span class="thumb big">
                        if song.cover {
                            <img class="cover" src=(big.as_str()) alt="">
                        } else {
                            <span class="cover"></span>
                        }
                    </span>
                    <div class="text">
                        <span class="kind">"song"</span>
                        <strong class="title">(song.title.as_str())</strong>
                        <span class="muted">credits(list: &song.artists)</span>
                        <span class="muted">album_link(id: song.album_id.as_str(), title: song.album.as_str())</span>
                        <span class="muted">(detail.as_str())</span>
                        <span class="muted">(matched.as_str())</span>
                        <nav class="acts">
                            if song.omm_id.is_some() {
                                <form method="post" action="/library/clear">
                                    <input type="hidden" name="id" value=(song.id.as_str())>
                                    <button class="ghost danger">"clear match"</button>
                                </form>
                            }
                        </nav>
                    </div>
                </div>
                <dl class="facts">
                    for (k, v) in choice.facts.iter() {
                        <div><dt>(k.as_str())</dt><dd>(v.as_str())</dd></div>
                    }
                </dl>
                </div>
                <span class="caption">"search results"</span>
                <div class="list">
                    <div class="row head">
                        <span class="c-cover">"#"</span>
                        <span class="main">"title"</span>
                        <span class="c-album">"album"</span>
                        <span class="c-year">"year"</span>
                        <span class="c-year">"disc-track"</span>
                        <span class="c-genre">"genre"</span>
                        <span class="c-code">"isrc"</span>
                        <span class="c-id">"id"</span>
                        <span class="c-time"><span class="muted">icon(data: super::icons::DURATION_ICON)</span></span>
                        <span class="c-act"></span>
                    </div>
                    <div class="rows">
                        for c in choice.candidates.iter() {
                            <form method="post" action="/library/pick" class="row">
                                <input type="hidden" name="id" value=(song.id.as_str())>
                                <input type="hidden" name="omm" value=(c.id.as_str())>
                                <span class="c-cover">
                                    <span class="thumb">candidate_cover(src: c.art.as_str())</span>
                                </span>
                                <span class="main">
                                    <strong>(c.name.as_str())</strong>
                                    <span class="muted">(c.artist.as_str())</span>
                                </span>
                                <span class="c-album muted">(c.album.as_str())</span>
                                <span class="c-year muted">(c.year.as_str())</span>
                                <span class="c-year muted">(c.number.as_str())</span>
                                <span class="c-genre muted">(c.genre.as_str())</span>
                                <span class="c-code muted">(c.isrc.as_str())</span>
                                <span class="c-id muted">(c.short.as_str())</span>
                                <span class="c-time muted">(c.length.as_str())</span>
                                <span class="c-act"><button>if c.current { "keep" } else { "use" }</button></span>
                            </form>
                        }
                    </div>
                </div>
                if choice.candidates.is_empty() {
                    <p class="muted">"nothing found"</p>
                }
                <span class="caption">"or paste an id"</span>
                <form method="post" action="/library/pick">
                    <input type="hidden" name="id" value=(song.id.as_str())>
                    field(label: "omm id", name: "omm", kind: "text", auto: "off", mode: "text", placeholder: "1234567890")
                    <button>"use id"</button>
                </form>
            </section>
        )
    })
}

#[derive(Deserialize)]
struct PickForm {
    id: String,
    omm: String,
}

#[route(POST "/library/pick")]
async fn pick(cx: &Cx, Form(form): Form<PickForm>) -> Result<SeeOther> {
    let me = require_role(cx, User::can_catalogue).await?;
    let st = state(cx);
    let Some(client) = st.omm.clone() else {
        return Ok(back(Page::Library, Notice::CatalogueOff));
    };
    let wanted = full_id(&form.omm, "song");
    if !omm::valid_id(&wanted, "song") {
        return Ok(back(Page::Library, Notice::Invalid));
    }
    let (uid, id) = (me.id.clone(), form.id.clone());
    let allowed = st
        .db
        .run(move |c| Ok(libraries::can_access_song(c, &uid, &id)?))
        .await
        .map_err(fail)?;
    if !allowed {
        return Ok(back(Page::Library, Notice::Invalid));
    }
    let fetched = tokio::task::spawn_blocking(move || client.catalog_song(&wanted))
        .await
        .map_err(|_| fail(crate::error::ApiError::generic("internal server error")))?;
    let Ok(Some(found)) = fetched else {
        return Ok(back(Page::Library, Notice::Invalid));
    };
    let id = form.id;
    st.db
        .run(move |c| {
            let (local, path): (String, String) = c.query_row(
                "SELECT al.title, s.path FROM songs s JOIN albums al ON al.id = s.album_id WHERE s.id = ?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let file = crate::scanner::tags::read_track(std::path::Path::new(&path)).ok();
            omm::store_manual_match(c, &id, &found, Some(&local), file.as_ref())?;
            Ok(())
        })
        .await
        .map_err(fail)?;
    st.live.notify_topics(crate::live::Live::LIBRARY);
    if let Some(handle) = &st.omm_handle {
        handle.wake();
    }
    Ok(back(Page::Library, Notice::CatalogueMatched))
}

#[derive(Deserialize)]
struct ClearForm {
    id: String,
}

#[route(POST "/library/clear")]
async fn clear(cx: &Cx, Form(form): Form<ClearForm>) -> Result<SeeOther> {
    let me = require_role(cx, User::can_catalogue).await?;
    let cleared = state(cx)
        .db
        .run(move |c| {
            if !libraries::can_access_song(c, &me.id, &form.id)? {
                return Ok(false);
            }
            let path: String =
                c.query_row("SELECT path FROM songs WHERE id = ?1", [&form.id], |r| {
                    r.get(0)
                })?;
            let file = crate::scanner::tags::read_track(std::path::Path::new(&path)).ok();
            omm::clear_match(c, &form.id, file.as_ref())?;
            Ok(true)
        })
        .await
        .map_err(fail)?;
    state(cx).live.notify_topics(crate::live::Live::LIBRARY);
    Ok(back(
        Page::Library,
        if cleared {
            Notice::CatalogueCleared
        } else {
            Notice::Invalid
        },
    ))
}

#[derive(Clone)]
struct Match {
    kind: &'static str,
    id: String,
    title: String,
    sub: String,
    art: bool,
    omm_id: Option<String>,
    facts: Vec<(String, String)>,
}

#[derive(Clone)]
struct Pick {
    id: String,
    short: String,
    name: String,
    sub: String,
    year: String,
    genre: String,
    tracks: String,
    label: String,
    code: String,
    art: String,
    current: bool,
}

#[derive(Clone)]
struct MatchChoice {
    me: User,
    found: Match,
    picks: Vec<Pick>,
}

fn fact(out: &mut Vec<(String, String)>, label: &str, value: impl Into<String>) {
    let value = value.into();
    if !value.trim().is_empty() {
        out.push((label.to_string(), value));
    }
}

fn names_of(c: &rusqlite::Connection, sql: &str, id: &str) -> rusqlite::Result<String> {
    let mut stmt = c.prepare(sql)?;
    let names: Vec<String> = stmt
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(names.join(", "))
}

fn song_facts(c: &rusqlite::Connection, id: &str) -> rusqlite::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    c.query_row(
        "SELECT s.title, s.track, s.disc, s.year, s.date, s.original_date, s.duration_ms, s.bitrate,
                s.sample_rate, s.channels, s.bit_depth, s.size, s.suffix, s.isrc, s.bpm, s.comment,
                s.composer, s.mbid, s.path, s.omm_id, al.title, al.upc, al.label, s.disc_subtitle, s.album_id
         FROM songs s JOIN albums al ON al.id = s.album_id WHERE s.id = ?1",
        [id],
        |r| {
            let text = |i: usize| -> rusqlite::Result<String> {
                Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
            };
            let num = |i: usize| -> rusqlite::Result<i64> {
                Ok(r.get::<_, Option<i64>>(i)?.unwrap_or(0))
            };
            fact(&mut out, "title", text(0)?);
            fact(
                &mut out,
                "artists",
                names_of(
                    c,
                    "SELECT a.name FROM song_artists sa JOIN artists a ON a.id = sa.artist_id WHERE sa.song_id = ?1 ORDER BY sa.position",
                    id,
                )?,
            );
            fact(&mut out, "album", text(20)?);
            fact(
                &mut out,
                "album artists",
                names_of(
                    c,
                    "SELECT a.name FROM album_artists aa JOIN artists a ON a.id = aa.artist_id WHERE aa.album_id = ?1 ORDER BY aa.position",
                    &text(24)?,
                )?,
            );
            if num(1)? > 0 {
                fact(&mut out, "track", num(1)?.to_string());
            }
            if num(2)? > 0 {
                fact(&mut out, "disc", num(2)?.to_string());
            }
            fact(&mut out, "disc title", text(23)?);
            if num(3)? > 0 {
                fact(&mut out, "year", num(3)?.to_string());
            }
            fact(&mut out, "date", text(4)?);
            fact(&mut out, "original date", text(5)?);
            fact(
                &mut out,
                "genres",
                names_of(
                    c,
                    "SELECT g.name FROM song_genres sg JOIN genres g ON g.id = sg.genre_id WHERE sg.song_id = ?1 ORDER BY sg.position",
                    id,
                )?,
            );
            fact(&mut out, "isrc", text(13)?);
            fact(&mut out, "length", if num(6)? > 0 { clock(num(6)?) } else { String::new() });
            let bitrate = num(7)?;
            let rate = num(8)?;
            let depth = num(10)?;
            let channels = num(9)?;
            let format = [
                text(12)?.to_uppercase(),
                if bitrate > 0 { format!("{bitrate} kbps") } else { String::new() },
                if rate > 0 { format!("{} kHz", rate as f64 / 1000.0) } else { String::new() },
                if depth > 0 { format!("{depth}-bit") } else { String::new() },
                match channels {
                    0 => String::new(),
                    1 => "mono".to_string(),
                    2 => "stereo".to_string(),
                    n => format!("{n} ch"),
                },
            ]
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
            fact(&mut out, "format", format);
            fact(&mut out, "size", format!("{:.1} MB", num(11)? as f64 / 1_048_576.0));
            fact(&mut out, "bpm", if num(14)? > 0 { num(14)?.to_string() } else { String::new() });
            fact(&mut out, "composer", text(16)?);
            fact(&mut out, "comment", text(15)?);
            fact(&mut out, "upc", text(21)?);
            fact(&mut out, "label", text(22)?);
            fact(&mut out, "musicbrainz id", text(17)?);
            let path = text(18)?;
            fact(&mut out, "file", path.rsplit('/').next().unwrap_or(&path).to_string());
            Ok(())
        },
    )?;
    Ok(out)
}

fn album_facts(c: &rusqlite::Connection, id: &str) -> rusqlite::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    c.query_row(
        "SELECT al.title, al.year, al.date, al.original_date, al.upc, al.label, al.mbid, al.is_compilation,
                (SELECT COUNT(*) FROM songs s WHERE s.album_id = al.id),
                (SELECT COALESCE(SUM(s.duration_ms), 0) FROM songs s WHERE s.album_id = al.id)
         FROM albums al WHERE al.id = ?1",
        [id],
        |r| {
            let text = |i: usize| -> rusqlite::Result<String> {
                Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
            };
            fact(&mut out, "title", text(0)?);
            fact(
                &mut out,
                "artists",
                names_of(
                    c,
                    "SELECT a.name FROM album_artists aa JOIN artists a ON a.id = aa.artist_id WHERE aa.album_id = ?1 ORDER BY aa.position",
                    id,
                )?,
            );
            if let Some(year) = r.get::<_, Option<i64>>(1)? {
                fact(&mut out, "year", year.to_string());
            }
            fact(&mut out, "date", text(2)?);
            fact(&mut out, "original date", text(3)?);
            fact(&mut out, "upc", text(4)?);
            fact(&mut out, "label", text(5)?);
            fact(&mut out, "musicbrainz id", text(6)?);
            if r.get::<_, i64>(7)? != 0 {
                fact(&mut out, "compilation", "yes");
            }
            fact(&mut out, "songs", r.get::<_, i64>(8)?.to_string());
            fact(&mut out, "length", clock(r.get::<_, i64>(9)?));
            fact(
                &mut out,
                "genres",
                names_of(
                    c,
                    "SELECT g.name FROM song_genres sg JOIN genres g ON g.id = sg.genre_id
                     JOIN songs s ON s.id = sg.song_id WHERE s.album_id = ?1 GROUP BY g.id ORDER BY COUNT(*) DESC",
                    id,
                )?,
            );
            Ok(())
        },
    )?;
    Ok(out)
}

fn artist_facts(c: &rusqlite::Connection, id: &str) -> rusqlite::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    c.query_row(
        "SELECT name, sort_name, mbid,
                (SELECT COUNT(DISTINCT aa.album_id) FROM album_artists aa WHERE aa.artist_id = ar.id),
                (SELECT COUNT(*) FROM song_artists sa WHERE sa.artist_id = ar.id)
         FROM artists ar WHERE ar.id = ?1",
        [id],
        |r| {
            let text = |i: usize| -> rusqlite::Result<String> {
                Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
            };
            fact(&mut out, "name", text(0)?);
            fact(&mut out, "sort name", text(1)?);
            fact(&mut out, "musicbrainz id", text(2)?);
            fact(&mut out, "albums", r.get::<_, i64>(3)?.to_string());
            fact(&mut out, "songs", r.get::<_, i64>(4)?.to_string());
            Ok(())
        },
    )?;
    Ok(out)
}

fn joined(names: &[String]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for n in names {
        if !seen.contains(&n.as_str()) {
            seen.push(n);
        }
    }
    seen.join(", ")
}

fn year_of(date: Option<&str>) -> String {
    date.and_then(|d| d.get(..4)).unwrap_or("").to_string()
}

fn art_url(template: Option<&str>) -> String {
    template
        .map(|u| u.replace("{w}", "128").replace("{h}", "128"))
        .unwrap_or_default()
}

async fn match_page(cx: &Cx, kind: &'static str) -> Result<MatchChoice> {
    let me = require_role(cx, User::can_catalogue).await?;
    let query = query_params::<CandidateQuery>(cx)?;
    let st = state(cx);
    let Some(client) = st.omm.clone() else {
        return Err(back(Page::Library, Notice::CatalogueOff).into());
    };
    let (uid, id) = (me.id.clone(), query.id.clone());
    let found = st
        .db
        .run(move |c| {
            if kind == "album" {
                if !libraries::can_access_album(c, &uid, &id)? {
                    return Ok(None);
                }
                Ok(c.query_row(
                    "SELECT al.title, COALESCE((SELECT a.name FROM album_artists aa JOIN artists a ON a.id = aa.artist_id
                         WHERE aa.album_id = al.id ORDER BY aa.position LIMIT 1), ''), al.omm_id, al.cover_hash IS NOT NULL
                     FROM albums al WHERE al.id = ?1",
                    [&id],
                    |r| {
                        Ok(Match {
                            kind,
                            id: id.clone(),
                            title: r.get(0)?,
                            sub: r.get(1)?,
                            omm_id: r.get(2)?,
                            art: r.get(3)?,
                            facts: Vec::new(),
                        })
                    },
                )
                .ok()
                .map(|mut m| {
                    m.facts = album_facts(c, &id).unwrap_or_default();
                    m
                }))
            } else {
                if !libraries::can_access_artist(c, &uid, &id)? {
                    return Ok(None);
                }
                Ok(c.query_row(
                    "SELECT name, omm_id, image_hash IS NOT NULL FROM artists WHERE id = ?1",
                    [&id],
                    |r| {
                        Ok(Match {
                            kind,
                            id: id.clone(),
                            title: r.get(0)?,
                            sub: String::new(),
                            omm_id: r.get(1)?,
                            art: r.get(2)?,
                            facts: Vec::new(),
                        })
                    },
                )
                .ok()
                .map(|mut m| {
                    m.facts = artist_facts(c, &id).unwrap_or_default();
                    m
                }))
            }
        })
        .await
        .map_err(fail)?;
    let Some(found) = found else {
        return Err(back(Page::Library, Notice::Invalid).into());
    };
    if topcoat::router::request::headers(cx).contains_key("x-live") {
        return Ok(MatchChoice {
            me,
            found,
            picks: Vec::new(),
        });
    }
    let (title, sub, current) = (found.title.clone(), found.sub.clone(), found.omm_id.clone());
    let picks = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Pick>> {
        let mut picks = if kind == "album" {
            let mut list = client.search_albums(&title, &sub, CANDIDATES, omm::Scope::default())?;
            if let Some(id) = current.as_deref()
                && let Some(album) = client.catalog_album(id)?
            {
                list.push(album);
            }
            list.into_iter()
                .map(|a| Pick {
                    short: short_id(&a.id).to_string(),
                    name: a.attributes.name,
                    sub: joined(&a.attributes.artist_names),
                    year: year_of(a.attributes.release_date.as_deref()),
                    genre: joined(&a.attributes.genres),
                    tracks: a
                        .attributes
                        .track_count
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                    label: a.attributes.label.clone().unwrap_or_default(),
                    code: a.attributes.upc.clone().unwrap_or_default(),
                    art: art_url(a.attributes.artwork_url.as_deref()),
                    current: current.as_deref() == Some(a.id.as_str()),
                    id: a.id,
                })
                .collect::<Vec<_>>()
        } else {
            let mut list = client.search_artists(&title, CANDIDATES)?;
            if let Some(id) = current.as_deref()
                && let Some(artist) = client.catalog_artist(id)?
            {
                list.push(artist);
            }
            list.into_iter()
                .map(|a| Pick {
                    short: short_id(&a.id).to_string(),
                    name: a.attributes.name,
                    sub: String::new(),
                    year: String::new(),
                    genre: joined(&a.attributes.genres),
                    tracks: String::new(),
                    label: String::new(),
                    code: String::new(),
                    art: art_url(a.attributes.artwork_url.as_deref()),
                    current: current.as_deref() == Some(a.id.as_str()),
                    id: a.id,
                })
                .collect::<Vec<_>>()
        };
        let mut seen = std::collections::HashSet::new();
        picks.retain(|p| seen.insert(p.id.clone()));
        Ok(picks)
    })
    .await
    .map_err(|_| fail(crate::error::ApiError::generic("internal server error")))?
    .unwrap_or_default();
    Ok(MatchChoice { me, found, picks })
}

#[page("/library/candidates/album")]
async fn candidates_album_page(cx: &Cx) -> Result<impl View> {
    let notice = notice_of(cx)?;
    let choice = match_page(cx, "album").await?;
    Ok(view! { match_view(choice: choice, notice: notice) })
}

#[page("/library/candidates/artist")]
async fn candidates_artist_page(cx: &Cx) -> Result<impl View> {
    let notice = notice_of(cx)?;
    let choice = match_page(cx, "artist").await?;
    Ok(view! { match_view(choice: choice, notice: notice) })
}

#[component]
async fn match_view(choice: MatchChoice, notice: Option<Notice>) -> Result<impl View> {
    let found = choice.found.clone();
    let big = format!("/art?id={}&size=480", escape(&found.id));
    let round = if found.kind == "artist" { " round" } else { "" };
    let thumb = format!("thumb big{round}");
    let row_thumb = format!("thumb{round}");
    let matched = match &found.omm_id {
        Some(id) => format!("matched: {}", short_id(id)),
        None => "not matched".to_string(),
    };
    let pick_action = format!("/library/pick/{}", found.kind);
    let clear_action = format!("/library/clear/{}", found.kind);
    let label = format!("omm {} id", found.kind);
    let example = "1234567890".to_string();
    Ok(view! {
        shell(
            page: Some(Page::Library),
            name: choice.me.name.as_str(),
            me: Some(choice.me.clone()),
            notice: notice,
            <section class="panel wide">
                <div class="live" data-live="match" data-topic="1">
                <div class="album-head">
                    <span class=(thumb.as_str())>
                        if found.art {
                            <img class="cover" src=(big.as_str()) alt="">
                        } else {
                            <span class="cover"></span>
                        }
                    </span>
                    <div class="text">
                        <span class="kind">(found.kind)</span>
                        <strong class="title">(found.title.as_str())</strong>
                        if !found.sub.is_empty() {
                            <span class="muted">(found.sub.as_str())</span>
                        }
                        <span class="muted">(matched.as_str())</span>
                        <nav class="acts">
                            if found.omm_id.is_some() {
                                <form method="post" action=(clear_action.as_str())>
                                    <input type="hidden" name="id" value=(found.id.as_str())>
                                    <button class="ghost danger">"clear match"</button>
                                </form>
                            }
                        </nav>
                    </div>
                </div>
                <dl class="facts">
                    for (k, v) in found.facts.iter() {
                        <div><dt>(k.as_str())</dt><dd>(v.as_str())</dd></div>
                    }
                </dl>
                </div>
                <span class="caption">"search results"</span>
                <div class="list">
                    <div class="row head">
                        <span class="c-cover">"#"</span>
                        <span class="main">"name"</span>
                        if found.kind == "album" { <span class="c-year">"year"</span> }
                        if found.kind == "album" { <span class="c-year wide-only">"songs"</span> }
                        <span class="c-genre">"genre"</span>
                        if found.kind == "album" { <span class="c-label">"label"</span> }
                        if found.kind == "album" { <span class="c-code">"upc"</span> }
                        <span class="c-id">"id"</span>
                        <span class="c-act"></span>
                    </div>
                    <div class="rows">
                        for c in choice.picks.iter() {
                            <form method="post" action=(pick_action.as_str()) class="row">
                                <input type="hidden" name="id" value=(found.id.as_str())>
                                <input type="hidden" name="omm" value=(c.id.as_str())>
                                <span class="c-cover">
                                    <span class=(row_thumb.as_str())>candidate_cover(src: c.art.as_str())</span>
                                </span>
                                <span class="main">
                                    <strong>(c.name.as_str())</strong>
                                    <span class="muted">(c.sub.as_str())</span>
                                </span>
                                if found.kind == "album" { <span class="c-year muted">(c.year.as_str())</span> }
                                if found.kind == "album" { <span class="c-year wide-only muted">(c.tracks.as_str())</span> }
                                <span class="c-genre muted">(c.genre.as_str())</span>
                                if found.kind == "album" { <span class="c-label muted">(c.label.as_str())</span> }
                                if found.kind == "album" { <span class="c-code muted">(c.code.as_str())</span> }
                                <span class="c-id muted">(c.short.as_str())</span>
                                <span class="c-act"><button>if c.current { "keep" } else { "use" }</button></span>
                            </form>
                        }
                    </div>
                </div>
                if choice.picks.is_empty() {
                    <p class="muted">"nothing found"</p>
                }
                <span class="caption">"or paste an id"</span>
                <form method="post" action=(pick_action.as_str())>
                    <input type="hidden" name="id" value=(found.id.as_str())>
                    field(label: label.as_str(), name: "omm", kind: "text", auto: "off", mode: "text", placeholder: example.as_str())
                    <button>"use id"</button>
                </form>
            </section>
        )
    })
}

async fn pick_entity(cx: &Cx, form: PickForm, kind: &'static str) -> Result<SeeOther> {
    let me = require_role(cx, User::can_catalogue).await?;
    let st = state(cx);
    let Some(client) = st.omm.clone() else {
        return Ok(back(Page::Library, Notice::CatalogueOff));
    };
    let wanted = full_id(&form.omm, kind);
    if !omm::valid_id(&wanted, kind) {
        return Ok(back(Page::Library, Notice::Invalid));
    }
    let (uid, id) = (me.id.clone(), form.id.clone());
    let allowed = st
        .db
        .run(move |c| {
            Ok(if kind == "album" {
                libraries::can_access_album(c, &uid, &id)?
            } else {
                libraries::can_access_artist(c, &uid, &id)?
            })
        })
        .await
        .map_err(fail)?;
    if !allowed {
        return Ok(back(Page::Library, Notice::Invalid));
    }
    let id = form.id;
    if kind == "album" {
        let fetched = tokio::task::spawn_blocking(move || client.catalog_album(&wanted))
            .await
            .map_err(|_| fail(crate::error::ApiError::generic("internal server error")))?;
        let Ok(Some(found)) = fetched else {
            return Ok(back(Page::Library, Notice::Invalid));
        };
        st.db
            .run(move |c| Ok(omm::store_album_match(c, &id, &found)?))
            .await
            .map_err(fail)?;
    } else {
        let fetched = tokio::task::spawn_blocking(move || client.catalog_artist(&wanted))
            .await
            .map_err(|_| fail(crate::error::ApiError::generic("internal server error")))?;
        let Ok(Some(found)) = fetched else {
            return Ok(back(Page::Library, Notice::Invalid));
        };
        st.db
            .run(move |c| Ok(omm::store_artist_match(c, &id, &found)?))
            .await
            .map_err(fail)?;
    }
    st.live.notify_topics(crate::live::Live::LIBRARY);
    if let Some(handle) = &st.omm_handle {
        handle.wake();
    }
    Ok(back(Page::Library, Notice::CatalogueMatched))
}

async fn clear_entity(cx: &Cx, form: ClearForm, kind: &'static str) -> Result<SeeOther> {
    let me = require_role(cx, User::can_catalogue).await?;
    let cleared = state(cx)
        .db
        .run(move |c| {
            if kind == "album" {
                if !libraries::can_access_album(c, &me.id, &form.id)? {
                    return Ok(false);
                }
                let mut stmt = c.prepare(
                    "SELECT id, path FROM songs WHERE album_id = ?1 ORDER BY disc, track, path",
                )?;
                let songs: Vec<(String, String)> = stmt
                    .query_map([&form.id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                let files: Vec<_> = songs
                    .into_iter()
                    .map(|(id, path)| {
                        (
                            id,
                            crate::scanner::tags::read_track(std::path::Path::new(&path)).ok(),
                        )
                    })
                    .collect();
                omm::clear_album_match(c, &form.id, &files)?;
            } else {
                if !libraries::can_access_artist(c, &me.id, &form.id)? {
                    return Ok(false);
                }
                omm::clear_artist_match(c, &form.id)?;
            }
            Ok(true)
        })
        .await
        .map_err(fail)?;
    state(cx).live.notify_topics(crate::live::Live::LIBRARY);
    Ok(back(
        Page::Library,
        if cleared {
            Notice::CatalogueCleared
        } else {
            Notice::Invalid
        },
    ))
}

#[route(POST "/library/pick/album")]
async fn pick_album(cx: &Cx, Form(form): Form<PickForm>) -> Result<SeeOther> {
    pick_entity(cx, form, "album").await
}

#[route(POST "/library/pick/artist")]
async fn pick_artist(cx: &Cx, Form(form): Form<PickForm>) -> Result<SeeOther> {
    pick_entity(cx, form, "artist").await
}

#[route(POST "/library/clear/album")]
async fn clear_album(cx: &Cx, Form(form): Form<ClearForm>) -> Result<SeeOther> {
    clear_entity(cx, form, "album").await
}

#[route(POST "/library/clear/artist")]
async fn clear_artist(cx: &Cx, Form(form): Form<ClearForm>) -> Result<SeeOther> {
    clear_entity(cx, form, "artist").await
}

#[route(GET "/player.js")]
async fn script() -> Result<([(HeaderName, HeaderValue); 2], &'static str)> {
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/javascript; charset=utf-8"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=86400"),
            ),
        ],
        SCRIPT,
    ))
}

pub fn routes(b: RouterBuilder) -> RouterBuilder {
    b.page(library_page)
        .page(candidates_page)
        .page(candidates_album_page)
        .page(candidates_artist_page)
        .route(pick_album)
        .route(pick_artist)
        .route(clear_album)
        .route(clear_artist)
        .route(pick)
        .route(clear)
        .route(script)
}
