use dioxus::prelude::*;

use crate::{api, i18n::AuthText, skills::SkillsPage};

#[derive(Clone, Copy, PartialEq)]
enum Session {
    Checking,
    SignedOut,
    Connected,
}

#[component]
pub fn TokenPage() -> Element {
    let mut session = use_signal(|| Session::Checking);
    let mut token = use_signal(String::new);
    let mut pending = use_signal(|| false);
    let mut error = use_signal(|| None::<api::AuthError>);

    use_effect(move || {
        spawn(async move {
            if let Some(saved) = api::auth_token().filter(|saved| !saved.trim().is_empty()) {
                match api::validate_token(&saved).await {
                    Ok(()) => session.set(Session::Connected),
                    Err(failure) => {
                        error.set(Some(failure));
                        session.set(Session::SignedOut);
                    }
                }
            } else {
                session.set(Session::SignedOut);
            }
        });
    });

    if session() == Session::Checking {
        return rsx! { p { role: "status", {AuthText::Checking.text()} } };
    }

    if session() == Session::Connected {
        return rsx! {
            div { class: "page",
                button {
                    class: "btn-electric",
                    onclick: move |_| {
                        match api::clear_token() {
                            Ok(()) => {
                                error.set(None);
                                session.set(Session::SignedOut);
                            }
                            Err(failure) => error.set(Some(failure)),
                        }
                    },
                    {AuthText::Disconnect.text()}
                }
                if let Some(failure) = error() {
                    p { class: "error-banner", role: "alert", {failure.text()} }
                }
            }
            SkillsPage {}
        };
    }

    rsx! {
        main { class: "page token-page",
            h1 { {AuthText::Title.text()} }
            p { {AuthText::Help.text()} }
            form {
                class: "token-form",
                onsubmit: move |event| {
                    event.prevent_default();
                    if pending() { return; }
                    let candidate = token().trim().to_owned();
                    if candidate.is_empty() {
                        error.set(Some(api::AuthError::Empty));
                        return;
                    }
                    pending.set(true);
                    error.set(None);
                    spawn(async move {
                        match api::validate_token(&candidate).await {
                            Ok(()) => match api::store_token(&candidate) {
                                Ok(()) => {
                                    token.set(String::new());
                                    session.set(Session::Connected);
                                }
                                Err(failure) => error.set(Some(failure)),
                            },
                            Err(failure) => error.set(Some(failure)),
                        }
                        pending.set(false);
                    });
                },
                label { r#for: "gateway-token", {AuthText::Token.text()} }
                input {
                    id: "gateway-token",
                    class: "input-electric",
                    r#type: "password",
                    autocomplete: "off",
                    spellcheck: "false",
                    required: true,
                    disabled: pending(),
                    value: "{token}",
                    oninput: move |event| token.set(event.value()),
                }
                button {
                    class: "btn-electric",
                    r#type: "submit",
                    disabled: pending(),
                    if pending() { {AuthText::Checking.text()} }
                    else { {AuthText::Connect.text()} }
                }
            }
            if let Some(failure) = error() {
                p { class: "error-banner", role: "alert", {failure.text()} }
            }
        }
    }
}
