//! `POST /csp-report` — where browsers report what the Content-Security-Policy
//! of `infra/Caddyfile` blocked (#325).
//!
//! The policy names this endpoint twice, because browsers disagree on how to
//! report: `report-to csp` (the Reporting API, with the `Reporting-Endpoints`
//! header, `application/reports+json`: an array of reports) and the older
//! `report-uri` that Firefox and Safari still use (`application/csp-report`:
//! one `{"csp-report": {…}}` object). Both bodies are read here, whatever
//! `Content-Type` they arrive with.
//!
//! Nothing leaves the host: the reports go to this API, on the site's own
//! origin, and end up as log lines — no third-party collector. What is
//! logged is a summary built by [`violations`], not the report:
//!
//! - **no IP address**, nor any other request header: the handler reads the
//!   body and nothing else;
//! - URLs stripped of their query, fragment and credentials
//!   ([`redact_url`]): `/verify-email?token=…` carries a secret in its query
//!   string, and the reset-password link one in its fragment;
//! - every field cut to [`MAX_FIELD_CHARS`], and at most
//!   [`MAX_VIOLATIONS_PER_REQUEST`] lines per request, under a body limit of
//!   [`MAX_REPORT_BODY_BYTES`] — the route answers anyone, so the size of
//!   what one request can write to the log is bounded here.

use axum::body::Bytes;
use axum::http::StatusCode;
use serde_json::Value;

/// A report is a few hundred bytes; the Reporting API may batch several.
pub const MAX_REPORT_BODY_BYTES: usize = 16 * 1024;

/// Log lines one request may produce, whatever its batch holds.
pub const MAX_VIOLATIONS_PER_REQUEST: usize = 10;

/// Characters kept of each logged field.
pub const MAX_FIELD_CHARS: usize = 200;

/// One reported violation, as logged.
#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    /// The page the violation happened on.
    pub document: String,
    /// The directive that blocked it (`script-src-elem`, `style-src-attr`…).
    pub directive: String,
    /// What was blocked: a URL, or a keyword (`inline`, `eval`).
    pub blocked: String,
    /// The script file the violation was raised from, if any.
    pub source: Option<String>,
    pub line: Option<u64>,
    /// `enforce` or `report`.
    pub disposition: String,
}

