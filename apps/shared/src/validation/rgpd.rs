//! Pure, dependency-light logic for the RGPD self-service screens (front epic
//! F10, issue #25), shared by `apps/web`'s SSR pages. Written test-first per
//! `.claude/CLAUDE.md`'s TDD process. Everything those screens need beyond
//! rendering lives here so the UI and the fixed backend
//! (`apps/api/src/rgpd/`, `apps/api/src/auth/mod.rs::delete_account` /
//! `cancel_delete_account`, `apps/api/src/jobs/account_purge.rs`) can never
//! drift.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Europe::Paris;

/// Mirror of `apps/api/src/jobs/account_purge.rs::PURGE_GRACE_DAYS` (and of
/// `apps/api/src/auth/mod.rs::ACCOUNT_DELETION_GRACE_DAYS`): how long a deletion
/// request sits cancellable before the purge job anonymizes the account.
pub const GRACE_PERIOD_DAYS: i64 = 30;

/// When the purge job will actually anonymize an account whose deletion was
/// requested at `requested_at` — the date the UI promises the user, computed
/// from the same 30-day constant the job uses.
pub fn deletion_deadline(requested_at: DateTime<Utc>) -> DateTime<Utc> {
    requested_at + Duration::days(GRACE_PERIOD_DAYS)
}

/// Formats a UTC instant in Europe/Paris (`26/07/2026 à 14:05`), the fixed v1
/// display timezone — same convention as `validation::user_admin` and
/// `validation::messagerie`.
pub fn format_rgpd_datetime(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&Paris)
        .format("%d/%m/%Y à %H:%M")
        .to_string()
}

/// Day-only variant of [`format_rgpd_datetime`], for the purge deadline (a
/// 30-day horizon: the hour would be noise). The calendar day is the **Paris**
/// one, so a request made late in the evening doesn't advertise the previous
/// day's date.
pub fn format_rgpd_date(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&Paris).format("%d/%m/%Y").to_string()
}

/// Filename the export download is served under (`Content-Disposition`). Dated
/// so a user who exports twice keeps both files.
pub fn export_filename(now: DateTime<Utc>) -> String {
    now.with_timezone(&Paris)
        .format("mes-donnees-manage-our-home-%Y-%m-%d.json")
        .to_string()
}

/// Whether the "request deletion" form should render: only when no request is
/// pending. A second `POST /account/delete` would just reset the clock — and
/// silently postpone the purge — so the UI offers the cancel path instead.
pub fn can_request_deletion(deletion_requested_at: Option<DateTime<Utc>>) -> bool {
    deletion_requested_at.is_none()
}

/// Mirror of the backend's `WHERE deletion_requested_at IS NOT NULL AND
/// deleted_at IS NULL` guard on `POST /account/delete/cancel` (404 otherwise):
/// only a pending request can be cancelled.
pub fn can_cancel_deletion(deletion_requested_at: Option<DateTime<Utc>>) -> bool {
    deletion_requested_at.is_some()
}

/// Validates the deletion confirmation before anything is sent to apps/api, and
/// returns the `current_password` to put in the request body (`None` for a
/// Google-only account, which has no password to check).
///
/// Mirrors `apps/api/src/auth/mod.rs::delete_account`: it verifies
/// `current_password` **only** when the account has a `password_hash`, and its
/// comment states that "Google-only accounts: re-consent is validated on the
/// frontend flow before this endpoint is called" — hence the explicit consent
/// checkbox, required for both kinds of account so an irreversible action is
/// never one stray click away. The password is passed through untrimmed: spaces
/// are legitimate characters and the backend compares against the stored hash.
pub fn validate_deletion_confirmation(
    has_password: bool,
    password: &str,
    consent: bool,
) -> Result<Option<String>, &'static str> {
    if has_password && password.trim().is_empty() {
        return Err("password_required");
    }
    if !consent {
        return Err("consent_required");
    }
    if has_password {
        Ok(Some(password.to_string()))
    } else {
        Ok(None)
    }
}

