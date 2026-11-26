use crate::components::ui::field::field;
use crate::components::ui::panel::panel;
use crate::layouts::shell::shell;
use crate::server::utils::notice::Notice;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

#[component]
pub async fn login_page(cx: &Cx, notice: Option<Notice>) -> Result<impl View> {
    Ok(view! {
        shell(
            page: None,
            name: "",
            me: None,
            notice: notice,
            panel(
                    <form method="post" action="/login">
                        field(label: "username", name: "username", kind: "text", auto: "username", required: true, focus: true)
                        field(label: "password", name: "password", kind: "password", auto: "current-password", required: true)
                        <button>"sign in"</button>
                    </form>
                )
        )
    })
}
