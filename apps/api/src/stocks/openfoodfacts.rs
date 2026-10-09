//! Open Food Facts, asked by the server for the product behind a scanned
//! code (#402).
//!
//! Server side, so that the visitor's address never reaches a third party:
//! Open Food Facts sees this server's address, the code and nothing else —
//! no cookie, no account, no family. Through `outbound_http::client()`, for
//! its timeouts.
//!
//! Its terms ask for an identifying `User-Agent` and allow 15 product reads
//! a minute per address (figure from issue #402). The second is kept by
//! `Throttle`, process-wide, and by the cache below: a product read once is
//! not asked again for `CACHE_TTL`, an unknown code for `MISS_TTL`. Past the
//! limit, or when the service fails, the scan answers without a product and
//! the page falls back on the manual form — nothing is cached then.
//!
//! The records are published under the Open Database Licence: the pages
//! that show one name the source (`apps/web`, `/stocks/new` and the legal
//! notice).

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, Utc};
use manage_our_home_shared::dto::stocks::ScannedProduct;
use serde_json::Value;

/// Production base URL. `OPENFOODFACTS_BASE_URL` overrides it (a stub in
/// tests and e2e).
pub const DEFAULT_BASE_URL: &str = "https://world.openfoodfacts.org";

/// The environment variable that overrides `DEFAULT_BASE_URL`.
pub const BASE_URL_ENV: &str = "OPENFOODFACTS_BASE_URL";

/// How long a product record read from Open Food Facts is served from the
/// cache before it is asked again.
pub const CACHE_TTL: Duration = Duration::days(30);

/// How long "unknown or nameless" is remembered: short, since a product a
/// member could not find may be added to Open Food Facts the same day.
pub const MISS_TTL: Duration = Duration::days(1);

/// Product reads allowed per `RATE_WINDOW`, from this process.
pub const RATE_LIMIT: usize = 15;

/// The window `RATE_LIMIT` is counted over.
pub const RATE_WINDOW: StdDuration = StdDuration::from_secs(60);

/// What the requests say they come from, as Open Food Facts asks:
/// application name, version and where to find out more.
pub const USER_AGENT: &str = concat!(
    "manage_our_home/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/maelprog/manage_our_home)"
);

/// The fields asked for: what `extract_product` reads, nothing more.
const FIELDS: &str = "product_name,product_name_fr,quantity,categories_tags";

/// `GET <base>/api/v2/product/<code>` with the fields `extract_product`
/// reads. `code` is a normalized code (digits only), so it needs no
/// escaping.
pub fn product_url(base: &str, code: &str) -> String {
    format!(
        "{}/api/v2/product/{code}?fields={FIELDS}",
        base.trim_end_matches('/')
    )
}

/// The product in an Open Food Facts answer, or `None` when the answer is
/// not a found product (`status` other than 1) or the record has no name —
/// the French one first, then the generic one, both trimmed.
pub fn extract_product(body: &Value) -> Option<ScannedProduct> {
    if body.get("status").and_then(Value::as_i64) != Some(1) {
        return None;
    }
    let product = body.get("product")?.as_object()?;
    let text = |key: &str| {
        product
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let name = text("product_name_fr").or_else(|| text("product_name"))?;
    let categories_tags = product
        .get("categories_tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(ScannedProduct {
        name,
        quantity: text("quantity"),
        categories_tags,
    })
}

/// Whether a cached record fetched at `fetched_at` is still served at
/// `now`: for `CACHE_TTL` when it holds a product, `MISS_TTL` when not.
pub fn is_fresh(fetched_at: DateTime<Utc>, found: bool, now: DateTime<Utc>) -> bool {
    let ttl = if found { CACHE_TTL } else { MISS_TTL };
    now < fetched_at + ttl
}

/// At most `RATE_LIMIT` reads per `RATE_WINDOW`, sliding. In-process, like
/// the login throttle: `infra/docker-compose.yml` runs one `api`.
#[derive(Default)]
pub struct Throttle {
    admitted: Mutex<VecDeque<Instant>>,
}

