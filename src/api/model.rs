use std::collections::HashMap;

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, Row, params_from_iter};
use serde_json::{Map, Value, json};

use crate::error::ApiError;
use crate::util::{date_obj, iso, placeholders};

pub const IGNORED_ARTICLES: &[&str] = &[
    "The", "El", "La", "Los", "Las", "Le", "Les", "Os", "As", "O", "A", "An",
];
const CHUNK: usize = 400;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ItemKind {
    Song,
    Album,
    Artist,
    Playlist,
}

pub fn kind_of(conn: &Connection, id: &str) -> rusqlite::Result<Option<ItemKind>> {
    use rusqlite::OptionalExtension;
    let tag: Option<String> = conn
        .query_row(
            "SELECT 's' FROM songs WHERE id = ?1 UNION ALL SELECT 'a' FROM albums WHERE id = ?1
             UNION ALL SELECT 'r' FROM artists WHERE id = ?1 UNION ALL SELECT 'p' FROM playlists WHERE id = ?1
             LIMIT 1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(tag.and_then(|t| match t.as_str() {
        "s" => Some(ItemKind::Song),
        "a" => Some(ItemKind::Album),
        "r" => Some(ItemKind::Artist),
        "p" => Some(ItemKind::Playlist),
        _ => None,
    }))
}

pub fn sv(s: &str) -> Sql {
    Sql::Text(s.to_string())
}

pub fn iv(i: i64) -> Sql {
    Sql::Integer(i)
}

#[derive(Clone, Debug)]
pub struct SongRow {
    pub id: String,
    pub title: String,
    pub album_id: String,
    pub album: String,
    pub track: Option<i64>,
    pub disc: Option<i64>,
    pub year: Option<i64>,
    pub duration_ms: i64,
    pub bitrate: i64,
    pub sample_rate: i64,
    pub channels: i64,
    pub bit_depth: i64,
    pub size: i64,
    pub suffix: String,
    pub content_type: String,
    pub path: String,
    pub cover_hash: Option<String>,
    pub album_cover: Option<String>,
    pub display_artist: String,
    pub mbid: Option<String>,
    pub isrc: Option<String>,
    pub bpm: Option<i64>,
    pub comment: Option<String>,
    pub composer: Option<String>,
    pub rg_track_gain: Option<f64>,
    pub rg_track_peak: Option<f64>,
    pub rg_album_gain: Option<f64>,
    pub rg_album_peak: Option<f64>,
    pub created_at: i64,
    pub omm_id: Option<String>,
    pub starred_at: Option<i64>,
    pub rating: Option<i64>,
    pub play_count: i64,
    pub last_played: Option<i64>,
}

pub const SONG_SELECT: &str = "SELECT s.id, s.title, s.album_id, al.title, s.track, s.disc, s.year,
    s.duration_ms, s.bitrate, s.sample_rate, s.channels, s.bit_depth, s.size, s.suffix, s.content_type, music_rel(s.path),
    s.cover_hash, al.cover_hash, s.display_artist, s.mbid, s.isrc, s.bpm, s.comment, s.composer,
    s.rg_track_gain, s.rg_track_peak, s.rg_album_gain, s.rg_album_peak, s.created_at, s.omm_id,
    st.starred_at, rt.rating, ps.play_count, ps.last_played
    FROM songs s JOIN albums al ON al.id = s.album_id
    LEFT JOIN starred st ON st.item_id = s.id AND st.user_id = ?1
    LEFT JOIN ratings rt ON rt.item_id = s.id AND rt.user_id = ?1
    LEFT JOIN play_stats ps ON ps.song_id = s.id AND ps.user_id = ?1";

fn map_song(r: &Row) -> rusqlite::Result<SongRow> {
    Ok(SongRow {
        id: r.get(0)?,
        title: r.get(1)?,
        album_id: r.get(2)?,
        album: r.get(3)?,
        track: r.get(4)?,
        disc: r.get(5)?,
        year: r.get(6)?,
        duration_ms: r.get(7)?,
        bitrate: r.get(8)?,
        sample_rate: r.get(9)?,
        channels: r.get(10)?,
        bit_depth: r.get(11)?,
        size: r.get(12)?,
        suffix: r.get(13)?,
        content_type: r.get(14)?,
        path: r.get(15)?,
        cover_hash: r.get(16)?,
        album_cover: r.get(17)?,
        display_artist: r.get(18)?,
        mbid: r.get(19)?,
        isrc: r.get(20)?,
        bpm: r.get(21)?,
        comment: r.get(22)?,
        composer: r.get(23)?,
        rg_track_gain: r.get(24)?,
        rg_track_peak: r.get(25)?,
        rg_album_gain: r.get(26)?,
        rg_album_peak: r.get(27)?,
        created_at: r.get(28)?,
        omm_id: r.get(29)?,
        starred_at: r.get(30)?,
        rating: r.get(31)?,
        play_count: r.get::<_, Option<i64>>(32)?.unwrap_or(0),
        last_played: r.get(33)?,
    })
}

pub fn songs_where(
    conn: &Connection,
    uid: &str,
    tail: &str,
    args: Vec<Sql>,
) -> Result<Vec<SongRow>, ApiError> {
    let mut all = vec![sv(uid)];
    all.extend(args);
    let mut stmt = conn.prepare(&format!("{SONG_SELECT} {tail}"))?;
    let rows = stmt
        .query_map(params_from_iter(all), map_song)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn songs_by_ids(
    conn: &Connection,
    uid: &str,
    ids: &[String],
) -> Result<Vec<SongRow>, ApiError> {
    let mut found: HashMap<String, SongRow> = HashMap::new();
    for chunk in ids.chunks(CHUNK) {
        let tail = format!("WHERE s.id IN ({})", placeholders(chunk.len()));
        for row in songs_where(conn, uid, &tail, chunk.iter().map(|i| sv(i)).collect())? {
            found.insert(row.id.clone(), row);
        }
    }
    Ok(ids.iter().filter_map(|id| found.get(id).cloned()).collect())
}

#[derive(Default)]
pub struct Extras {
    pub artists: HashMap<String, Vec<(String, String)>>,
    pub genres: HashMap<String, Vec<String>>,
    pub album_artists: HashMap<String, Vec<(String, String)>>,
}

fn pairs(
    conn: &Connection,
    sql_prefix: &str,
    sql_suffix: &str,
    ids: &[String],
) -> rusqlite::Result<Vec<(String, String, String)>> {
    let mut out = Vec::new();
    for chunk in ids.chunks(CHUNK) {
        let sql = format!("{sql_prefix} ({}) {sql_suffix}", placeholders(chunk.len()));
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(chunk.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

pub fn album_artist_map(
    conn: &Connection,
    album_ids: &[String],
) -> rusqlite::Result<HashMap<String, Vec<(String, String)>>> {
    let mut map: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (album, id, name) in pairs(
        conn,
        "SELECT aa.album_id, a.id, a.name FROM album_artists aa JOIN artists a ON a.id = aa.artist_id WHERE aa.album_id IN",
        "ORDER BY aa.position",
        album_ids,
    )? {
        map.entry(album).or_default().push((id, name));
    }
    Ok(map)
}

pub fn album_genre_map(
    conn: &Connection,
    album_ids: &[String],
) -> rusqlite::Result<HashMap<String, Vec<String>>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (album, name, _) in pairs(
        conn,
        "SELECT s.album_id, g.name, CAST(COUNT(*) AS TEXT) FROM song_genres sg JOIN genres g ON g.id = sg.genre_id
         JOIN songs s ON s.id = sg.song_id WHERE s.album_id IN",
        "GROUP BY s.album_id, g.id ORDER BY COUNT(*) DESC, g.name",
        album_ids,
    )? {
        map.entry(album).or_default().push(name);
    }
    Ok(map)
}

pub fn load_extras(conn: &Connection, songs: &[SongRow]) -> rusqlite::Result<Extras> {
    let song_ids: Vec<String> = songs.iter().map(|s| s.id.clone()).collect();
    let mut album_ids: Vec<String> = songs.iter().map(|s| s.album_id.clone()).collect();
    album_ids.sort();
    album_ids.dedup();

    let mut ex = Extras::default();
    for (song, id, name) in pairs(
        conn,
        "SELECT sa.song_id, a.id, a.name FROM song_artists sa JOIN artists a ON a.id = sa.artist_id WHERE sa.song_id IN",
        "ORDER BY sa.position",
        &song_ids,
    )? {
        ex.artists.entry(song).or_default().push((id, name));
    }
    for (song, name, _) in pairs(
        conn,
        "SELECT sg.song_id, g.name, '' FROM song_genres sg JOIN genres g ON g.id = sg.genre_id WHERE sg.song_id IN",
        "ORDER BY sg.position",
        &song_ids,
    )? {
        ex.genres.entry(song).or_default().push(name);
    }
    ex.album_artists = album_artist_map(conn, &album_ids)?;
    Ok(ex)
}

fn put(m: &mut Map<String, Value>, k: &str, v: impl Into<Value>) {
    m.insert(k.to_string(), v.into());
}

fn put_opt<T: Into<Value>>(m: &mut Map<String, Value>, k: &str, v: Option<T>) {
    if let Some(v) = v {
        m.insert(k.to_string(), v.into());
    }
}

fn names_json(list: &[(String, String)]) -> Vec<Value> {
    list.iter()
        .map(|(id, name)| json!({"id": id, "name": name}))
        .collect()
}

fn join_names(list: &[(String, String)]) -> String {
    list.iter()
        .map(|(_, n)| n.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn child_json(s: &SongRow, ex: &Extras) -> Value {
    let artists = ex.artists.get(&s.id).cloned().unwrap_or_default();
    let album_artists = ex
        .album_artists
        .get(&s.album_id)
        .cloned()
        .unwrap_or_default();
    let genres = ex.genres.get(&s.id).cloned().unwrap_or_default();

    let mut m = Map::new();
    put(&mut m, "id", s.id.clone());
    put(&mut m, "parent", s.album_id.clone());
    put(&mut m, "isDir", false);
    put(&mut m, "title", s.title.clone());
    put(&mut m, "album", s.album.clone());
    put(&mut m, "artist", s.display_artist.clone());
    put_opt(&mut m, "track", s.track);
    put_opt(&mut m, "year", s.year);
    put_opt(&mut m, "genre", genres.first().cloned());
    let cover = match (&s.cover_hash, &s.album_cover) {
        (Some(c), Some(a)) if c == a => Some(s.album_id.clone()),
        (Some(_), _) => Some(s.id.clone()),
        (None, Some(_)) => Some(s.album_id.clone()),
        (None, None) => None,
    };
    put_opt(&mut m, "coverArt", cover);
    put(&mut m, "size", s.size);
    put(&mut m, "contentType", s.content_type.clone());
    put(&mut m, "suffix", s.suffix.clone());
    put(&mut m, "duration", s.duration_ms / 1000);
    put(&mut m, "bitRate", s.bitrate);
    put_opt(&mut m, "bitDepth", (s.bit_depth > 0).then_some(s.bit_depth));
    put_opt(
        &mut m,
        "samplingRate",
        (s.sample_rate > 0).then_some(s.sample_rate),
    );
    put_opt(
        &mut m,
        "channelCount",
        (s.channels > 0).then_some(s.channels),
    );
    put(&mut m, "path", s.path.as_str());
    put(&mut m, "playCount", s.play_count);
    put_opt(&mut m, "played", s.last_played.map(iso));
    put_opt(&mut m, "discNumber", s.disc);
    put(&mut m, "created", iso(s.created_at));
    put_opt(&mut m, "starred", s.starred_at.map(iso));
    put_opt(&mut m, "userRating", s.rating);
    put(&mut m, "albumId", s.album_id.clone());
    put_opt(&mut m, "artistId", artists.first().map(|a| a.0.clone()));
    put(&mut m, "type", "music");
    put(&mut m, "isVideo", false);
    put(&mut m, "mediaType", "song");
    put_opt(&mut m, "bpm", s.bpm);
    put_opt(&mut m, "comment", s.comment.clone());
    put_opt(&mut m, "musicBrainzId", s.mbid.clone());
    put_opt(&mut m, "ommId", s.omm_id.clone());
    put_opt(&mut m, "displayComposer", s.composer.clone());
    if let Some(isrc) = &s.isrc {
        put(&mut m, "isrc", json!([isrc]));
    }
    if !genres.is_empty() {
        put(
            &mut m,
            "genres",
            genres
                .iter()
                .map(|g| json!({"name": g}))
                .collect::<Vec<_>>(),
        );
    }
    if !artists.is_empty() {
        put(&mut m, "artists", names_json(&artists));
    }
    put(&mut m, "displayArtist", s.display_artist.clone());
    if !album_artists.is_empty() {
        put(&mut m, "albumArtists", names_json(&album_artists));
        put(&mut m, "displayAlbumArtist", join_names(&album_artists));
    }
    let mut rg = Map::new();
    put_opt(&mut rg, "trackGain", s.rg_track_gain);
    put_opt(&mut rg, "albumGain", s.rg_album_gain);
    put_opt(&mut rg, "trackPeak", s.rg_track_peak);
    put_opt(&mut rg, "albumPeak", s.rg_album_peak);
    if !rg.is_empty() {
        put(&mut m, "replayGain", Value::Object(rg));
    }
    Value::Object(m)
}

pub fn songs_json(conn: &Connection, rows: &[SongRow]) -> Result<Vec<Value>, ApiError> {
    let ex = load_extras(conn, rows)?;
    Ok(rows.iter().map(|r| child_json(r, &ex)).collect())
}

#[derive(Clone, Debug)]
pub struct AlbumRow {
    pub id: String,
    pub title: String,
    pub year: Option<i64>,
    pub date: Option<String>,
    pub original_date: Option<String>,
    pub is_compilation: bool,
    pub cover_hash: Option<String>,
    pub mbid: Option<String>,
    pub label: Option<String>,
    pub created_at: i64,
    pub omm_id: Option<String>,
    pub song_count: i64,
    pub duration_ms: i64,
    pub play_count: i64,
    pub last_played: Option<i64>,
    pub starred_at: Option<i64>,
    pub rating: Option<i64>,
}

pub const ALBUM_SELECT: &str =
    "SELECT al.id, al.title, al.year, al.date, al.original_date, al.is_compilation,
    al.cover_hash, al.mbid, al.label, al.created_at, al.omm_id,
    (SELECT COUNT(*) FROM songs s WHERE s.album_id = al.id),
    (SELECT COALESCE(SUM(s.duration_ms), 0) FROM songs s WHERE s.album_id = al.id),
    (SELECT COALESCE(SUM(p.play_count), 0) FROM play_stats p JOIN songs s ON s.id = p.song_id
        WHERE s.album_id = al.id AND p.user_id = ?1),
    (SELECT MAX(p.last_played) FROM play_stats p JOIN songs s ON s.id = p.song_id
        WHERE s.album_id = al.id AND p.user_id = ?1),
    st.starred_at, rt.rating
    FROM albums al
    LEFT JOIN starred st ON st.item_id = al.id AND st.user_id = ?1
    LEFT JOIN ratings rt ON rt.item_id = al.id AND rt.user_id = ?1";

fn map_album(r: &Row) -> rusqlite::Result<AlbumRow> {
    Ok(AlbumRow {
        id: r.get(0)?,
        title: r.get(1)?,
        year: r.get(2)?,
        date: r.get(3)?,
        original_date: r.get(4)?,
        is_compilation: r.get(5)?,
        cover_hash: r.get(6)?,
        mbid: r.get(7)?,
        label: r.get(8)?,
        created_at: r.get(9)?,
        omm_id: r.get(10)?,
        song_count: r.get(11)?,
        duration_ms: r.get(12)?,
        play_count: r.get(13)?,
        last_played: r.get(14)?,
        starred_at: r.get(15)?,
        rating: r.get(16)?,
    })
}

pub fn albums_where(
    conn: &Connection,
    uid: &str,
    tail: &str,
    args: Vec<Sql>,
) -> Result<Vec<AlbumRow>, ApiError> {
    let mut all = vec![sv(uid)];
    all.extend(args);
    let mut stmt = conn.prepare(&format!("{ALBUM_SELECT} {tail}"))?;
    let rows = stmt
        .query_map(params_from_iter(all), map_album)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn albums_by_ids(
    conn: &Connection,
    uid: &str,
    ids: &[String],
) -> Result<Vec<AlbumRow>, ApiError> {
    let mut found: HashMap<String, AlbumRow> = HashMap::new();
    for chunk in ids.chunks(CHUNK) {
        let tail = format!("WHERE al.id IN ({})", placeholders(chunk.len()));
        for row in albums_where(conn, uid, &tail, chunk.iter().map(|i| sv(i)).collect())? {
            found.insert(row.id.clone(), row);
        }
    }
    Ok(ids.iter().filter_map(|id| found.get(id).cloned()).collect())
}

pub struct AlbumExtras {
    pub artists: HashMap<String, Vec<(String, String)>>,
    pub genres: HashMap<String, Vec<String>>,
}

pub fn load_album_extras(conn: &Connection, rows: &[AlbumRow]) -> rusqlite::Result<AlbumExtras> {
    let ids: Vec<String> = rows.iter().map(|a| a.id.clone()).collect();
    Ok(AlbumExtras {
        artists: album_artist_map(conn, &ids)?,
        genres: album_genre_map(conn, &ids)?,
    })
}

pub fn album_json(a: &AlbumRow, ex: &AlbumExtras) -> Value {
    let artists = ex.artists.get(&a.id).cloned().unwrap_or_default();
    let genres = ex.genres.get(&a.id).cloned().unwrap_or_default();
    let display = join_names(&artists);

    let mut m = Map::new();
    put(&mut m, "id", a.id.clone());
    put(&mut m, "name", a.title.clone());
    put(&mut m, "artist", display.clone());
    put_opt(&mut m, "artistId", artists.first().map(|x| x.0.clone()));
    put_opt(
        &mut m,
        "coverArt",
        a.cover_hash.as_ref().map(|_| a.id.clone()),
    );
    put(&mut m, "songCount", a.song_count);
    put(&mut m, "duration", a.duration_ms / 1000);
    put(&mut m, "playCount", a.play_count);
    put(&mut m, "created", iso(a.created_at));
    put_opt(&mut m, "starred", a.starred_at.map(iso));
    put_opt(&mut m, "year", a.year);
    put_opt(&mut m, "genre", genres.first().cloned());
    put_opt(&mut m, "userRating", a.rating);
    put_opt(&mut m, "played", a.last_played.map(iso));
    if let Some(label) = &a.label {
        put(&mut m, "recordLabels", json!([{"name": label}]));
    }
    put_opt(&mut m, "musicBrainzId", a.mbid.clone());
    put_opt(&mut m, "ommId", a.omm_id.clone());
    if !genres.is_empty() {
        put(
            &mut m,
            "genres",
            genres
                .iter()
                .map(|g| json!({"name": g}))
                .collect::<Vec<_>>(),
        );
    }
    if !artists.is_empty() {
        put(&mut m, "artists", names_json(&artists));
    }
    put(&mut m, "displayArtist", display);
    put(&mut m, "isCompilation", a.is_compilation);
    if a.is_compilation {
        put(&mut m, "releaseTypes", json!(["compilation"]));
    }
    put_opt(
        &mut m,
        "originalReleaseDate",
        a.original_date.as_deref().and_then(date_obj),
    );
    put_opt(&mut m, "releaseDate", a.date.as_deref().and_then(date_obj));
    Value::Object(m)
}

pub fn albums_json(conn: &Connection, rows: &[AlbumRow]) -> Result<Vec<Value>, ApiError> {
    let ex = load_album_extras(conn, rows)?;
    Ok(rows.iter().map(|r| album_json(r, &ex)).collect())
}

pub fn album_child_json(a: &AlbumRow, ex: &AlbumExtras) -> Value {
    let artists = ex.artists.get(&a.id).cloned().unwrap_or_default();
    let genres = ex.genres.get(&a.id).cloned().unwrap_or_default();
    let mut m = Map::new();
    put(&mut m, "id", a.id.clone());
    put_opt(&mut m, "parent", artists.first().map(|x| x.0.clone()));
    put(&mut m, "isDir", true);
    put(&mut m, "title", a.title.clone());
    put(&mut m, "album", a.title.clone());
    put(&mut m, "artist", join_names(&artists));
    put_opt(&mut m, "year", a.year);
    put_opt(&mut m, "genre", genres.first().cloned());
    put_opt(
        &mut m,
        "coverArt",
        a.cover_hash.as_ref().map(|_| a.id.clone()),
    );
    put(&mut m, "playCount", a.play_count);
    put(&mut m, "created", iso(a.created_at));
    put_opt(&mut m, "starred", a.starred_at.map(iso));
    put_opt(&mut m, "userRating", a.rating);
    put_opt(&mut m, "artistId", artists.first().map(|x| x.0.clone()));
    put_opt(&mut m, "ommId", a.omm_id.clone());
    Value::Object(m)
}

#[derive(Clone, Debug)]
pub struct ArtistRow {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub mbid: Option<String>,
    pub omm_id: Option<String>,
    pub album_count: i64,
    pub starred_at: Option<i64>,
    pub rating: Option<i64>,
    pub has_image: bool,
}

pub const ARTIST_SELECT: &str = "SELECT ar.id, ar.name, ar.sort_name, ar.mbid, ar.omm_id,
    (SELECT COUNT(*) FROM album_artists aa WHERE aa.artist_id = ar.id), st.starred_at, rt.rating,
    ar.image_path IS NOT NULL
    FROM artists ar
    LEFT JOIN starred st ON st.item_id = ar.id AND st.user_id = ?1
    LEFT JOIN ratings rt ON rt.item_id = ar.id AND rt.user_id = ?1";

fn map_artist(r: &Row) -> rusqlite::Result<ArtistRow> {
    Ok(ArtistRow {
        id: r.get(0)?,
        name: r.get(1)?,
        sort_name: r.get(2)?,
        mbid: r.get(3)?,
        omm_id: r.get(4)?,
        album_count: r.get(5)?,
        starred_at: r.get(6)?,
        rating: r.get(7)?,
        has_image: r.get(8)?,
    })
}

pub fn artists_where(
    conn: &Connection,
    uid: &str,
    tail: &str,
    args: Vec<Sql>,
) -> Result<Vec<ArtistRow>, ApiError> {
    let mut all = vec![sv(uid)];
    all.extend(args);
    let mut stmt = conn.prepare(&format!("{ARTIST_SELECT} {tail}"))?;
    let rows = stmt
        .query_map(params_from_iter(all), map_artist)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn artists_by_ids(
    conn: &Connection,
    uid: &str,
    ids: &[String],
) -> Result<Vec<ArtistRow>, ApiError> {
    let mut found: HashMap<String, ArtistRow> = HashMap::new();
    for chunk in ids.chunks(CHUNK) {
        let tail = format!("WHERE ar.id IN ({})", placeholders(chunk.len()));
        for row in artists_where(conn, uid, &tail, chunk.iter().map(|i| sv(i)).collect())? {
            found.insert(row.id.clone(), row);
        }
    }
    Ok(ids.iter().filter_map(|id| found.get(id).cloned()).collect())
}

pub fn artist_json(a: &ArtistRow) -> Value {
    let mut m = Map::new();
    put(&mut m, "id", a.id.clone());
    put(&mut m, "name", a.name.clone());
    put_opt(
        &mut m,
        "coverArt",
        (a.album_count > 0 || a.has_image).then(|| a.id.clone()),
    );
    put(&mut m, "albumCount", a.album_count);
    put_opt(&mut m, "starred", a.starred_at.map(iso));
    put_opt(&mut m, "userRating", a.rating);
    put_opt(&mut m, "musicBrainzId", a.mbid.clone());
    put_opt(&mut m, "sortName", a.sort_name.clone());
    put_opt(&mut m, "ommId", a.omm_id.clone());
    put(&mut m, "roles", json!(["artist"]));
    Value::Object(m)
}
