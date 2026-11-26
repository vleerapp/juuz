use crate::server::utils::db::libraries;
use crate::server::utils::db::libraries::Library;
use topcoat::Result;

#[derive(Clone)]
pub struct Row {
    pub library: Library,
    pub songs: i64,
    pub last_seen: Option<i64>,
}

pub fn rows(c: &rusqlite::Connection) -> Result<Vec<Row>, crate::server::utils::error::ApiError> {
    let mut stmt = c.prepare(
        "SELECT library_id, COUNT(*), MAX(indexed_at) / 1000 FROM songs GROUP BY library_id",
    )?;
    let counts: std::collections::HashMap<i64, (i64, Option<i64>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(libraries::list(c)?
        .into_iter()
        .map(|library| {
            let (songs, last_seen) = counts.get(&library.id).copied().unwrap_or((0, None));
            Row {
                library,
                songs,
                last_seen,
            }
        })
        .collect())
}
