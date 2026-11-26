use crate::components::ui::nav::Page;
use crate::pages::library::index::full_id;
use crate::server::utils::db::libraries;
use crate::server::utils::db::users::User;
use crate::server::utils::format::clock;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::omm;
use crate::server::utils::session::{fail, require_role, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::error::SeeOther;

#[derive(Deserialize)]
pub struct PickForm {
    pub id: String,
    pub omm: String,
}

#[derive(Deserialize)]
pub struct ClearForm {
    pub id: String,
}

pub fn fact(out: &mut Vec<(String, String)>, label: &str, value: impl Into<String>) {
    let value = value.into();
    if !value.trim().is_empty() {
        out.push((label.to_string(), value));
    }
}

pub fn names_of(c: &rusqlite::Connection, sql: &str, id: &str) -> rusqlite::Result<String> {
    let mut stmt = c.prepare(sql)?;
    let names: Vec<String> = stmt
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(names.join(", "))
}

pub fn song_facts(c: &rusqlite::Connection, id: &str) -> rusqlite::Result<Vec<(String, String)>> {
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

pub fn album_facts(c: &rusqlite::Connection, id: &str) -> rusqlite::Result<Vec<(String, String)>> {
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

pub fn artist_facts(c: &rusqlite::Connection, id: &str) -> rusqlite::Result<Vec<(String, String)>> {
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

pub fn joined(names: &[String]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for n in names {
        if !seen.contains(&n.as_str()) {
            seen.push(n);
        }
    }
    seen.join(", ")
}

pub fn year_of(date: Option<&str>) -> String {
    date.and_then(|d| d.get(..4)).unwrap_or("").to_string()
}

pub fn art_url(template: Option<&str>) -> String {
    template
        .map(|u| u.replace("{w}", "128").replace("{h}", "128"))
        .unwrap_or_default()
}

pub async fn pick_entity(cx: &Cx, form: PickForm, kind: &'static str) -> Result<SeeOther> {
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
            .map_err(|_| {
                fail(crate::server::utils::error::ApiError::generic(
                    "internal server error",
                ))
            })?;
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
            .map_err(|_| {
                fail(crate::server::utils::error::ApiError::generic(
                    "internal server error",
                ))
            })?;
        let Ok(Some(found)) = fetched else {
            return Ok(back(Page::Library, Notice::Invalid));
        };
        st.db
            .run(move |c| Ok(omm::store_artist_match(c, &id, &found)?))
            .await
            .map_err(fail)?;
    }
    st.live
        .notify_topics(crate::server::utils::live::Live::LIBRARY);
    if let Some(handle) = &st.omm_handle {
        handle.wake();
    }
    Ok(back(Page::Library, Notice::CatalogueMatched))
}

pub async fn clear_entity(cx: &Cx, form: ClearForm, kind: &'static str) -> Result<SeeOther> {
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
                            crate::server::utils::scanner::tags::read_track(std::path::Path::new(
                                &path,
                            ))
                            .ok(),
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
    state(cx)
        .live
        .notify_topics(crate::server::utils::live::Live::LIBRARY);
    Ok(back(
        Page::Library,
        if cleared {
            Notice::CatalogueCleared
        } else {
            Notice::Invalid
        },
    ))
}
