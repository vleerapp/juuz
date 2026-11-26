use crate::components::ui::nav::Page;
use crate::server::utils::auth;
use crate::server::utils::db::users;
use crate::server::utils::db::users::RoleUpdate;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{fail, require_admin, state, token};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct UpdateForm {
    pub username: String,
    #[serde(default)]
    pub password: String,
    pub admin: Option<String>,
    pub settings: Option<String>,
    pub stream: Option<String>,
    pub download: Option<String>,
    pub upload: Option<String>,
    pub playlist: Option<String>,
    pub cover_art: Option<String>,
    pub comment: Option<String>,
    pub share: Option<String>,
    pub jukebox: Option<String>,
    pub keys: Option<String>,
    pub scan: Option<String>,
    pub server: Option<String>,
    pub radio: Option<String>,
    pub delete: Option<String>,
    pub library: Option<String>,
    pub catalogue: Option<String>,
}

#[route(POST "/users/update")]
pub async fn update_user(cx: &Cx, Form(form): Form<UpdateForm>) -> Result<SeeOther> {
    let me = require_admin(cx).await?;
    let st = state(cx);
    let roles = RoleUpdate {
        admin: Some(form.admin.is_some()),
        settings: Some(form.settings.is_some()),
        stream: Some(form.stream.is_some()),
        download: Some(form.download.is_some()),
        upload: Some(form.upload.is_some()),
        playlist: Some(form.playlist.is_some()),
        cover_art: Some(form.cover_art.is_some()),
        comment: Some(form.comment.is_some()),
        share: Some(form.share.is_some()),
        jukebox: Some(form.jukebox.is_some()),
        keys: Some(form.keys.is_some()),
        scan: Some(form.scan.is_some()),
        server: Some(form.server.is_some()),
        radio: Some(form.radio.is_some()),
        delete: Some(form.delete.is_some()),
        library: Some(form.library.is_some()),
        catalogue: Some(form.catalogue.is_some()),
        ..RoleUpdate::default()
    };
    let credentials = if form.password.is_empty() {
        None
    } else {
        Some(
            auth::credentials(&st.crypto, &form.password)
                .await
                .map_err(fail)?,
        )
    };
    let keep = token(cx).await?;
    let name = form.username;
    let outcome = st
        .db
        .run(move |c| {
            let Some(target) = users::by_name(c, &name)? else {
                return Ok(Notice::Invalid);
            };
            let own = target.id == me.id;
            if own && roles.admin != Some(true) {
                return Ok(Notice::SelfProtected);
            }
            users::update_roles(c, &target.id, &roles)?;
            if let Some((hash, sealed)) = credentials {
                users::set_password(c, &target.id, &hash, sealed.as_deref())?;
                if own {
                    users::delete_sessions_except(c, &target.id, &keep)?;
                } else {
                    users::delete_sessions(c, &target.id)?;
                }
            }
            Ok(Notice::UserUpdated)
        })
        .await
        .map_err(fail)?;
    Ok(back(Page::Users, outcome))
}
