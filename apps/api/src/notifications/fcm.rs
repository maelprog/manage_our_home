//! Firebase Cloud Messaging, for the Android app (#1002): the device's
//! registration token, and the server's FCM configuration.
//!
//! The app is a Capacitor WebView with no Web Push; Google's FCM pushes it
//! instead, and knows the device by a **registration token** the app gets
//! from the Firebase SDK. The token is stored in `push_subscriptions`
//! (`platform = 'fcm'`, migration 0071) in place of a browser's endpoint.
//!
//! **A token is an opaque string the server never requests.** Unlike a
//! browser's endpoint (`push::validate_endpoint`), it is not a URL: the
//! server will hand it to FCM's API in a JSON body (#1003). It is still
//! user input stored per account and shown in the art. 15 export, so
//! [`validate_token`] bounds its length and its characters, and checks
//! nothing else — FCM documents no format.
//!
//! **The two platforms cannot share a row.** `endpoint` is unique across
//! both; a token admits neither `/` nor `.`, which every endpoint
//! `push::validate_endpoint` admits holds (`https://<host>/…`), so no
//! token is ever a browser's endpoint, and no device moves from one
//! platform to the other on registering again.

use std::path::{Path, PathBuf};

/// Longest token taken. Real ones run to about 160 characters (an
/// instance id, `:`, then `APA91b…`); the bound is that of a browser's
/// endpoint (`push::MAX_ENDPOINT_LEN`), the same column.
pub const MAX_TOKEN_LEN: usize = 1024;

/// Why a token is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    Empty,
    TooLong,
    /// A character outside the tokens' alphabet: ASCII letters and
    /// digits, `-`, `_` and `:`.
    UnexpectedCharacter,
}

/// The token, if it may be one: 1 to [`MAX_TOKEN_LEN`] characters, each an
/// ASCII letter or digit, `-`, `_` (base64url) or `:` (between the
/// instance id and the rest).
pub fn validate_token(raw: &str) -> Result<&str, TokenError> {
    if raw.is_empty() {
        return Err(TokenError::Empty);
    }
    if raw.len() > MAX_TOKEN_LEN {
        return Err(TokenError::TooLong);
    }
    if !raw
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
    {
        return Err(TokenError::UnexpectedCharacter);
    }
    Ok(raw)
}

/// Environment variable holding the Firebase project's id.
pub const PROJECT_ID_VAR: &str = "FCM_PROJECT_ID";
/// Environment variable holding the path of the service account's JSON
/// key, the credential FCM's API is called with.
pub const SERVICE_ACCOUNT_FILE_VAR: &str = "FCM_SERVICE_ACCOUNT_FILE";

/// The server's FCM configuration. Read at start, not used to send yet
/// (#1003): its presence alone opens the registration of tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FcmConfig {
    project_id: String,
    service_account_file: PathBuf,
}

/// Why a configuration is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// One of the two variables set without the other.
    Incomplete,
    /// Not a Google Cloud project id: 6 to 30 lowercase ASCII letters,
    /// digits or hyphens, starting with a letter, not ending with a
    /// hyphen. It goes into the path of FCM's API (#1003).
    BadProjectId,
    /// The service account's file is not there.
    MissingServiceAccountFile,
}

impl FcmConfig {
    /// From the project id and the service account's file path, both
    /// already read: checks the id's form, not the file.
    pub fn new(project_id: &str, service_account_file: &str) -> Result<Self, ConfigError> {
        let (project_id, file) = (project_id.trim(), service_account_file.trim());
        if project_id.is_empty() || file.is_empty() {
            return Err(ConfigError::Incomplete);
        }
        if !is_project_id(project_id) {
            return Err(ConfigError::BadProjectId);
        }
        Ok(FcmConfig {
            project_id: project_id.to_string(),
            service_account_file: PathBuf::from(file),
        })
    }

