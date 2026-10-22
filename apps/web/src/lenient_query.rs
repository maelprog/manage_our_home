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

use std::str::FromStr;

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
        serde_urlencoded::from_str(s).expect("a lenient query never fails")
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
}
