use crate::components::cover::candidate_cover;
use crate::components::ui::field::field;
use crate::components::ui::nav::Page;
use crate::composables::use_match::MatchChoice;
use crate::layouts::shell::shell;
use crate::pages::library::index::short_id;
use crate::server::utils::format::escape;
use crate::server::utils::notice::Notice;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

#[component]
pub async fn match_view(cx: &Cx, choice: MatchChoice, notice: Option<Notice>) -> Result<impl View> {
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
