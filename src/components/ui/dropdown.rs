use crate::components::ui::icon::CHEVRON_DOWN_ICON;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::icon::icon;
use topcoat::view::{View, component, view};

#[component]
pub async fn dropdown(
    cx: &Cx,
    name: &str,
    current: &str,
    options: &[(String, String)],
) -> Result<impl View> {
    let label = options
        .iter()
        .find(|(value, _)| value == current)
        .map(|(_, label)| label.as_str())
        .unwrap_or("");
    Ok(view! {
        <div class="select" data-select="">
            <input type="hidden" name=(name) value=(current)>
            <button type="button" class="select-trigger" aria-haspopup="listbox" aria-expanded="false">
                <span class="value">(label)</span>
                icon(data: CHEVRON_DOWN_ICON)
            </button>
            <div class="options" role="listbox" hidden=(true)>
                for (value, text) in options.iter() {
                    <button type="button" role="option" data-value=(value.as_str()) aria-selected=(if value == current { "true" } else { "false" })>(text.as_str())</button>
                }
            </div>
        </div>
    })
}
