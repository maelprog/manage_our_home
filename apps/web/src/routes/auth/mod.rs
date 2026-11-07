pub mod forgot_password;
pub mod login;
pub mod register;
pub mod resend_verification;
pub mod reset_password;
pub mod verify_email;

/// The script of the Android app's native Google sign-in (#670), served
/// under `/assets` (`assets::Script::GoogleSignIn`) and loaded by
/// [`google_actions`] in the app's pages only.
pub(crate) const GOOGLE_SIGN_IN_SCRIPT: &str = include_str!("../../google-sign-in.js");

/// The Google sign-in of `/login` and `/register`, the content of their
/// `.actions.stacked` row.
///
/// In a browser, the link to apps/api's `/auth/google/start`, which sends
/// on to Google's page. In the Android app (`mobile_app::current`), never
/// that link: Google refuses its page inside a WebView
/// (`403 disallowed_useragent`). There, a button that signs in through the
/// system's account picker ([`GOOGLE_SIGN_IN_SCRIPT`], loaded right after
/// it), hidden until the script finds the plugin; an APK built without the
/// plugin shows `#google-native-unsupported` instead, and a sign-in that
/// fails says so in `#google-native-error`.
pub(crate) fn google_actions(api_public_base_url: &str) -> String {
    let base = crate::app::html_escape(api_public_base_url);
    if !crate::mobile_app::current() {
        return format!(
            r#"<a href="{base}/auth/google/start" class="btn secondary">Continuer avec Google</a>"#
        );
    }
    format!(
        r#"<button type="button" class="secondary" id="google-native" data-api="{base}" hidden>Continuer avec Google</button>
<p class="notice warning" id="google-native-unsupported" hidden>Cette version de l'application ne permet pas la connexion avec Google : installez la dernière version.</p>
<p class="notice error" id="google-native-error" role="alert" hidden></p>
{script}"#,
        script = crate::assets::Script::GoogleSignIn.tag(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::Script;

    #[test]
    fn a_browser_gets_the_link_to_googles_page() {
        let html = google_actions("/api");
        assert_eq!(
            html,
            r#"<a href="/api/auth/google/start" class="btn secondary">Continuer avec Google</a>"#
        );
        assert!(!html.contains(&Script::GoogleSignIn.tag()), "{html}");
        // What `mobile_app::scope` sets for a browser's request.
        assert_eq!(
            crate::mobile_app::serving(false, || google_actions("/api")),
            html
        );
    }

    #[test]
    fn the_app_never_gets_the_link_but_a_native_button() {
        let html = crate::mobile_app::serving(true, || google_actions("/api"));
        // Google refuses its page in a WebView (`disallowed_useragent`).
        assert!(!html.contains("/auth/google/start"), "{html}");
        // Hidden until the script finds the plugin; it calls apps/api at
        // `data-api`.
        assert!(
            html.starts_with(
                r#"<button type="button" class="secondary" id="google-native" data-api="/api" hidden>Continuer avec Google</button>"#
            ),
            "{html}"
        );
        // An APK without the plugin is told to update; a failure is said
        // in a live region.
        assert!(
            html.contains(r#"<p class="notice warning" id="google-native-unsupported" hidden>"#),
            "{html}"
        );
        assert!(html.contains("installez la dernière version"), "{html}");
        assert!(
            html.contains(
                r#"<p class="notice error" id="google-native-error" role="alert" hidden></p>"#
            ),
            "{html}"
        );
        // The script, once, after what it drives.
        let tag = Script::GoogleSignIn.tag();
        assert_eq!(html.matches(&tag).count(), 1, "{html}");
        assert!(html.ends_with(&tag), "{html}");
    }

    #[test]
    fn the_api_base_is_escaped_in_both() {
        let base = r#"/a"b<c>&d"#;
        let browser = google_actions(base);
        assert!(
            browser.contains(r#"href="/a&quot;b&lt;c&gt;&amp;d/auth/google/start""#),
            "{browser}"
        );
        let app = crate::mobile_app::serving(true, || google_actions(base));
        assert!(
            app.contains(r#"data-api="/a&quot;b&lt;c&gt;&amp;d""#),
            "{app}"
        );
    }
}
