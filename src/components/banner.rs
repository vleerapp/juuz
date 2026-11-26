use crate::server::utils::notice::Notice;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, class, component, view};

#[component]
pub async fn banner(cx: &Cx, notice: Option<Notice>) -> Result<impl View> {
    Ok(view! {
        match notice {
            Some(n) => <p class=(class!("notice", "error" if n.is_error())) role="status">(n.text())</p>,
            None => "",
        }
    })
}