    /// From [`PROJECT_ID_VAR`] and [`SERVICE_ACCOUNT_FILE_VAR`]: `Ok(None)`
    /// when both are unset or empty — the Android app's notifications are
    /// then off on this server, and a token is refused with
    /// `fcm_not_configured` —, an error when one is set without the other,
    /// when the id is malformed or when the file is not there, so that a
    /// typo stops the start instead of silently turning them off.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let project_id = std::env::var(PROJECT_ID_VAR).unwrap_or_default();
        let file = std::env::var(SERVICE_ACCOUNT_FILE_VAR).unwrap_or_default();
        if project_id.trim().is_empty() && file.trim().is_empty() {
            return Ok(None);
        }
        let config = FcmConfig::new(&project_id, &file)
            .and_then(|config| {
                if config.service_account_file.is_file() {
                    Ok(config)
                } else {
                    Err(ConfigError::MissingServiceAccountFile)
                }
            })
            .map_err(|e| anyhow::anyhow!("{PROJECT_ID_VAR} / {SERVICE_ACCOUNT_FILE_VAR}: {e:?}"))?;
        Ok(Some(config))
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn service_account_file(&self) -> &Path {
        &self.service_account_file
    }
}

fn is_project_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (6..=30).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[bytes.len() - 1] != b'-'
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of a real token: an instance id, `:`, then `APA91b` and
    /// base64url.
    const PLAUSIBLE: &str = "dVq3mJ8kTq2xQ9Yw1ZbC0e:APA91bHk3s-Lp_Q7vR2mXy8T0aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ-_0123456789abcdefghijklmnopqrs";

    #[test]
    fn a_plausible_token_is_taken_as_it_is() {
        assert_eq!(validate_token(PLAUSIBLE), Ok(PLAUSIBLE));
        assert_eq!(validate_token("a"), Ok("a"));
        let longest = "A".repeat(MAX_TOKEN_LEN);
        assert_eq!(validate_token(&longest), Ok(longest.as_str()));
    }

    #[test]
    fn an_empty_token_is_refused() {
        assert_eq!(validate_token(""), Err(TokenError::Empty));
    }

    #[test]
    fn a_token_too_long_is_refused() {
        let token = "A".repeat(MAX_TOKEN_LEN + 1);
        assert_eq!(validate_token(&token), Err(TokenError::TooLong));
    }

    #[test]
    fn a_character_outside_the_alphabet_is_refused() {
        for token in [
            " abc",
            "abc ",
            "ab c",
            "abc\n",
            "abc\0",
            "ab/c",
            "ab.c",
            "ab+c",
            "ab=c",
            "ab%2F",
            "ab\"c",
            "<script>",
            "abcé",
            "https://fcm.googleapis.com/fcm/send/abc",
        ] {
            assert_eq!(
                validate_token(token),
                Err(TokenError::UnexpectedCharacter),
                "{token:?}"
            );
        }
    }

    /// `endpoint` is unique across both platforms: no browser's endpoint
    /// is ever a token, so a registration never takes a row of the other
    /// platform over.
    #[test]
    fn no_browser_endpoint_is_a_token() {
        for endpoint in [
            "https://fcm.googleapis.com/fcm/send/dVq3mJ8:APA91bH",
            "https://updates.push.services.mozilla.com/wpush/v2/gAAAAABk",
            "https://web.push.apple.com/QGx",
            "https://wns2-par02p.notify.windows.com/w/?token=BQYAAAB",
        ] {
            assert!(
                super::super::push::validate_endpoint(endpoint).is_ok(),
                "{endpoint}"
            );
            assert!(validate_token(endpoint).is_err(), "{endpoint}");
        }
    }

    #[test]
    fn a_configuration_needs_both_values() {
        assert_eq!(
            FcmConfig::new("", "/run/secrets/fcm.json"),
            Err(ConfigError::Incomplete)
        );
        assert_eq!(
            FcmConfig::new("manage-our-home", "  "),
            Err(ConfigError::Incomplete)
        );
        let config = FcmConfig::new(" manage-our-home ", "/run/secrets/fcm.json").unwrap();
        assert_eq!(config.project_id(), "manage-our-home");
        assert_eq!(
            config.service_account_file(),
            Path::new("/run/secrets/fcm.json")
        );
    }

    #[test]
    fn the_project_id_is_a_google_cloud_one() {
        for id in ["abcdef", "manage-our-home", "home-2026", &"a".repeat(30)] {
            assert!(FcmConfig::new(id, "/k.json").is_ok(), "{id}");
        }
        for id in [
            "abcde",
            &"a".repeat(31),
            "1home-app",
            "-home-app",
            "home-app-",
            "Home-App",
            "home_app",
            "home.app",
            "home/app",
            "home app",
            "../../x",
        ] {
            assert_eq!(
                FcmConfig::new(id, "/k.json"),
                Err(ConfigError::BadProjectId),
                "{id}"
            );
        }
    }
}