/// `url` without what may carry a secret: query, fragment and userinfo.
/// A `data:` or `blob:` URL is reduced to its scheme — the rest is content.
/// Values that are not URLs (`inline`, `eval`, `self`) pass through.
/// Cut to [`MAX_FIELD_CHARS`] in every case.
pub fn redact_url(url: &str) -> String {
    let url = url.trim();
    if let Some((scheme, rest)) = url.split_once(':') {
        if is_scheme(scheme) && !rest.starts_with("//") {
            return cut(scheme);
        }
    }
    let url = &url[..url.find(['?', '#']).unwrap_or(url.len())];
    let Some((scheme, rest)) = url.split_once("://") else {
        return cut(url);
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    cut(&format!("{scheme}://{host}{path}"))
}

/// RFC 3986's `scheme`: a letter, then letters, digits, `+`, `-` or `.`.
fn is_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// `s`, cut to [`MAX_FIELD_CHARS`] characters.
fn cut(s: &str) -> String {
    s.chars().take(MAX_FIELD_CHARS).collect()
}

/// The violations a report body describes, legacy `report-uri` object or
/// Reporting API array alike; nothing for a body that is neither, and only
/// the `csp-violation` entries of a Reporting API batch.
pub fn violations(body: &[u8]) -> Vec<Violation> {
    let Ok(json) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    let reports: Vec<(&Value, &Names)> = match &json {
        Value::Object(legacy) => legacy
            .get("csp-report")
            .map(|report| (report, &LEGACY))
            .into_iter()
            .collect(),
        Value::Array(batch) => batch
            .iter()
            .filter(|r| r.get("type").and_then(Value::as_str) == Some("csp-violation"))
            .filter_map(|r| r.get("body"))
            .map(|report| (report, &REPORTING_API))
            .collect(),
        _ => Vec::new(),
    };
    reports
        .into_iter()
        .filter_map(|(report, names)| violation(report, names))
        .take(MAX_VIOLATIONS_PER_REQUEST)
        .collect()
}

/// Where each field sits in one of the two report shapes.
struct Names {
    document: &'static str,
    directive: &'static str,
    /// Browsers that send no effective directive send the violated one,
    /// sometimes followed by its source list (`style-src 'self'`).
    violated: &'static str,
    blocked: &'static str,
    source: &'static str,
    line: &'static str,
}

const LEGACY: Names = Names {
    document: "document-uri",
    directive: "effective-directive",
    violated: "violated-directive",
    blocked: "blocked-uri",
    source: "source-file",
    line: "line-number",
};

const REPORTING_API: Names = Names {
    document: "documentURL",
    directive: "effectiveDirective",
    violated: "violatedDirective",
    blocked: "blockedURL",
    source: "sourceFile",
    line: "lineNumber",
};

fn violation(report: &Value, names: &Names) -> Option<Violation> {
    let report = report.as_object()?;
    let text = |key: &str| report.get(key).and_then(Value::as_str);
    let directive = text(names.directive)
        .or_else(|| text(names.violated).and_then(|d| d.split_whitespace().next()))
        .unwrap_or("");
    Some(Violation {
        document: redact_url(text(names.document).unwrap_or("")),
        directive: cut(directive),
        blocked: redact_url(text(names.blocked).unwrap_or("")),
        source: text(names.source).map(redact_url),
        line: report.get(names.line).and_then(Value::as_u64),
        disposition: cut(text("disposition").unwrap_or("")),
    })
}

/// `POST /csp-report`. Always 204: the browser does nothing with the answer,
/// and a report that cannot be read is not worth telling its sender about.
pub async fn receive(body: Bytes) -> StatusCode {
    for v in violations(&body) {
        tracing::warn!(
            document = %v.document,
            directive = %v.directive,
            blocked = %v.blocked,
            source = v.source.as_deref().unwrap_or(""),
            line = v.line.unwrap_or(0),
            disposition = %v.disposition,
            "content security policy violation reported"
        );
    }
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_url_drops_query_fragment_and_credentials() {
        assert_eq!(
            redact_url("https://maison.example.org/verify-email?token=abc#x"),
            "https://maison.example.org/verify-email"
        );
        assert_eq!(
            redact_url("https://maison.example.org/reset-password#token=abc"),
            "https://maison.example.org/reset-password"
        );
        assert_eq!(
            redact_url("https://user:secret@evil.test/x.js"),
            "https://evil.test/x.js"
        );
        assert_eq!(
            redact_url("https://maison.example.org/"),
            "https://maison.example.org/"
        );
    }

    #[test]
    fn redact_url_keeps_keywords_and_reduces_data_urls_to_their_scheme() {
        assert_eq!(redact_url("inline"), "inline");
        assert_eq!(redact_url("eval"), "eval");
        assert_eq!(redact_url("data:text/javascript,alert(1)"), "data");
        assert_eq!(redact_url("blob:https://maison.example.org/1234"), "blob");
        assert_eq!(redact_url(""), "");
    }

    #[test]
    fn redact_url_cuts_long_values_on_a_char_boundary() {
        let long = format!("https://evil.test/{}", "é".repeat(400));
        let out = redact_url(&long);
        assert_eq!(out.chars().count(), MAX_FIELD_CHARS);
        assert!(long.starts_with(&out));
    }

    #[test]
    fn violations_reads_a_legacy_report_uri_body() {
        let body = br#"{"csp-report": {
            "document-uri": "https://maison.example.org/messagerie?group=1",
            "referrer": "https://maison.example.org/",
            "violated-directive": "script-src-elem",
            "effective-directive": "script-src-elem",
            "original-policy": "default-src 'self'",
            "disposition": "enforce",
            "blocked-uri": "inline",
            "line-number": 12,
            "source-file": "https://maison.example.org/messagerie?group=1",
            "status-code": 200
        }}"#;
        assert_eq!(
            violations(body),
            vec![Violation {
                document: "https://maison.example.org/messagerie".into(),
                directive: "script-src-elem".into(),
                blocked: "inline".into(),
                source: Some("https://maison.example.org/messagerie".into()),
                line: Some(12),
                disposition: "enforce".into(),
            }]
        );
    }

    #[test]
    fn violations_falls_back_on_the_violated_directive() {
        // Safari sends no `effective-directive`.
        let body = br#"{"csp-report": {
            "document-uri": "https://maison.example.org/login",
            "violated-directive": "style-src 'self'",
            "blocked-uri": "https://evil.test/x.css"
        }}"#;
        let found = violations(body);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].directive, "style-src");
        assert_eq!(found[0].blocked, "https://evil.test/x.css");
        assert_eq!(found[0].source, None);
        assert_eq!(found[0].line, None);
        assert_eq!(found[0].disposition, "");
    }

    #[test]
    fn violations_reads_a_reporting_api_batch_and_skips_other_report_types() {
        let body = br#"[
            {"type": "csp-violation", "age": 3, "url": "https://maison.example.org/agenda?date=2026-10-04",
             "user_agent": "Mozilla/5.0",
             "body": {"documentURL": "https://maison.example.org/agenda?date=2026-10-04",
                      "effectiveDirective": "script-src-attr",
                      "blockedURL": "inline",
                      "sourceFile": "https://maison.example.org/assets/enhance-0123.js",
                      "lineNumber": 3, "columnNumber": 7,
                      "disposition": "enforce", "statusCode": 200}},
            {"type": "deprecation", "body": {"id": "x", "message": "y"}},
            {"type": "csp-violation",
             "body": {"documentURL": "https://maison.example.org/",
                      "effectiveDirective": "img-src",
                      "blockedURL": "https://tracker.test/p.gif?u=42",
                      "disposition": "report"}}
        ]"#;
        assert_eq!(
            violations(body),
            vec![
                Violation {
                    document: "https://maison.example.org/agenda".into(),
                    directive: "script-src-attr".into(),
                    blocked: "inline".into(),
                    source: Some("https://maison.example.org/assets/enhance-0123.js".into()),
                    line: Some(3),
                    disposition: "enforce".into(),
                },
                Violation {
                    document: "https://maison.example.org/".into(),
                    directive: "img-src".into(),
                    blocked: "https://tracker.test/p.gif".into(),
                    source: None,
                    line: None,
                    disposition: "report".into(),
                },
            ]
        );
    }

    #[test]
    fn violations_ignores_what_is_not_a_report() {
        for body in [
            &b""[..],
            b"not json",
            b"{}",
            b"[]",
            b"42",
            br#"{"csp-report": "x"}"#,
            br#"[{"type": "csp-violation", "body": 1}]"#,
        ] {
            assert_eq!(
                violations(body),
                vec![],
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn violations_logs_at_most_the_per_request_cap() {
        let one = r#"{"type":"csp-violation","body":{"documentURL":"https://h/","effectiveDirective":"img-src","blockedURL":"https://e/"}}"#;
        let batch = format!("[{}]", vec![one; 50].join(","));
        assert_eq!(
            violations(batch.as_bytes()).len(),
            MAX_VIOLATIONS_PER_REQUEST
        );
    }

    #[test]
    fn violations_cuts_every_field() {
        let long = "a".repeat(1000);
        let body = format!(
            r#"{{"csp-report": {{"document-uri": "{long}", "effective-directive": "{long}",
                "blocked-uri": "{long}", "source-file": "{long}", "disposition": "{long}"}}}}"#
        );
        let v = &violations(body.as_bytes())[0];
        for field in [
            &v.document,
            &v.directive,
            &v.blocked,
            v.source.as_ref().unwrap(),
            &v.disposition,
        ] {
            assert_eq!(field.chars().count(), MAX_FIELD_CHARS);
        }
    }
}
