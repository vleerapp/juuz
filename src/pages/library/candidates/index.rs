use crate::components::cover::candidate_cover;
use crate::components::credits::{album_link, credits};
use crate::components::ui::field::field;
use crate::components::ui::icon::DURATION_ICON;
use crate::components::ui::nav::Page;
use crate::composables::use_notice::notice_of;
use crate::layouts::shell::shell;
use crate::pages::library::index::{Song, short_id, song};
use crate::server::utils::catalogue::{joined, song_facts};
use crate::server::utils::db::users::User;
use crate::server::utils::format::{clock, escape};
use crate::server::utils::model::{load_extras, songs_by_ids};
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::omm;
use crate::server::utils::omm::SongRes;
use crate::server::utils::session::{fail, require_role, state};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::icon::icon;
use topcoat::router::{page, query_params};
use topcoat::view::{View, component, view};

pub const CANDIDATES: usize = 10;

#[query_params(error = bad_request)]
pub struct CandidateQuery {
    pub id: String,
}

#[derive(Clone)]
pub struct Candidate {
    pub id: String,
    pub short: String,
    pub art: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub year: String,
    pub number: String,
    pub genre: String,
    pub isrc: String,
    pub length: String,
    pub current: bool,
}

#[derive(Clone)]
pub struct Choice {
    pub me: User,
    pub facts: Vec<(String, String)>,
    pub song: Song,
    pub candidates: Vec<Candidate>,
}

pub fn candidate(res: &SongRes, current: Option<&str>) -> Candidate {
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
pub async fn candidates_page(cx: &Cx) -> Result<impl View> {
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
        .map_err(|_| {
            fail(crate::server::utils::error::ApiError::generic(
                "internal server error",
            ))
        })?
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
pub async fn candidates_view(cx: &Cx, choice: Choice, notice: Option<Notice>) -> Result<impl View> {
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
                        <span class="c-time"><span class="muted">icon(data: DURATION_ICON)</span></span>
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
