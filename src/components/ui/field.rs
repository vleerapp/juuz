use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

#[component]
pub async fn field(
    cx: &Cx,
    label: &str,
    name: &str,
    kind: &str,
    auto: &str,
    #[default] value: &str,
    #[default] mode: &str,
    #[default] required: bool,
    #[default] focus: bool,
    #[default] placeholder: &str,
) -> Result<impl View> {
    Ok(view! {
        <label>
            <span class="caption">(label)</span>
            <input name=(name) type=(kind) autocomplete=(auto) value=(value) inputmode=(mode) required=(required) autofocus=(focus) placeholder=(placeholder)>
        </label>
    })
}
