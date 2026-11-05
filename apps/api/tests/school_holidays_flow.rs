//! The school holidays refresh (#563), against a real database and a local
//! stand-in for data.education.gouv.fr: no request leaves the machine.
//!
//! A pass that succeeds replaces the cached periods of the two school years
//! it asked for and keeps the older ones; a pass that fails — service
//! unreachable, error status, an answer that is not the expected JSON —
//! leaves the cache exactly as it was.

mod common;

use std::sync::{Arc, Mutex};

use axum::extract::RawQuery;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::NaiveDate;
use manage_our_home::agenda::school_holidays::RECORDS_PATH;
use manage_our_home::jobs::school_holidays::refresh;
use manage_our_home::outbound_http::client;
use sqlx::PgPool;

const RECORDED: &str = include_str!("fixtures/school_calendar/2026-2027_2027-2028.json");

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// The day the fixture was recorded: its two school years are the ones a
/// pass asks for that day.
fn recorded_on() -> NaiveDate {
    day(2026, 11, 5)
}

/// Answers every request to `RECORDS_PATH` with `status` and `body`, and
/// keeps the query string of the last one.
async fn stub(status: StatusCode, body: &'static str) -> (String, Arc<Mutex<Option<String>>>) {
    let seen = Arc::new(Mutex::new(None));
    let record = seen.clone();
    let app = axum::Router::new().route(
        RECORDS_PATH,
        axum::routing::get(move |RawQuery(query): RawQuery| {
            let record = record.clone();
            async move {
                *record.lock().unwrap() = query;
                (
                    status,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

/// The whole cache, in a stable order.
async fn cache(db: &PgPool) -> Vec<(String, String, NaiveDate, NaiveDate, String)> {
    sqlx::query_as(
        "SELECT zone, label, starts_on, ends_on, school_year FROM school_holidays
         ORDER BY zone, starts_on, label",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

/// A period of a past year and a stale one of the current year, cached
/// before the pass.
async fn seed(db: &PgPool) {
    sqlx::query(
        "INSERT INTO school_holidays (zone, label, starts_on, ends_on, school_year) VALUES
         ('B', 'Vacances de Noël', '2025-12-20', '2026-01-04', '2025-2026'),
         ('B', 'Vacances périmées', '2026-11-20', '2026-11-22', '2026-2027')",
    )
    .execute(db)
    .await
    .unwrap();
}

/// AC (#563): the recorded answer replaces the periods of the two years
/// asked for — the stale one goes — and keeps the past year's.
#[sqlx::test]
async fn a_pass_replaces_the_years_it_asked_for(db: PgPool) {
    seed(&db).await;
    let (base, seen) = stub(StatusCode::OK, RECORDED).await;

    let cached = refresh(&db, client(), &base, recorded_on()).await.unwrap();
    assert_eq!(cached, 30);

    let rows = cache(&db).await;
    assert_eq!(rows.len(), 31);
    assert!(!rows.iter().any(|r| r.1 == "Vacances périmées"));
    assert!(rows.contains(&(
        "B".into(),
        "Vacances de Noël".into(),
        day(2025, 12, 20),
        day(2026, 1, 4),
        "2025-2026".into()
    )));
    assert!(rows.contains(&(
        "C".into(),
        "Vacances d'Hiver".into(),
        day(2027, 2, 6),
        day(2027, 2, 21),
        "2026-2027".into()
    )));

    // The request names the two years and the three zones, nothing else.
    let query = seen.lock().unwrap().clone().unwrap();
    let params: Vec<(String, String)> = reqwest::Url::parse(&format!("http://stub/?{query}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    assert!(params.contains(&(
        "where".into(),
        r#"annee_scolaire in ("2026-2027","2027-2028") and zones in ("Zone A","Zone B","Zone C")"#
            .into()
    )));

    // A second pass over the same answer changes nothing.
    let before = cache(&db).await;
    refresh(&db, client(), &base, recorded_on()).await.unwrap();
    assert_eq!(cache(&db).await, before);
}

/// AC (#563): the service unreachable — nothing listens on port 1 — the
/// pass fails and the cache stays as it was.
#[sqlx::test]
async fn a_network_failure_keeps_the_cache(db: PgPool) {
    seed(&db).await;
    let before = cache(&db).await;
    assert!(refresh(&db, client(), "http://127.0.0.1:1", recorded_on())
        .await
        .is_err());
    assert_eq!(cache(&db).await, before);
}

/// AC (#563): an error status, a body that is not JSON, JSON of another
/// shape, or an answer missing a zone: the pass fails and the cache stays
/// as it was.
#[sqlx::test]
async fn an_unexpected_answer_keeps_the_cache(db: PgPool) {
    let (base, _) = stub(StatusCode::OK, RECORDED).await;
    refresh(&db, client(), &base, recorded_on()).await.unwrap();
    let before = cache(&db).await;
    assert_eq!(before.len(), 30);

    for (status, body) in [
        (StatusCode::INTERNAL_SERVER_ERROR, "{}"),
        (StatusCode::SERVICE_UNAVAILABLE, RECORDED),
        (StatusCode::OK, "<html>maintenance</html>"),
        (
            StatusCode::OK,
            r#"{"error_code": "ODSQLError", "message": "Unknown field"}"#,
        ),
        (StatusCode::OK, r#"{"total_count": 0, "results": []}"#),
    ] {
        let (base, _) = stub(status, body).await;
        let outcome = refresh(&db, client(), &base, recorded_on()).await;
        assert!(outcome.is_err(), "{status} {body:.40}");
        assert_eq!(cache(&db).await, before, "{status} {body:.40}");
    }
}