/// Renders the markdown subset used by `docs/privacy-policy.md` — which
/// `apps/api` serves verbatim over `GET /privacy-policy` — into HTML:
/// `#`/`##`/`###` headings, wrapped paragraphs, `- ` bullet lists (with
/// indented continuation lines), pipe tables with a header row, and the inline
/// `` `code` ``, `**strong**` and `[text](url)` markers.
///
/// Everything else is emitted as escaped literal text: the source is trusted
/// (it ships in the repo, compiled into the API binary via `include_str!`), but
/// nothing here can turn it into markup — raw HTML never passes through, and a
/// link whose scheme isn't `http`/`https`/`mailto` or a local path stays plain
/// text rather than becoming an `href`. The `renders_the_real_privacy_policy_*`
/// test is the regression guard that the shipped document stays in this subset.
pub fn render_markdown(md: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut block = Block::None;

    for raw in md.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();

        if trimmed.is_empty() {
            flush(&mut block, &mut out);
            continue;
        }

        if let Some((level, text)) = heading(trimmed) {
            flush(&mut block, &mut out);
            out.push(format!("<h{level}>{}</h{level}>", inline(text)));
            continue;
        }

        if let Some(item) = trimmed.strip_prefix("- ") {
            if !matches!(block, Block::List(_)) {
                flush(&mut block, &mut out);
                block = Block::List(Vec::new());
            }
            if let Block::List(items) = &mut block {
                items.push(item.trim().to_string());
            }
            continue;
        }

        if trimmed.starts_with('|') {
            let cells = table_cells(trimmed);
            if !matches!(block, Block::Table { .. }) {
                flush(&mut block, &mut out);
                block = Block::Table {
                    header: Vec::new(),
                    rows: Vec::new(),
                };
            }
            if let Block::Table { header, rows } = &mut block {
                if is_separator_row(&cells) {
                    // `|---|---|` — carries no content.
                } else if header.is_empty() {
                    *header = cells;
                } else {
                    rows.push(cells);
                }
            }
            continue;
        }

        // An indented line inside a list continues its last item (the shipped
        // document wraps long bullets that way).
        if let Block::List(items) = &mut block {
            if line.starts_with(char::is_whitespace) {
                if let Some(last) = items.last_mut() {
                    last.push(' ');
                    last.push_str(trimmed);
                    continue;
                }
            }
        }

        if !matches!(block, Block::Paragraph(_)) {
            flush(&mut block, &mut out);
            block = Block::Paragraph(Vec::new());
        }
        if let Block::Paragraph(lines) = &mut block {
            lines.push(trimmed.to_string());
        }
    }
    flush(&mut block, &mut out);

    out.join("\n")
}

enum Block {
    None,
    Paragraph(Vec<String>),
    List(Vec<String>),
    Table {
        header: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}

fn flush(block: &mut Block, out: &mut Vec<String>) {
    match std::mem::replace(block, Block::None) {
        Block::None => {}
        Block::Paragraph(lines) => out.push(format!("<p>{}</p>", inline(&lines.join(" ")))),
        Block::List(items) => {
            let body: String = items
                .iter()
                .map(|i| format!("<li>{}</li>\n", inline(i)))
                .collect();
            out.push(format!("<ul>\n{body}</ul>"));
        }
        Block::Table { header, rows } => {
            let head: String = header
                .iter()
                .map(|c| format!("<th>{}</th>", inline(c)))
                .collect();
            let body: String = rows
                .iter()
                .map(|row| {
                    let cells: String = row
                        .iter()
                        .map(|c| format!("<td>{}</td>", inline(c)))
                        .collect();
                    format!("<tr>{cells}</tr>\n")
                })
                .collect();
            out.push(format!(
                "<div class=\"table-wrap\"><table>\n<thead><tr>{head}</tr></thead>\n<tbody>\n{body}</tbody>\n</table></div>"
            ));
        }
    }
}

fn heading(trimmed: &str) -> Option<(usize, &str)> {
    for level in 1..=3usize {
        let prefix = format!("{} ", "#".repeat(level));
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            if !rest.starts_with('#') {
                return Some((level, rest.trim()));
            }
        }
    }
    None
}

fn table_cells(trimmed: &str) -> Vec<String> {
    trimmed
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}

fn is_separator_row(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim_matches(':');
            !c.is_empty() && c.chars().all(|ch| ch == '-')
        })
}

