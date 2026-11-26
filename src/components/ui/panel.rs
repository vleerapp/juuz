use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{Child, View, class, component, view};

#[component]
pub async fn panel(
    cx: &Cx,
    #[default] title: &str,
    #[default] note: &str,
    #[default] wide: bool,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    Ok(view! {
        <section class=(class!("panel", "wide" if wide))>
            if !title.is_empty() {
                <h2>(title) if !note.is_empty() { <small>(note)</small> }</h2>
            }
            (child)
        </section>
    })
}
