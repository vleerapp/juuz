use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

#[component]
pub async fn check(
    cx: &Cx,
    name: &str,
    label: &str,
    #[default] checked: bool,
    #[default] disabled: bool,
) -> Result<impl View> {
    Ok(view! {
        <label class="check">
            <input type="checkbox" name=(name) checked=(checked) disabled=(disabled)>
            (label)
        </label>
    })
}