/// Escapes the text, then resolves the inline markers. Escaping first is what
/// makes the scan safe: no `<`/`>` can survive from the source, so nothing the
/// document contains can close or open a tag.
fn inline(text: &str) -> String {
    let chars: Vec<char> = escape_html(text).chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '`' => {
                if let Some(end) = find(&chars, i + 1, &['`']) {
                    out.push_str("<code>");
                    out.extend(&chars[i + 1..end]);
                    out.push_str("</code>");
                    i = end + 1;
                    continue;
                }
            }
            '*' if chars.get(i + 1) == Some(&'*') => {
                if let Some(end) = find(&chars, i + 2, &['*', '*']) {
                    out.push_str("<strong>");
                    out.extend(&chars[i + 2..end]);
                    out.push_str("</strong>");
                    i = end + 2;
                    continue;
                }
            }
            '[' => {
                if let Some((label, url, next)) = link(&chars, i) {
                    if is_safe_url(&url) {
                        out.push_str(&format!("<a href=\"{url}\">{label}</a>"));
                        i = next;
                        continue;
                    }
                }
            }
            _ => {}
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Index of the next occurrence of `needle` in `chars` at or after `from`.
fn find(chars: &[char], from: usize, needle: &[char]) -> Option<usize> {
    (from..chars.len().saturating_sub(needle.len() - 1))
        .find(|&i| chars[i..i + needle.len()] == *needle)
}

/// Parses `[label](url)` starting at `start` (which must be the `[`), returning
/// the label, the url, and the index just past the closing `)`.
fn link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let close = find(chars, start + 1, &[']'])?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = find(chars, close + 2, &[')'])?;
    Some((
        chars[start + 1..close].iter().collect(),
        chars[close + 2..end].iter().collect(),
        end + 1,
    ))
}

