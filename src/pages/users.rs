use crate::components::member_row::member_row;
use crate::components::ui::check::check;
use crate::components::ui::field::field;
use crate::components::ui::nav::Page;
use crate::components::ui::panel::panel;
use crate::composables::use_notice::notice_of;
use crate::layouts::shell::shell;
use crate::server::utils::data;
use crate::server::utils::data::Admin;
use crate::server::utils::db::users::User;
use crate::server::utils::notice::Notice;
use crate::server::utils::session::{fail, require_admin, state};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::page;
use topcoat::view::{View, component, view};

#[page("/users")]
pub async fn users_page(cx: &Cx) -> Result<impl View> {
    let me = require_admin(cx).await?;
    let notice = notice_of(cx)?;
    let admin = state(cx).db.run(|c| data::admin(c)).await.map_err(fail)?;
    Ok(view! { users_view(me: me, admin: admin, notice: notice) })
}

#[component]
pub async fn users_view(
    cx: &Cx,
    me: User,
    admin: Admin,
    notice: Option<Notice>,
) -> Result<impl View> {
    Ok(view! {
        shell(
            page: Some(Page::Users),
            name: me.name.as_str(),
            me: Some(me.clone()),
            notice: notice,
            panel(
                title: "users",
                wide: true,
                note: "admin only",
                                <div class="members">
                    for member in admin.members.iter() {
                        member_row(member: member, me: me.id.as_str(), libraries: &admin.libraries)
                    }
                </div>
            )
            panel(
                title: "new user",
                                wide: true,
                                <form method="post" action="/users/create">
                    field(label: "username", name: "username", kind: "text", auto: "off", required: true)
                    field(label: "password", name: "password", kind: "password", auto: "new-password", required: true)
                    check(name: "admin", label: "admin")
                    <button>"create user"</button>
                </form>
            )
        )
    })
}