impl Throttle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a read may go out at `now`; counted when it may.
    pub fn admit(&self, now: Instant) -> bool {
        let mut admitted = self
            .admitted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while admitted
            .front()
            .is_some_and(|&at| now.saturating_duration_since(at) >= RATE_WINDOW)
        {
            admitted.pop_front();
        }
        if admitted.len() >= RATE_LIMIT {
            return false;
        }
        admitted.push_back(now);
        true
    }
}

/// Why a lookup brought nothing back that may be cached.
#[derive(Debug)]
pub enum LookupError {
    /// Over `RATE_LIMIT`: not asked.
    Throttled,
    /// Unreachable, too slow, or an answer other than a product or a 404.
    Unavailable,
}

/// Ask Open Food Facts for `code`. `Ok(None)` is a definite "unknown or
/// nameless", worth caching; an `Err` is not.
pub async fn fetch(
    base: &str,
    code: &str,
    throttle: &Throttle,
) -> Result<Option<ScannedProduct>, LookupError> {
    if !throttle.admit(Instant::now()) {
        return Err(LookupError::Throttled);
    }
    let response = crate::outbound_http::client()
        .get(product_url(base, code))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "open food facts unreachable");
            LookupError::Unavailable
        })?;
    // An unknown code is a 404 carrying `status: 0`.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), "open food facts answered an error");
        return Err(LookupError::Unavailable);
    }
    let body: Value = response.json().await.map_err(|e| {
        tracing::warn!(error = %e, "open food facts answered something other than JSON");
        LookupError::Unavailable
    })?;
    Ok(extract_product(&body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn found(product: Value) -> Value {
        json!({ "code": "3017620422003", "status": 1, "status_verbose": "product found", "product": product })
    }

    #[test]
    fn a_found_product_gives_its_name_quantity_and_categories() {
        let body = found(json!({
            "product_name": "Nutella",
            "quantity": "400 g",
            "categories_tags": ["en:breakfasts", "en:spreads"],
        }));
        assert_eq!(
            extract_product(&body),
            Some(ScannedProduct {
                name: "Nutella".into(),
                quantity: Some("400 g".into()),
                categories_tags: vec!["en:breakfasts".into(), "en:spreads".into()],
            })
        );
    }

    #[test]
    fn the_french_name_wins_over_the_generic_one() {
        let body = found(
            json!({ "product_name": "Hazelnut spread", "product_name_fr": " Pâte à tartiner " }),
        );
        assert_eq!(extract_product(&body).unwrap().name, "Pâte à tartiner");
    }

    #[test]
    fn a_blank_french_name_falls_back_on_the_generic_one() {
        let body = found(json!({ "product_name": "Nutella", "product_name_fr": "  " }));
        assert_eq!(extract_product(&body).unwrap().name, "Nutella");
    }

    #[test]
    fn a_nameless_record_is_no_product() {
        for product in [
            json!({}),
            json!({ "product_name": "" }),
            json!({ "product_name": "   ", "product_name_fr": "" }),
            json!({ "product_name": null, "quantity": "1 kg" }),
            json!({ "product_name": 42 }),
        ] {
            assert_eq!(extract_product(&found(product.clone())), None, "{product}");
        }
    }

    #[test]
    fn an_unknown_code_is_no_product() {
        let body =
            json!({ "code": "3017620422004", "status": 0, "status_verbose": "product not found" });
        assert_eq!(extract_product(&body), None);
        // `status: 1` without a product object, or not an object at all.
        assert_eq!(extract_product(&json!({ "status": 1 })), None);
        assert_eq!(extract_product(&json!([])), None);
    }

    #[test]
    fn a_blank_or_missing_quantity_is_none() {
        for product in [
            json!({ "product_name": "Riz" }),
            json!({ "product_name": "Riz", "quantity": " " }),
            json!({ "product_name": "Riz", "quantity": null }),
        ] {
            assert_eq!(
                extract_product(&found(product.clone())).unwrap().quantity,
                None,
                "{product}"
            );
        }
    }

    #[test]
    fn categories_keep_the_strings_and_drop_the_rest() {
        let body = found(
            json!({ "product_name": "Riz", "categories_tags": ["en:rices", 3, null, "fr:riz"] }),
        );
        assert_eq!(
            extract_product(&body).unwrap().categories_tags,
            vec!["en:rices".to_string(), "fr:riz".to_string()]
        );
        let none = found(json!({ "product_name": "Riz", "categories_tags": "en:rices" }));
        assert!(extract_product(&none).unwrap().categories_tags.is_empty());
    }

    #[test]
    fn a_found_record_is_served_for_thirty_days() {
        let fetched = Utc::now();
        assert!(is_fresh(fetched, true, fetched + Duration::days(29)));
        assert!(!is_fresh(fetched, true, fetched + Duration::days(30)));
    }

    #[test]
    fn a_miss_is_remembered_for_one_day_only() {
        let fetched = Utc::now();
        assert!(is_fresh(fetched, false, fetched + Duration::hours(23)));
        assert!(!is_fresh(fetched, false, fetched + Duration::days(1)));
    }

    #[test]
    fn the_throttle_admits_fifteen_reads_a_minute() {
        let throttle = Throttle::new();
        let start = Instant::now();
        for i in 0..RATE_LIMIT {
            assert!(throttle.admit(start), "read {i}");
        }
        assert!(!throttle.admit(start), "the 16th read within the minute");
        assert!(!throttle.admit(start + RATE_WINDOW - StdDuration::from_millis(1)));
        assert!(
            throttle.admit(start + RATE_WINDOW),
            "a minute later, the window has slid"
        );
    }

    #[test]
    fn a_refused_read_does_not_count() {
        let throttle = Throttle::new();
        let start = Instant::now();
        for _ in 0..RATE_LIMIT {
            throttle.admit(start);
        }
        for _ in 0..100 {
            assert!(!throttle.admit(start + StdDuration::from_secs(30)));
        }
        // Only the fifteen admitted at `start` leave the window.
        let later = start + RATE_WINDOW;
        for i in 0..RATE_LIMIT {
            assert!(throttle.admit(later), "read {i}");
        }
    }

    #[test]
    fn the_product_url_names_the_code_and_the_fields() {
        assert_eq!(
            product_url("https://world.openfoodfacts.org/", "3017620422003"),
            "https://world.openfoodfacts.org/api/v2/product/3017620422003?fields=product_name,product_name_fr,quantity,categories_tags"
        );
    }

    #[test]
    fn the_user_agent_names_the_application() {
        assert!(USER_AGENT.starts_with("manage_our_home/"), "{USER_AGENT}");
    }

    /// Open Food Facts asks for `AppName/Version (ContactEmail)`, and the
    /// contact is the controller's dedicated address, published under a
    /// pseudonym (#379). Until that address is chosen, the outbound
    /// User-Agents — this one and the recipe import's — point at the code
    /// repository, whose URL carries its owner's handle. The day the privacy
    /// policy's contact placeholder is filled, both must carry that address
    /// instead: `docs/v2-deployment.md` #16 sends the author here.
    #[test]
    fn the_user_agents_carry_the_contact_address_once_it_is_filled() {
        use manage_our_home_shared::validation::rgpd::release_placeholders;
        let policy = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/privacy-policy.md"
        ));
        let contact_pending =
            release_placeholders(policy).contains(&"adresse de contact".to_string());
        for agent in [USER_AGENT, crate::recipes::import::USER_AGENT] {
            assert_eq!(
                agent.contains("github.com"),
                contact_pending,
                "the User-Agent `{agent}` and the privacy policy disagree on \
                 whether the contact address is still to be filled"
            );
            assert_eq!(agent.contains('@'), !contact_pending, "{agent}");
        }
    }
}
