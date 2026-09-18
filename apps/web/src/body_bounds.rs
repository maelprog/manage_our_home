//! The pages apps/web answers when a request body is refused for its pace
//! or for lack of room (#219). The bounds themselves live in
//! `manage_our_home_http_guard`; `build_router` installs them on every
//! route, and `routes::agenda::attachments::upload` takes the upload
//! permit.

use axum::response::{Html, IntoResponse, Response};

use crate::app::{shell, Width};

/// Body of the 408 for a request body that came too slowly. Status and
/// `Connection: close` are set by the middleware. Generic on purpose: any
/// form can be the one cut, not only an upload.
pub fn body_read_timeout_page() -> Response {
    let body = r#"<h1>Envoi interrompu</h1>
<p>Les données arrivaient trop lentement et l'envoi a été interrompu. Merci de réessayer.</p>
<a class="btn secondary" href="/">Retour à l'accueil</a>"#;
    Html(shell(Width::Form, "Envoi interrompu", body)).into_response()
}
