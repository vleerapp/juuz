use crate::components::ui::nav::Page;
use crate::server::utils::db::users;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{fail, require_admin, state};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[route(POST "/users/libraries")]
pub async fn user_libraries(
    cx: &Cx,
    Form(form): Form<std::collections::HashMap<String, String>>,
) -> Result<SeeOther> {
    require_admin(cx).await?;
    let Some(name) = form.get("username").cloned() else {
        return Ok(back(Page::Users, Notice::Invalid));
    };
    let granted: Vec<i64> = form
        .keys()
        .filter_map(|k| k.strip_prefix("lib_")?.parse().ok())
        .collect();
    let upload: Option<i64> = form.get("upload").and_then(|v| v.parse().ok());
    let outcome = state(cx)
        .db
        .run(move |c| {
            let Some(user) = users::by_name(c, &name)? else {
                return Ok(Notice::Invalid);
            };
            if !user.admin {
                crate::server::utils::db::libraries::set_access(c, &user.id, &granted)?;
            }
            let upload = upload.filter(|id| {
                (user.admin || granted.contains(id))
                    && crate::server::utils::db::libraries::get(c, *id)
                        .ok()
                        .flatten()
                        .is_some()
            });
            c.execute(
                "UPDATE users SET upload_library_id = ?2 WHERE id = ?1",
                rusqlite::params![user.id, upload],
            )?;
            Ok(Notice::AccessSaved)
        })
        .await
        .map_err(fail)?;
    Ok(back(Page::Users, outcome))
}
