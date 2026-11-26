//! Model center: providers and their models, used by translation and by plugins that depend on it.

use super::plugin_form::BuiltinSettings;
use dioxus::prelude::*;

#[component]
pub fn ModelCenter() -> Element {
    rsx! {
        BuiltinSettings { id: zulip_builtins::model_center::ID, title: "模型中心" }
    }
}
