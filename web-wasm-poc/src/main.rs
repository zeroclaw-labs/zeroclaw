mod api;
mod auth;
mod i18n;
mod skill_card;
mod skills;

use dioxus::prelude::*;

#[component]
fn App() -> Element {
    rsx! {
        auth::TokenPage {}
    }
}

fn main() {
    dioxus::launch(App);
}