/// Only absolute `http(s)`/`mailto:` URLs and local paths/fragments become an
/// `href`; anything else (`javascript:`, `data:`, …) stays literal text.
fn is_safe_url(url: &str) -> bool {
    url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("mailto:")
        || url.starts_with('/')
        || url.starts_with('#')
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    // -- deletion_deadline ---------------------------------------------------

    #[test]
    fn deadline_is_the_request_plus_the_grace_period() {
        assert_eq!(
            deletion_deadline(at(2026, 7, 26, 10, 0)),
            at(2026, 8, 25, 10, 0)
        );
    }

    #[test]
    fn deadline_crosses_a_month_and_year_boundary() {
        assert_eq!(
            deletion_deadline(at(2026, 12, 20, 8, 30)),
            at(2027, 1, 19, 8, 30)
        );
    }

    // -- format_rgpd_datetime / _date ---------------------------------------

    #[test]
    fn datetime_is_rendered_in_paris_summer_time() {
        // 2026-07-26 12:05 UTC is 14:05 in Paris (CEST, UTC+2).
        assert_eq!(
            format_rgpd_datetime(at(2026, 7, 26, 12, 5)),
            "26/07/2026 à 14:05"
        );
    }

    #[test]
    fn datetime_is_rendered_in_paris_winter_time() {
        assert_eq!(
            format_rgpd_datetime(at(2026, 1, 5, 12, 5)),
            "05/01/2026 à 13:05"
        );
    }

    #[test]
    fn date_drops_the_time_of_day() {
        assert_eq!(format_rgpd_date(at(2026, 8, 25, 12, 5)), "25/08/2026");
    }

    #[test]
    fn date_uses_the_paris_calendar_day_not_the_utc_one() {
        // 22:30 UTC on 2026-08-25 is already 00:30 on the 26th in Paris.
        assert_eq!(format_rgpd_date(at(2026, 8, 25, 22, 30)), "26/08/2026");
    }

    // -- export_filename -----------------------------------------------------

    #[test]
    fn export_filename_is_dated_and_json() {
        assert_eq!(
            export_filename(at(2026, 7, 26, 12, 0)),
            "mes-donnees-manage-our-home-2026-07-26.json"
        );
    }

    #[test]
    fn export_filename_uses_the_paris_calendar_day() {
        assert_eq!(
            export_filename(at(2026, 7, 26, 22, 30)),
            "mes-donnees-manage-our-home-2026-07-27.json"
        );
    }

    // -- can_request_deletion / can_cancel_deletion -------------------------

    #[test]
    fn an_account_with_no_pending_request_can_request_deletion() {
        assert!(can_request_deletion(None));
        assert!(!can_cancel_deletion(None));
    }

    #[test]
    fn an_account_with_a_pending_request_can_only_cancel() {
        let requested = Some(at(2026, 7, 26, 10, 0));
        assert!(!can_request_deletion(requested));
        assert!(can_cancel_deletion(requested));
    }

    // -- validate_deletion_confirmation -------------------------------------

    #[test]
    fn a_password_account_must_type_its_password() {
        assert_eq!(
            validate_deletion_confirmation(true, "", true),
            Err("password_required")
        );
        assert_eq!(
            validate_deletion_confirmation(true, "   ", true),
            Err("password_required")
        );
    }

    #[test]
    fn a_password_account_sends_the_typed_password_verbatim() {
        // Never trimmed: leading/trailing spaces are legitimate password
        // characters and the backend compares against the stored hash.
        assert_eq!(
            validate_deletion_confirmation(true, " pass phrase ", true),
            Ok(Some(" pass phrase ".to_string()))
        );
    }

    #[test]
    fn a_google_only_account_confirms_by_consent_and_sends_no_password() {
        assert_eq!(validate_deletion_confirmation(false, "", true), Ok(None));
    }

    #[test]
    fn a_google_only_account_must_tick_the_consent_box() {
        assert_eq!(
            validate_deletion_confirmation(false, "", false),
            Err("consent_required")
        );
    }

    #[test]
    fn a_password_account_also_needs_the_consent_box() {
        assert_eq!(
            validate_deletion_confirmation(true, "pass phrase", false),
            Err("consent_required")
        );
    }

    // -- render_markdown -----------------------------------------------------

    #[test]
    fn renders_headings_by_level() {
        assert_eq!(render_markdown("# Titre"), "<h1>Titre</h1>");
        assert_eq!(render_markdown("## Section"), "<h2>Section</h2>");
        assert_eq!(render_markdown("### Détail"), "<h3>Détail</h3>");
    }

    #[test]
    fn joins_a_wrapped_paragraph_into_one_block() {
        assert_eq!(
            render_markdown("Une phrase\ncoupée en deux.\n\nUne autre."),
            "<p>Une phrase coupée en deux.</p>\n<p>Une autre.</p>"
        );
    }

    #[test]
    fn renders_a_bullet_list_with_continuation_lines() {
        assert_eq!(
            render_markdown("- premier\n  suite du premier\n- second\n"),
            "<ul>\n<li>premier suite du premier</li>\n<li>second</li>\n</ul>"
        );
    }

    #[test]
    fn renders_a_table_with_a_header_row() {
        assert_eq!(
            render_markdown("| Catégorie | Base |\n|---|---|\n| Compte | Contrat |\n"),
            "<div class=\"table-wrap\"><table>\n<thead><tr><th>Catégorie</th><th>Base</th></tr></thead>\n<tbody>\n<tr><td>Compte</td><td>Contrat</td></tr>\n</tbody>\n</table></div>"
        );
    }

    #[test]
    fn renders_inline_bold_and_code() {
        assert_eq!(
            render_markdown("Voir **le registre** dans `docs/x.md`."),
            "<p>Voir <strong>le registre</strong> dans <code>docs/x.md</code>.</p>"
        );
    }

    #[test]
    fn renders_a_link_with_an_allowed_scheme() {
        assert_eq!(
            render_markdown("[la CNIL](https://cnil.fr)"),
            "<p><a href=\"https://cnil.fr\">la CNIL</a></p>"
        );
        assert_eq!(
            render_markdown("[nous écrire](mailto:dpo@example.test)"),
            "<p><a href=\"mailto:dpo@example.test\">nous écrire</a></p>"
        );
    }

    #[test]
    fn leaves_a_link_with_a_disallowed_scheme_as_literal_text() {
        // Never emits `javascript:`/`data:` hrefs, even though the source
        // document is trusted (it ships in the repo).
        assert_eq!(
            render_markdown("[clic](javascript:alert(1))"),
            "<p>[clic](javascript:alert(1))</p>"
        );
    }

    #[test]
    fn escapes_html_in_the_source() {
        assert_eq!(
            render_markdown("Un <script>alert(1)</script> & une esperluette"),
            "<p>Un &lt;script&gt;alert(1)&lt;/script&gt; &amp; une esperluette</p>"
        );
    }

    #[test]
    fn code_spans_are_not_reinterpreted_as_markup() {
        assert_eq!(
            render_markdown("`**pas gras**`"),
            "<p><code>**pas gras**</code></p>"
        );
    }

    #[test]
    fn renders_the_real_privacy_policy_without_leftover_markup() {
        // Regression guard: the shipped document must stay inside the subset
        // this renderer supports (`apps/api` serves it verbatim over
        // `GET /privacy-policy`, and `apps/web` renders it with this function).
        let md = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/privacy-policy.md"
        ));
        let html = render_markdown(md);
        assert!(html.starts_with("<h1>Politique de confidentialité"));
        assert!(html.contains("<h2>Vos droits</h2>"));
        assert!(html.contains("<table>"));
        assert!(html.contains("<li>"));
        assert!(html.contains("<strong>Droit à l'effacement (Art. 17)</strong>"));
        assert!(html.contains("<code>GET /account/export</code>"));
        // No raw markdown markers survive into the output.
        assert!(!html.contains("**"));
        assert!(!html.contains(" | "));
        assert!(!html.contains("|---"));
        for line in html.lines() {
            assert!(
                !line.starts_with("- ") && !line.starts_with('#'),
                "unrendered markdown line: {line}"
            );
        }
    }
}
