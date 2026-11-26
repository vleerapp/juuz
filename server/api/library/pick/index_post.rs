use crate::components::ui::nav::Page;
use crate::pages::library::index::full_id;
use crate::server::utils::catalogue::PickForm;
use crate::server::utils::db::libraries;
use crate::server::utils::db::users::User;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::omm;
use crate::server::utils::session::{fail, require_role, state};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[route(POST "/library/pick")]
pub async fn pick(cx: &Cx, Form(form): Form<PickForm>) -> Result<SeeOther> {
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
        .map_err(|_| {
            fail(crate::server::utils::error::ApiError::generic(
                "internal server error",
            ))
        })?;
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
            let file = crate::server::utils::scanner::tags::read_track(std::path::Path::new(&path)).ok();
            omm::store_manual_match(c, &id, &found, Some(&local), file.as_ref())?;
            Ok(())
        })
        .await
        .map_err(fail)?;
    st.live
        .notify_topics(crate::server::utils::live::Live::LIBRARY);
    if let Some(handle) = &st.omm_handle {
        handle.wake();
    }
    Ok(back(Page::Library, Notice::CatalogueMatched))
}
