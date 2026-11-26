use topcoat::context::{Cx, try_context};
use topcoat::runtime::RuntimeSetup;

pub fn runtime_on(cx: &Cx) -> bool {
    try_context::<RuntimeSetup>(cx).is_some()
}
