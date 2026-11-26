use crate::components::library::album_head::album_head;
use crate::components::library::album_tile::album_tile;
use crate::components::library::artist_head::artist_head;
use crate::components::library::artist_tile::artist_tile;
use crate::components::library::song_row::song_row;
use crate::components::library::sort_head::sort_head;
use crate::components::library::track_row::track_row;
use crate::components::playlist::adding_note::adding_note;
use crate::components::playlist::create::playlist_create;
use crate::components::playlist::head::playlist_head;
use crate::components::playlist::tile::playlist_tile;
use crate::components::ui::icon::DURATION_ICON;
use crate::components::ui::nav::{Page, jump};
use crate::layouts::shell::shell;
use crate::server::api::rest::playlists as store;
use crate::server::utils::db::libraries::{artist_visible, visible};
use crate::server::utils::db::users::User;
use crate::server::utils::format::{clock, escape};
use crate::server::utils::model::{
    AlbumRow, SongRow, album_artist_map, album_genre_map, albums_by_ids, albums_where,
    artists_by_ids, artists_where, load_extras, songs_by_ids, songs_where, sv,
};
use crate::server::utils::notice::Notice;
use crate::server::utils::playlists::{Playlist, Shelf, playlist};
use crate::server::utils::search;
use crate::server::utils::session::{fail, state};
use std::collections::HashMap;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::icon::icon;
use topcoat::router::error::{SeeOther, see_other};
use topcoat::router::{query_params, route};
use topcoat::view::{View, component, view};

pub const CHUNK: i64 = 60;

pub const MAX_CHUNK: i64 = 200;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View_ {
    Albums,
    Songs,
    Artists,
    Playlists,
}

pub type Names = Vec<(String, String)>;

#[query_params(error = bad_request)]
pub struct Query {
    pub q: Option<String>,
    pub view: Option<String>,
    pub status: Option<String>,
    pub album: Option<String>,
    pub artist: Option<String>,
    pub playlist: Option<String>,
    pub add: Option<String>,
    pub sort: Option<String>,
    pub dir: Option<String>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Clone)]
pub struct Song {
    pub id: String,
    pub album_id: String,
    pub cover: bool,
    pub title: String,
    pub artist: String,
    pub artists: Names,
    pub album: String,
    pub length: String,
    pub gain: String,
    pub omm_id: Option<String>,
    pub number: String,
    pub year: String,
    pub format: String,
}

#[derive(Clone)]
pub struct Album {
    pub id: String,
    pub cover: bool,
    pub title: String,
    pub artists: Names,
    pub detail: String,
    pub genres: String,
}

#[derive(Clone)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub image: bool,
}

#[derive(Clone)]
pub struct Counts {
    pub total: i64,
    pub matched: i64,
}

#[derive(Clone)]
pub struct Board {
    pub me: User,
    pub q: String,
    pub albums_view: bool,
    pub artists_view: bool,
    pub lists_view: bool,
    pub status: String,
    pub sort: String,
    pub desc: bool,
    pub total: i64,
    pub offset: i64,
    pub chunk: i64,
    pub heading: Option<Album>,
    pub person: Option<Artist>,
    pub songs: Vec<Song>,
    pub albums: Vec<Album>,
    pub people: Vec<Artist>,
    pub shelf: Shelf,
    pub adding: Option<Playlist>,
    pub counts: Counts,
    pub lookup: bool,
}

pub fn song(row: SongRow, mut artists: Names) -> Song {
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

pub fn album(row: &AlbumRow, artists: &HashMap<String, Vec<(String, String)>>) -> Album {
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

pub struct Link<'a> {
    pub q: &'a str,
    pub view: &'a str,
    pub status: &'a str,
    pub album: &'a str,
    pub sort: &'a str,
    pub dir: &'a str,
}

impl Link<'_> {
    fn href(&self) -> String {
        let mut out = format!("/?view={}&status={}", self.view, self.status);
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

pub async fn library_board(cx: &Cx, me: User) -> Result<Board> {
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
                        .map(|r| playlist(&r, &owner)))
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
                    shelf.open = Some(playlist(&row, &viewer));
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
                    shelf.lists = rows.iter().map(|r| playlist(r, &viewer)).collect();
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
                            "title" => "al.title COLLATE NOCASE",
                            "year" => "al.year DESC, al.title COLLATE NOCASE",
                            _ => "al.created_at DESC, al.title COLLATE NOCASE",
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
    Ok(board)
}

pub fn song_counts(
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

pub fn full_id(input: &str, kind: &str) -> String {
    let input = input.trim();
    if !input.is_empty() && input.bytes().all(|b| b.is_ascii_digit()) {
        format!("omm:{kind}:{input}")
    } else {
        input.to_string()
    }
}

pub fn short_id(id: &str) -> &str {
    id.rsplit(':').next().unwrap_or(id)
}

pub fn at(q: &str, view: &str, status: &str, album: &str) -> String {
    at_sorted(q, view, status, album, "", "")
}

pub fn at_sorted(q: &str, view: &str, status: &str, album: &str, sort: &str, dir: &str) -> String {
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

pub fn next_sort(board: &Board, key: &str) -> (String, String) {
    if board.sort != key {
        (key.to_string(), "asc".to_string())
    } else if !board.desc {
        (key.to_string(), "desc".to_string())
    } else {
        (String::new(), String::new())
    }
}

#[component]
pub async fn library_view(cx: &Cx, board: Board, notice: Option<Notice>) -> Result<impl View> {
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
                                jump(href: href.clone(), label: *name, on: board.sort == *name || (board.sort.is_empty() && *name == "added"))
                            }
                        }
                    </nav>
                    <form class="searchbar" method="get" action="/" data-search="">
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
                                <span class="c-time"><span class="muted">icon(data: DURATION_ICON)</span></span>
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
                                    <span class="c-time"><span class="muted">icon(data: DURATION_ICON)</span></span>
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
                                <span class="c-time"><span class="muted">icon(data: DURATION_ICON)</span></span>
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

#[route(GET "/library")]
pub async fn legacy(cx: &Cx) -> Result<SeeOther> {
    Ok(see_other(match topcoat::router::request::uri(cx).query() {
        Some(q) => format!("/?{q}"),
        None => "/".to_string(),
    }))
}
