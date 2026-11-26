use crate::server::utils::db::users::User;
use crate::server::utils::state::AppState;

pub struct Song {
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub duration_secs: u32,
}

pub fn load_song(conn: &rusqlite::Connection, id: &str) -> Option<Song> {
    conn.query_row(
        "SELECT s.title, s.display_artist, al.title, s.duration_ms,
                (SELECT a.name FROM song_artists sa JOIN artists a ON a.id = sa.artist_id
                   WHERE sa.song_id = s.id ORDER BY sa.position LIMIT 1)
         FROM songs s JOIN albums al ON al.id = s.album_id WHERE s.id = ?1",
        [id],
        |r| {
            let album: String = r.get(2)?;
            let first: Option<String> = r.get(4)?;
            Ok(Song {
                title: r.get(0)?,
                artist: first.unwrap_or(r.get(1)?),
                album: (!album.starts_with('[')).then_some(album),
                duration_secs: (r.get::<_, i64>(3)? / 1000).max(0) as u32,
            })
        },
    )
    .ok()
}

pub fn forward(st: &AppState, user: &User, ids: Vec<String>, times: Vec<i64>, submission: bool) {
    if !user.scrobbling {
        return;
    }
    super::lastfm::forward(st, user, ids.clone(), times.clone(), submission);
    super::listenbrainz::forward(st, user, ids, times, submission);
}
