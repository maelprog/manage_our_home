//! `/account/appearance` (#585, maquette « Mobile — Apparence ») — the
//! theme: Automatique, Clair or Sombre.
//!
//! Three native radio buttons in a POST form, no script: the CSP's
//! `script-src` is unchanged. The choice is a cookie (`crate::theme`), so it
//! holds for this browser, not for the account — storing it on the account
//! is out of this issue's scope. Like the family choice of the profile
//! (#751), a radio applies itself since #844: with JavaScript, picking one
//! sends the form (`data-submit-on-change`), the page comes back with
//! « Apparence enregistrée. » and the focus on that radio
//! (`data-keep-scroll`), and a sentence says the choice applies on the
//! click (WCAG 3.2.2). « Enregistrer » is the fallback without the script,
//! which hides it (`app::submit_fallback`).

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;

use super::account_header;
use crate::app::{shell_with_header, submit_fallback, Width, APPLIES_ON_CHANGE};
use crate::layout::CurrentUser;
use crate::state::AppState;
use crate::theme::{self, Appearance};

pub const PATH: &str = "/account/appearance";

#[derive(serde::Deserialize)]
pub struct PageQuery {
    notice: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct ThemeForm {
    #[serde(default)]
    theme: String,
}

/// What each choice does, under its name.
fn hint(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Auto => "Suit le réglage de votre appareil",
        Appearance::Light => "Toujours clair, quel que soit l'appareil",
        Appearance::Dark => "Toujours sombre, quel que soit l'appareil",
    }
}

/// One choice: the whole row is the radio's label, as for the families of
/// the profile (`profile::family_choice`).
fn choice(appearance: Appearance, current: Appearance) -> String {
    let checked = if appearance == current {
        " checked"
    } else {
        ""
    };
    format!(
        r#"<label class="list-row"><input type="radio" name="theme" value="{value}"{checked} data-submit-on-change><span><strong>{label}</strong><br><span class="muted">{hint}</span></span></label>"#,
        value = appearance.value(),
        label = appearance.label(),
        hint = hint(appearance),
    )
}

/// The page's content for the `current` choice, with the confirmation when
/// one was just saved.
pub(crate) fn page_body(current: Appearance, saved: bool) -> String {
    let notice = if saved {
        r#"<p class="notice success" role="status">Apparence enregistrée.</p>"#
    } else {
        ""
    };
    let choices: String = Appearance::ALL
        .iter()
        .map(|a| choice(*a, current))
        .collect();
    format!(
        r#"<h1>Apparence</h1>
{notice}<section class="card">
<h2 id="theme">Thème</h2>
<form method="post" action="{PATH}" data-keep-scroll>
{APPLIES_ON_CHANGE}
<div role="radiogroup" aria-labelledby="theme">{choices}</div>
{fallback}
</form>
<p class="muted">Ce choix vaut pour ce navigateur, et il est gardé un an.</p>
</section>
<p><a href="/account">Retour à mon compte</a></p>"#,
        fallback = submit_fallback("Enregistrer"),
    )
}

/// `GET /account/appearance`.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Response {
    let header = account_header(&state, &headers, &me, PATH).await;
    let body = page_body(theme::current(), query.notice.as_deref() == Some("saved"));
    Html(shell_with_header(Width::Read, "Apparence", &header, &body)).into_response()
}

/// `POST /account/appearance` — sets the cookie for Clair or Sombre,
/// removes it for Automatique, then back to the page. A value the page
/// does not offer changes nothing.
pub async fn post(CurrentUser(_me): CurrentUser, Form(form): Form<ThemeForm>) -> Response {
    let Some(appearance) = Appearance::from_value(&form.theme) else {
        return Redirect::to(PATH).into_response();
    };
    let mut response = Redirect::to(&format!("{PATH}?notice=saved")).into_response();
    if let Ok(value) = HeaderValue::from_str(&appearance.set_cookie()) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_choices_are_radios_of_one_field_in_a_post_form() {
        let html = page_body(Appearance::Auto, false);
        assert!(
            html.contains(r#"<form method="post" action="/account/appearance""#),
            "{html}"
        );
        for value in ["auto", "light", "dark"] {
            assert!(
                html.contains(&format!(
                    r#"<input type="radio" name="theme" value="{value}""#
                )),
                "{value}: {html}"
            );
        }
        for label in ["Automatique", "Clair", "Sombre"] {
            assert!(
                html.contains(&format!("<strong>{label}</strong>")),
                "{label}"
            );
        }
        assert!(html.contains(r#"<button type="submit">Enregistrer</button>"#));
        assert!(!html.contains("<script"), "{html}");
    }

    #[test]
    fn only_the_current_choice_is_checked() {
        for current in Appearance::ALL {
            let html = page_body(current, false);
            assert_eq!(html.matches(" checked").count(), 1, "{html}");
            assert!(
                html.contains(&format!(r#"value="{}" checked "#, current.value())),
                "{current:?}: {html}"
            );
        }
    }

    #[test]
    fn a_saved_choice_is_confirmed() {
        assert!(page_body(Appearance::Dark, true).contains("Apparence enregistrée."));
        assert!(!page_body(Appearance::Dark, false).contains("Apparence enregistrée."));
    }

    /// #844: with JavaScript a radio sends the form by itself, the page
    /// comes back with the focus on it, and the sentence that says so is
    /// shown; the button and its sentence are the fallback without the
    /// script, rendered and hidden by it.
    #[test]
    fn a_choice_applies_itself_with_the_button_as_fallback() {
        let html = page_body(Appearance::Light, false);
        assert!(
            html.contains(r#"<form method="post" action="/account/appearance" data-keep-scroll>"#),
            "{html}"
        );
        for value in ["auto", "light", "dark"] {
            let at = html
                .find(&format!(r#"name="theme" value="{value}""#))
                .expect(value);
            let input = &html[at..at + html[at..].find('>').unwrap()];
            assert!(input.contains(" data-submit-on-change"), "{input}");
        }
        assert!(
            html.contains(
                r#"<p class="muted" data-shown-with-js hidden>Le choix s'applique dès que vous le touchez.</p>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<div data-hidden-with-js><p class="muted">Sans JavaScript, le choix s'envoie avec le bouton.</p><button type="submit">Enregistrer</button></div>"#
            ),
            "{html}"
        );
        // The fallback stays inside the form, so it still posts the radios.
        let form = &html[html.find("<form").unwrap()..html.find("</form>").unwrap()];
        assert!(form.contains("<button type=\"submit\">"), "{form}");
    }

    #[test]
    fn the_confirmation_is_announced() {
        assert!(page_body(Appearance::Dark, true)
            .contains(r#"<p class="notice success" role="status">Apparence enregistrée.</p>"#));
    }
}
