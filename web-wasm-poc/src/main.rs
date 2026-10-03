mod api;
mod i18n;
mod skill_card;
mod skills;

use dioxus::prelude::*;

#[component]
fn App() -> Element {
    rsx! {
        skills::SkillsPage {}
    }
}

fn main() {
    dioxus::launch(App);
}
