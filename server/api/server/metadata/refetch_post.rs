use crate::components::ui::nav::Page;
use crate::server::utils::db::users::User;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{fail, require_role, state};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[route(POST "/server/metadata/refetch")]
pub async fn refetch_metadata(cx: &Cx) -> Result<SeeOther> {
    require_role(cx, User::can_catalogue).await?;
    let st = state(cx);
    let Some(handle) = st.omm_handle.clone() else {
        return Ok(back(Page::Server, Notice::CatalogueOff));
    };
    st.db
        .run(move |c| Ok(crate::server::utils::omm::refetch_all(c)?))
        .await
        .map_err(fail)?;
    handle.wake();
    st.live
        .notify_topics(crate::server::utils::live::Live::LIBRARY);
    Ok(back(Page::Server, Notice::MetadataRefetch))
}
