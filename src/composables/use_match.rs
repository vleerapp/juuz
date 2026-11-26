use crate::components::ui::nav::Page;
use crate::pages::library::candidates::index::{CANDIDATES, CandidateQuery};
use crate::pages::library::index::short_id;
use crate::server::utils::catalogue::{album_facts, art_url, artist_facts, joined, year_of};
use crate::server::utils::db::libraries;
use crate::server::utils::db::users::User;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::omm;
use crate::server::utils::session::{fail, require_role, state};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::query_params;

#[derive(Clone)]
pub struct Match {
    pub kind: &'static str,
    pub id: String,
    pub title: String,
    pub sub: String,
    pub art: bool,
    pub omm_id: Option<String>,
    pub facts: Vec<(String, String)>,
}

#[derive(Clone)]
pub struct Pick {
    pub id: String,
    pub short: String,
    pub name: String,
    pub sub: String,
    pub year: String,
    pub genre: String,
    pub tracks: String,
    pub label: String,
    pub code: String,
    pub art: String,
    pub current: bool,
}

#[derive(Clone)]
pub struct MatchChoice {
    pub me: User,
    pub found: Match,
    pub picks: Vec<Pick>,
}

pub async fn match_page(cx: &Cx, kind: &'static str) -> Result<MatchChoice> {
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
    .map_err(|_| {
        fail(crate::server::utils::error::ApiError::generic(
            "internal server error",
        ))
    })?
    .unwrap_or_default();
    Ok(MatchChoice { me, found, picks })
}
