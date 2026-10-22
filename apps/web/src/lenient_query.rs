//! Tolerant conversion for the typed, optional query parameters of GET pages
//! (#580). Axum's `Query` extractor rejects the whole request when one field
//! does not convert: a forged or truncated link (`/messagerie?reply_to=`)
//! answered a plain-text 400, outside the page shell. With
//! `#[serde(default, deserialize_with = "crate::lenient_query::lenient")]` a
//! parameter that is empty or does not parse reads as absent, and the page
//! renders as if it had not been given.
//!
//! Only for optional parameters that merely refine a page: a value the page
//! cannot do without belongs in the path or in a form, where a rejection is
//! the right answer.
//!
//! A repeated parameter (`?reply_to=a&reply_to=b`) was the other way to get
//! that 400: serde rejects a struct key seen twice as a "duplicate field".
//! [`LenientQuery`], which replaces `Query` on those pages (#582), keeps the
//! **first occurrence** of each key and drops the others, typed or not —
//! even when the first does not convert and a later one would: one rule,
//! not a search for a usable value.

use std::collections::HashSet;
use std::str::FromStr;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};

/// Reads the parameter as text, then converts it with `FromStr`: an empty
/// or unconvertible value is `None`, never an error.
pub fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
{
    Ok(Option::<String>::deserialize(deserializer)?.and_then(|raw| raw.parse().ok()))
}

/// Decodes a query string into `T`, keeping only the first occurrence of
/// each key. The pairs are decoded once, then re-encoded, so a value's
/// escapes (`%26`, `%2B`) survive the round trip as the characters they were.
pub fn from_query_str<T: DeserializeOwned>(raw: &str) -> Result<T, serde_urlencoded::de::Error> {
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(raw)?;
    let mut seen = HashSet::new();
    let first: Vec<(String, String)> = pairs
        .into_iter()
        .filter(|(key, _)| seen.insert(key.clone()))
        .collect();
    let deduplicated = serde_urlencoded::to_string(&first)
        .map_err(<serde_urlencoded::de::Error as serde::de::Error>::custom)?;
    serde_urlencoded::from_str(&deduplicated)
}

/// `Query` with [`from_query_str`]'s rule for repeated keys. A struct whose
/// typed fields all carry [`lenient`] cannot fail; anything else still
/// answers the plain-text 400 `Query` did.
pub struct LenientQuery<T>(pub T);

#[axum::async_trait]
impl<T, S> FromRequestParts<S> for LenientQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        from_query_str(parts.uri.query().unwrap_or_default())
            .map(LenientQuery)
            .map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("Failed to deserialize query string: {e}"),
                )
                    .into_response()
            })
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeZone, Utc};
    use uuid::Uuid;

    #[derive(serde::Deserialize, Debug)]
    struct Q {
        #[serde(default, deserialize_with = "super::lenient")]
        id: Option<Uuid>,
        #[serde(default, deserialize_with = "super::lenient")]
        at: Option<DateTime<Utc>>,
        #[serde(default, deserialize_with = "super::lenient")]
        n: Option<i64>,
        #[serde(default, deserialize_with = "super::lenient")]
        m: Option<i32>,
        other: Option<String>,
    }

    fn q(s: &str) -> Q {
        super::from_query_str(s).expect("a lenient query never fails")
    }

    #[test]
    fn absent_parameters_are_none() {
        let q = q("");
        assert!(q.id.is_none() && q.at.is_none() && q.n.is_none() && q.m.is_none());
    }

    #[test]
    fn empty_values_are_none() {
        let q = q("id=&at=&n=&m=");
        assert!(q.id.is_none() && q.at.is_none() && q.n.is_none() && q.m.is_none());
    }

    #[test]
    fn invalid_values_are_none() {
        let q = q("id=pas-un-uuid&at=y&n=abc&m=1.5");
        assert!(q.id.is_none() && q.at.is_none() && q.n.is_none() && q.m.is_none());
    }

    #[test]
    fn out_of_range_integer_is_none() {
        assert!(q("m=99999999999").m.is_none());
    }

    #[test]
    fn valid_values_convert() {
        let q = q(
            "id=6f1c1c1e-3b4a-4d2e-9f00-0123456789ab&at=2026-10-22T08%3A30%3A00.123456Z&n=-5&m=30",
        );
        assert_eq!(
            q.id,
            Some(Uuid::parse_str("6f1c1c1e-3b4a-4d2e-9f00-0123456789ab").unwrap())
        );
        assert_eq!(
            q.at,
            Some(
                Utc.with_ymd_and_hms(2026, 10, 22, 8, 30, 0).unwrap()
                    + chrono::Duration::microseconds(123_456)
            )
        );
        assert_eq!(q.n, Some(-5));
        assert_eq!(q.m, Some(30));
    }

    #[test]
    fn an_invalid_parameter_leaves_the_others_intact() {
        let q = q("id=x&n=7&other=keep");
        assert!(q.id.is_none());
        assert_eq!(q.n, Some(7));
        assert_eq!(q.other.as_deref(), Some("keep"));
    }

    #[test]
    fn a_repeated_parameter_keeps_its_first_occurrence() {
        // #582: serde rejects a repeated key as a duplicate field; the page
        // reads the first one, typed or not.
        let q = q("n=7&other=a&n=8&other=b");
        assert_eq!(q.n, Some(7));
        assert_eq!(q.other.as_deref(), Some("a"));
    }

    #[test]
    fn a_repeated_parameter_whose_first_occurrence_is_invalid_is_absent() {
        // First occurrence wins even when a later one would convert: one
        // rule, not a search for a usable value.
        assert!(q("n=x&n=7").n.is_none());
        assert!(q("n=&n=7").n.is_none());
    }

    #[test]
    fn repeated_values_are_decoded_once() {
        // Re-encoding must not double-decode: `%2B` stays a literal plus.
        let q = q("other=a%2Bb%26c&other=z");
        assert_eq!(q.other.as_deref(), Some("a+b&c"));
    }
}
