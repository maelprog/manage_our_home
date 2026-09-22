//! Pure, dependency-light logic for the RGPD self-service screens (front epic
//! F10, issue #25), shared by `apps/web`'s SSR pages. Written test-first per
//! `.claude/CLAUDE.md`'s TDD process. Everything those screens need beyond
//! rendering lives here so the UI and the fixed backend
//! (`apps/api/src/rgpd/`, `apps/api/src/auth/mod.rs::delete_account` /
//! `cancel_delete_account`, `apps/api/src/jobs/account_purge.rs`) can never
//! drift.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Europe::Paris;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

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

/// Suffix that marks, inside square brackets, a value the shipped RGPD
/// documents still leave to fill before the service opens publicly — today the
/// controller's name and contact address (#131). Written out in full so it can
/// be grepped from the repository root.
pub const RELEASE_PLACEHOLDER_SUFFIX: &str = "— à renseigner avant la mise en ligne";

/// The label of every `[… — à renseigner avant la mise en ligne]` placeholder
/// the document still carries, in reading order. Line breaks inside a
/// placeholder don't hide it: the source is whitespace-normalized first, since
/// the documents are hard-wrapped.
///
/// The point isn't to forbid placeholders — the controller's identity is
/// deliberately one until the public launch. This function only reports what
/// the document handed to it carries; it pins nothing by itself. The pinning
/// lives in the tests below and covers exactly three documents:
/// `docs/privacy-policy.md`, `docs/registre-traitements.md` and
/// `docs/architecture.md`. A placeholder written anywhere else in the
/// repository — `docs/v2-deployment.md` quotes the form on purpose — turns no
/// test red; only those three are watched, and filling a real value in one of
/// them has to go through the list the tests share.
///
/// Brackets are scanned flat, not nested: in `[a [b — <suffix>]` the label comes
/// out as `a [b`, because the scan pairs the first `[` with the next `]`. No
/// shipped document nests them, and a wrong label still trips the pinning test
/// rather than passing silently.
pub fn release_placeholders(md: &str) -> Vec<String> {
    let flat = flatten(md);
    let mut out = Vec::new();
    let mut rest = flat.as_str();
    // `[` and `]` are ASCII, so every index below lands on a char boundary.
    while let Some(open) = rest.find('[') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find(']') else { break };
        if let Some(label) = rest[..close].strip_suffix(RELEASE_PLACEHOLDER_SUFFIX) {
            out.push(label.trim().to_string());
        }
        rest = &rest[close + 1..];
    }
    out
}

/// Collapses every run of whitespace to one space. The RGPD documents are
/// hard-wrapped at 76 columns, so a sentence — or a placeholder — spans lines in
/// the source and no substring search over the raw text would find it.
fn flatten(md: &str) -> String {
    md.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Top-level directories of this repository (`git ls-tree -d origin/main`, plus
/// the dot-directories it lists): a token starting with one of them is a path,
/// whatever it ends in. Nested directories are deliberately absent — they are
/// reached through their parent (`apps/api/migrations/…` starts with `apps/`).
const REPO_DIRECTORIES: [&str; 7] = [
    "apps/",
    "docs/",
    "e2e/",
    "infra/",
    ".claude/",
    ".githooks/",
    ".github/",
];

/// Extensions that make a token a repository file even without a directory —
/// the half `docs/` alone was missing.
const REPO_FILE_EXTENSIONS: [&str; 9] = [
    ".md", ".rs", ".ts", ".tsx", ".toml", ".sql", ".yml", ".yaml", ".json",
];

/// Markdown punctuation to peel off a token's start; `.` is absent on purpose,
/// so `.github/…` survives.
const TOKEN_LEAD: [char; 8] = ['`', '*', '(', '[', '«', '"', '\'', '_'];

/// Markdown and sentence punctuation to peel off a token's end; `.` is present,
/// so a path closing a sentence still matches its extension.
const TOKEN_TAIL: [char; 13] = [
    '`', '*', ')', ']', '»', '"', '\'', ',', ';', ':', '!', '?', '.',
];

/// Repository paths the document points at, deduplicated, in reading order: a
/// token under a source tree (`apps/`, `docs/`, …) **or** one that merely ends
/// in a source/doc extension. The bare-filename half matters: the guard on
/// `docs/privacy-policy.md` used to look for `docs/` alone, so a cross-reference
/// written `architecture.md` went unnoticed (#131).
///
/// An endpoint (`POST /account/delete`) has neither shape and is left alone.
/// An **absolute URL is not exempt**: `https://example.org/rapport.json` ends in
/// a source extension and is reported. The bias is deliberate — the caller is a
/// test on a document that has no reason to link a `.json`, `.rs` or `.md` at
/// all, and a guard that waved URLs through would wave through a link to this
/// repository's own files on a forge.
pub fn repo_path_references(md: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in md.split_whitespace() {
        let token = raw
            .trim_start_matches(TOKEN_LEAD)
            .trim_end_matches(TOKEN_TAIL);
        let is_path = REPO_DIRECTORIES.iter().any(|d| token.starts_with(d))
            || REPO_FILE_EXTENSIONS.iter().any(|e| token.ends_with(e));
        if is_path && !out.iter().any(|seen| seen == token) {
            out.push(token.to_string());
        }
    }
    out
}

/// How much of a user-chosen name an email body will carry on one line.
/// Wide enough for any real display name or group name, narrow enough that
/// what is left cannot hold a paragraph.
pub const EMAIL_FIELD_MAX_CHARS: usize = 80;

/// Flattens a user-chosen value — a display name, a group name — into one
/// bounded line fit to interpolate into a plain-text email body.
///
/// Both fields reach this module straight from the database:
/// `validate_display_name` only refuses a name that is empty after trimming,
/// `groups.name` is checked no harder, and both columns are unbounded `TEXT`.
/// Interpolated verbatim into an email sent from the service's own `From`
/// address, a newline in either would let the sender close the paragraph and
/// open a section of their own — `-- Ce que vous pouvez faire --`, a
/// controller of their choosing, a contact address they own. That is a
/// forgery the reader has no way to spot, so the value is flattened, stripped
/// and bounded before it is interpolated.
///
/// The characters dropped are the two Unicode categories the reader cannot
/// see: **Cc** (control) and **Cf** (format). Cf was missed at first (#269),
/// because `char::is_control()` answers for Cc alone — so `U+202E`
/// RIGHT-TO-LEFT OVERRIDE and `U+200B` ZERO WIDTH SPACE went through
/// untouched. Neither opens a line, so the guarantee above was never at
/// stake; what they do is make the rendered line disagree with the bytes —
/// an override reverses the rest of the line in the reader's client, a zero
/// width space splits a word or an address in two leaving no mark. A name
/// containing a `U+200D` ZERO WIDTH JOINER loses the joiner, so an emoji
/// sequence held together by one is shown as its separate glyphs: the
/// deliberate price of not keeping an invisible character we cannot vouch
/// for. Nothing else is touched — accents, non-Latin scripts and the
/// variation selectors that pick an emoji's presentation (`U+FE0F` is Mn,
/// not Cf) all survive.
///
/// The order is worth stating because it shows through: whitespace runs
/// (newlines included) collapse to one space and the ends are trimmed
/// **first**, then the invisible characters that are left are dropped, then
/// the result is cut at [`EMAIL_FIELD_MAX_CHARS`] with an ellipsis marking the
/// cut. An invisible character sitting between or before words is therefore
/// dropped after the space around it has been counted: `"a \u{7} b"` comes out
/// as `"a  b"` and `"\u{7} Alice"` as `" Alice"`. That is cosmetic, and it is
/// the only thing the order costs — no line break, and nothing past the
/// bound, survives either way, which is the whole guarantee. The bound is
/// measured *after* the drop, so padding a name with invisible characters
/// neither pushes its visible tail past the cut nor earns it an ellipsis.
pub fn sanitize_email_line(value: &str) -> String {
    let flat: String = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !is_invisible(*c))
        .collect();
    if flat.chars().count() <= EMAIL_FIELD_MAX_CHARS {
        return flat;
    }
    let mut cut: String = flat.chars().take(EMAIL_FIELD_MAX_CHARS).collect();
    cut.push('…');
    cut
}

/// A character that renders as nothing, or as a change to how its neighbours
/// render: Unicode category Cc (which is all `char::is_control()` answers
/// for) or Cf. See [`sanitize_email_line`] for why the second half matters.
fn is_invisible(c: char) -> bool {
    c.is_control() || c.general_category() == GeneralCategory::Format
}

/// Body of the group-invitation email — the one place a person who has no
/// account, and never asked for one, meets this service. Art. 14 RGPD applies
/// there in full (#134): the address was handed over by somebody else, so the
/// email itself has to say who processes it, why, on what legal basis, for how
/// long, where it came from and what the reader can do. A link to a policy
/// sitting behind the sign-up screen would inform nobody.
///
/// The controller's identity and contact address travel as the same two
/// `[… — à renseigner avant la mise en ligne]` placeholders the shipped RGPD
/// documents carry, pinned to the same list by the tests below (#131): the day
/// those documents are filled, this body is filled with them.
///
/// The text is hard-wrapped: it is sent as `text/plain` with no HTML part
/// (`apps/api/src/email.rs`) and nothing re-wraps it in the reader's client.
/// Only a URL alone on its line may run past the margin — breaking a URL
/// breaks the link. The group name sits alone on its own line for that
/// reason; the inviter's name is in the middle of a sentence, so a long one
/// does push that line past the margin. Both go through
/// [`sanitize_email_line`] first, which is what bounds the damage to one
/// over-long line instead of a forged paragraph.
///
/// What the email must not do is promise a door that does not exist. Nothing
/// in this service erases an invitation: `apps/api/src/jobs/account_purge.rs`
/// leaves `invitations` untouched and `export_account` never reads it, so
/// creating an account opens rights over that account's data and not over
/// this row. Arbitrated 2026-09-19 and confirmed 2026-09-20 — the product
/// adds no no-account path and the email tells the truth instead: the address
/// stays until the group is deleted, and any request goes to the controller.
/// Doing nothing is stated first all the same, together with what it costs.
pub fn invitation_email_body(
    group_name: &str,
    inviter_display_name: &str,
    invitation_link: &str,
    privacy_policy_url: &str,
) -> String {
    let group_name = sanitize_email_line(group_name);
    let inviter_display_name = sanitize_email_line(inviter_display_name);
    format!(
        "Bonjour,

Vous êtes invité(e) à rejoindre, sur Manage Our Home, le groupe
« {group_name} »

C'est {inviter_display_name}, membre du groupe, qui a saisi votre adresse email
pour vous inviter : nous ne la tenons pas de vous, et vous n'avez aucun
compte sur ce service.

Pour rejoindre le groupe :
{invitation_link}

Ce lien est valable 7 jours et ne sert qu'une fois.

-- Pourquoi vous recevez cet email --

Manage Our Home est une application d'organisation familiale. Votre
adresse y est traitée dans le seul but de vous transmettre cette
invitation et de rattacher votre compte au groupe si vous l'acceptez. La
base légale est l'intérêt légitime du membre qui invite un proche.

Votre adresse est enregistrée avec cette invitation, puis effacée :
dès que le lien est utilisé, ou au plus tard 30 jours après cet envoi.

Le responsable de traitement est [nom du responsable de traitement — à
renseigner avant la mise en ligne], joignable à [adresse de contact — à
renseigner avant la mise en ligne]. Vous pouvez aussi introduire une
réclamation auprès de la CNIL.

-- Ce que vous pouvez faire --

Si vous ne voulez pas de cette invitation, ignorez cet email : le lien
cesse de fonctionner au bout de 7 jours. Votre adresse, elle, reste
enregistrée avec l'invitation jusqu'à 30 jours après cet envoi, puis est
effacée.

Aucun écran de ce service ne permet d'agir sur cette adresse. Créer un
compte depuis le lien ci-dessus ouvre des droits sur les données de ce
compte, pas sur cette invitation. Pour accéder à votre adresse, la faire
rectifier ou effacer, ou vous opposer à son traitement,
adressez la demande au responsable de traitement, à l'adresse ci-dessus.

Politique de confidentialité :
{privacy_policy_url}
"
    )
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

    // -- release_placeholders ------------------------------------------------

    #[test]
    fn release_placeholders_lists_each_value_left_to_fill() {
        let md = "Exploité par [nom du responsable de traitement — à renseigner\n\
                  avant la mise en ligne], joignable à\n\
                  [adresse de contact — à renseigner avant la mise en ligne].";
        assert_eq!(
            release_placeholders(md),
            vec![
                "nom du responsable de traitement".to_string(),
                "adresse de contact".to_string(),
            ]
        );
    }

    #[test]
    fn release_placeholders_ignores_brackets_that_are_not_placeholders() {
        assert!(release_placeholders(
            "[la CNIL](https://www.cnil.fr) et [une note entre crochets]"
        )
        .is_empty());
    }

    // -- repo_path_references ------------------------------------------------

    #[test]
    fn repo_path_references_catches_a_bare_filename_not_only_a_directory() {
        // The `docs/`-only check let a cross-reference written `architecture.md`
        // through (#131).
        assert_eq!(
            repo_path_references("Voir `architecture.md` et docs/registre-traitements.md."),
            vec![
                "architecture.md".to_string(),
                "docs/registre-traitements.md".to_string(),
            ]
        );
    }

    #[test]
    fn repo_path_references_catches_source_trees_and_repeats_nothing() {
        assert_eq!(
            repo_path_references(
                "apps/api/src/lib.rs, infra/docker-compose.yml, apps/api/src/lib.rs"
            ),
            vec![
                "apps/api/src/lib.rs".to_string(),
                "infra/docker-compose.yml".to_string(),
            ]
        );
    }

    #[test]
    fn repo_path_references_leaves_endpoints_and_plain_urls_alone() {
        assert!(repo_path_references(
            "Demandez la suppression via `POST /account/delete` (Art. 17), ou écrivez à la \
             [CNIL](https://www.cnil.fr/fr/adresser-une-plainte)."
        )
        .is_empty());
    }

    #[test]
    fn repo_path_references_does_not_exempt_a_url_ending_in_a_source_extension() {
        // Not an oversight: the caller is a guard on a document that has no
        // business linking a `.json`/`.rs`/`.md` anywhere, including on a forge.
        assert_eq!(
            repo_path_references("Voir https://example.org/rapport.json"),
            vec!["https://example.org/rapport.json".to_string()]
        );
    }

    // -- the shipped documents -----------------------------------------------

    /// The values the RGPD documents still leave to fill, in reading order.
    /// Single source of truth for the two tests below: the day the last of them
    /// is filled in for the public launch, this list empties, and every
    /// assertion built on it has to be looked at.
    ///
    /// Two of them are the controller's identity (#131), filled by
    /// `docs/v2-deployment.md` #16. The last three are the subprocessor
    /// questions #136 left open on purpose: what contract actually binds the
    /// email subprocessor, and which transfer mechanism — if any — covers it
    /// and Google. Neither the policy nor the registre asserts an answer, and
    /// `docs/v2-deployment.md` #18 is what closes them. They sit here because
    /// the documents have to keep saying the same thing: a value filled in one
    /// and forgotten in the other turns this suite red.
    fn pending_release_values() -> Vec<String> {
        let mut values = pending_controller_values();
        values.extend([
            "cadre contractuel du sous-traitant email".to_string(),
            "transferts hors UE du sous-traitant email".to_string(),
            "transferts hors UE de Google".to_string(),
        ]);
        values
    }

    /// The controller's identity alone, in reading order: the two values the
    /// invitation email carries (art. 14(1)(a)) and the ones
    /// `docs/v2-deployment.md` #16 fills. The email says nothing about
    /// subprocessors, so it must not be pinned to the three values #18 owns.
    fn pending_controller_values() -> Vec<String> {
        vec![
            "nom du responsable de traitement".to_string(),
            "adresse de contact".to_string(),
        ]
    }

    /// `docs/registre-traitements.md` carries the same placeholders as the
    /// policy, and `docs/architecture.md` announces that they exist without
    /// carrying any of its own. The three are pinned as **one** state, so
    /// filling one document and leaving another stale can't happen silently
    /// (#131) — `docs/v2-deployment.md` #16 sends the future author here.
    #[test]
    fn the_internal_rgpd_documents_carry_the_same_placeholders() {
        let policy = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/privacy-policy.md"
        ));
        let registre = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/registre-traitements.md"
        ));
        let architecture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/architecture.md"
        ));
        let pending = pending_release_values();
        assert_eq!(release_placeholders(policy), pending);
        assert_eq!(release_placeholders(registre), pending);
        // architecture.md points at the placeholders, it holds none: a third one
        // added there would otherwise never be filled.
        assert_eq!(release_placeholders(architecture), Vec::<String>::new());
        // …and it must stop announcing them the day the two documents above are
        // filled, which is the day `pending` empties.
        assert_eq!(
            flatten(architecture).contains(RELEASE_PLACEHOLDER_SUFFIX),
            !pending.is_empty(),
            "architecture.md and the two RGPD documents disagree on whether the \
             controller's identity is still to be filled"
        );
        for (name, md) in [
            ("the policy", policy),
            ("the registre", registre),
            ("architecture.md", architecture),
        ] {
            assert!(
                !md.contains("placeholder_name"),
                "{name} still names the controller `placeholder_name`"
            );
        }
    }

    /// The values `docs/legal-notice.md` still leaves to fill (#132). A list of
    /// its own, not `pending_release_values`: LCEN art. 6-III asks the legal
    /// notice for things the privacy policy never had to carry — a postal
    /// address, a publication director, a host — and the host is not chosen yet
    /// (self-hosting, a VPS later; arbitrated 2026-09-19). The contact address
    /// is deliberately worded the same in both documents: it is the same
    /// address, and it is filled once.
    fn pending_legal_notice_values() -> Vec<String> {
        vec![
            "nom de l'éditeur".to_string(),
            "adresse postale de l'éditeur".to_string(),
            "adresse de contact".to_string(),
            "nom du directeur de la publication".to_string(),
            "nom et adresse de l'hébergeur".to_string(),
        ]
    }

    #[test]
    fn the_public_legal_documents_carry_only_the_placeholders_pinned_here() {
        let notice = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/legal-notice.md"
        ));
        let terms = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/terms-of-service.md"
        ));
        let policy = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/privacy-policy.md"
        ));
        assert_eq!(release_placeholders(notice), pending_legal_notice_values());
        // The CGU name nobody: they send the reader to the legal notice for the
        // publisher's identity, so there is exactly one place to fill.
        assert_eq!(release_placeholders(terms), Vec::<String>::new());
        // Publisher and controller are the same physical person, so the two
        // documents empty on the same day; filling one and forgetting the other
        // is the failure this pins.
        assert_eq!(
            release_placeholders(notice).is_empty(),
            release_placeholders(policy).is_empty(),
            "the legal notice and the privacy policy disagree on whether the \
             publisher/controller is still to be filled"
        );
    }

    #[test]
    fn renders_the_real_legal_notice_without_leftover_markup() {
        let md = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/legal-notice.md"
        ));
        let html = render_markdown(md);
        assert!(html.starts_with("<h1>Mentions légales"));
        // The three identities LCEN art. 6-III makes mandatory.
        assert!(html.contains("<h2>Éditeur du service</h2>"));
        assert!(html.contains("<h2>Directeur de la publication</h2>"));
        assert!(html.contains("<h2>Hébergeur</h2>"));
        // Same rule as the policy: the reader of a public page cannot open a
        // repository path, so the document has to stand on its own.
        assert_eq!(
            repo_path_references(md),
            Vec::<String>::new(),
            "the legal notice points at repo files"
        );
        // Every placeholder survives the renderer as readable text rather than
        // being swallowed as a link label.
        for pending in pending_legal_notice_values() {
            assert!(
                html.contains(&format!("[{pending}")),
                "`{pending}` is not readable in the rendered legal notice"
            );
        }
        assert_no_raw_markdown(&html);
    }

    #[test]
    fn renders_the_real_terms_without_leftover_markup() {
        let md = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/terms-of-service.md"
        ));
        let html = render_markdown(md);
        assert!(html.starts_with("<h1>Conditions générales d'utilisation"));
        assert!(html.contains("<h2>Vos contenus</h2>"));
        assert!(html.contains("<h2>Fermeture de votre compte</h2>"));
        // #132: the CGU are the contractual vehicle for the arbitrage that a
        // member's content outlives the deletion of their account. Until now
        // only the privacy policy carried it, and a policy is not a contract.
        assert!(
            html.contains("reste dans le groupe"),
            "the CGU do not state that content outlives the account"
        );
        // #137: the CGU are also where the art. 8 GDPR age condition is
        // contractual. The threshold is read from the constant the API
        // enforces, so raising one and forgetting the other turns this red
        // instead of leaving the contract promising a rule nothing applies.
        assert!(
            html.contains("<h2>Âge minimal</h2>"),
            "the CGU have no minimum-age clause"
        );
        assert!(
            html.contains(&format!(
                "au moins <strong>{} ans</strong>",
                crate::validation::auth::MINIMUM_AGE_YEARS
            )),
            "the CGU state a minimum age the registration does not enforce"
        );
        assert_eq!(
            repo_path_references(md),
            Vec::<String>::new(),
            "the CGU point at repo files"
        );
        assert_no_raw_markdown(&html);
    }

    /// No markdown marker survived into the output: a shipped document has to
    /// stay inside the subset `render_markdown` supports, or the page shows it
    /// raw.
    fn assert_no_raw_markdown(html: &str) {
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
        // Art. 13 mentions (#133): the six rights, not four, and the right to
        // lodge a complaint with the CNIL as a working link.
        assert!(html.contains("<strong>Droit d'opposition (Art. 21)</strong>"));
        assert!(html.contains("<strong>Droit à la limitation (Art. 18)</strong>"));
        assert!(html.contains("<a href=\"https://www.cnil.fr/fr/adresser-une-plainte\">"));
        // #137, art. 8 GDPR: the policy says where the service stands on
        // minors — the audit's constat n° 8 was that no document did.
        assert!(
            html.contains("<h2>Le service et les mineurs</h2>"),
            "the policy says nothing about minors"
        );
        // The reader of `/privacy-policy` cannot open a repository path: the
        // document must stand on its own. Checked over every path shape, not
        // just `docs/` — a bare `architecture.md` is as unreachable (#131).
        assert_eq!(
            repo_path_references(md),
            Vec::<String>::new(),
            "the policy points at repo files"
        );
        // #131: the controller's identity and contact address are deliberately
        // still placeholders, and exactly these two. Filling them in for the
        // public launch has to come through `pending_release_values`.
        assert_eq!(release_placeholders(md), pending_release_values());
        assert!(!md.contains("placeholder_name"));
        // Both placeholders survive the renderer as readable text rather than
        // being swallowed as a link label.
        assert!(html.contains("[nom du responsable de traitement"));
        assert!(html.contains("[adresse de contact"));
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

    // -- invitation_email_body (#134) ----------------------------------------

    const INVITE_LINK: &str =
        "https://maison.example.org/groups/invitations/6f1a2b3c-0000-4000-8000-000000000001/accept";
    const POLICY_URL: &str = "https://maison.example.org/privacy-policy";

    fn invitation_sample() -> String {
        invitation_email_body("Famille Dupont", "Alice Martin", INVITE_LINK, POLICY_URL)
    }

    #[test]
    fn invitation_email_names_the_group_the_inviter_and_carries_both_links() {
        let body = invitation_sample();
        assert!(body.contains("Famille Dupont"), "{body}");
        assert!(body.contains("Alice Martin"), "{body}");
        assert!(body.contains(INVITE_LINK), "{body}");
        assert!(body.contains(POLICY_URL), "{body}");
    }

    #[test]
    fn invitation_email_says_the_address_came_from_the_member_not_from_the_reader() {
        // Art. 14(2)(f): the source of the data. This reader never handed us
        // their address, and the email is the only place they meet the service.
        let body = invitation_sample();
        assert!(body.contains("Alice Martin, membre du groupe"), "{body}");
        assert!(body.contains("a saisi votre adresse email"), "{body}");
        assert!(body.contains("nous ne la tenons pas de vous"), "{body}");
    }

    #[test]
    fn invitation_email_states_the_purpose_the_legal_basis_and_the_retention() {
        // Art. 14(1)(c)(d) and 14(2)(a). The retention is the one the privacy
        // policy and the processing register state: a 7-day single-use link,
        // the row deleted when the link is used, and otherwise 30 days after
        // it was sent (#138).
        let body = invitation_sample();
        assert!(body.contains("intérêt légitime"), "{body}");
        assert!(body.contains("valable 7 jours"), "{body}");
        assert!(body.contains("ne sert qu'une fois"), "{body}");
        assert!(body.contains("dès que le lien est utilisé"), "{body}");
        assert!(body.contains("30 jours après cet envoi"), "{body}");
        assert!(!body.contains("suppression du groupe"), "{body}");
    }

    #[test]
    fn invitation_email_carries_the_controller_identity_and_contact() {
        // Art. 14(1)(a): the controller's identity *and* contact details —
        // 14(1)(b) is the data protection officer, which this service has no
        // reason to appoint. Both are still `[… — à renseigner avant la mise
        // en ligne]`, and they are exactly the two the RGPD documents carry
        // for the controller: the day those are filled, this body is filled
        // with them (#131). The three subprocessor placeholders the documents
        // also carry (#136) have no place in an invitation email.
        let body = invitation_sample();
        assert_eq!(release_placeholders(&body), pending_controller_values());
    }

    #[test]
    fn invitation_email_carries_the_right_to_lodge_a_complaint() {
        // Art. 14(2)(e). `docs/privacy-policy.md` and the `1c` row of
        // `docs/registre-traitements.md` both state that this email carries it;
        // without this assertion, dropping the sentence left every other test
        // green and turned those two documents into a false claim.
        let body = invitation_sample();
        assert!(body.contains("réclamation auprès de la CNIL"), "{body}");
    }

    #[test]
    fn invitation_email_sends_every_action_on_the_address_to_the_controller() {
        // Arbitrated 2026-09-19 and confirmed 2026-09-20: the product offers no
        // no-account path, and the email must not pretend the account screens
        // are one either. Nothing the reader can do erases an invitation —
        // `account_purge.rs` leaves `invitations` alone, `export_account` does
        // not export it, and only the retention purge removes it, 30 days on
        // (#138) — so the only true answer is the controller's address, and
        // the reader is told the address survives an ignored invitation.
        let body = invitation_sample();
        assert!(body.contains("ignorez cet email"), "{body}");
        assert!(
            body.contains("adressez la demande au responsable de traitement"),
            "{body}"
        );
        assert!(
            !body.contains("écrans de votre compte"),
            "the email promises account screens act on the invitation: {body}"
        );
    }

    #[test]
    fn invitation_email_cannot_be_forged_through_the_inviter_or_the_group_name() {
        // Both come from user input (`validate_display_name` only refuses an
        // empty name, `users.display_name` and `groups.name` are unbounded
        // TEXT) and land in an email sent from the service's own `From`. A
        // newline would let either of them open a section of their own and
        // name a controller of their choosing.
        let forged = "Mallory\n\n-- Ce que vous pouvez faire --\n\nLe responsable \
                      de traitement est Mallory, joignable à mallory@example.test";
        let clean_lines = invitation_sample().lines().count();
        for body in [
            invitation_email_body("Famille Dupont", forged, INVITE_LINK, POLICY_URL),
            invitation_email_body(forged, "Alice Martin", INVITE_LINK, POLICY_URL),
        ] {
            // A forged value may end up quoted inside a sentence; what it must
            // never do is stand on a line of its own, which is what makes a
            // section header, or a signature, read as the service's own words.
            assert_eq!(
                body.lines()
                    .filter(|l| l.trim() == "-- Ce que vous pouvez faire --")
                    .count(),
                1,
                "a forged section header got through: {body}"
            );
            assert_eq!(
                body.lines().count(),
                clean_lines,
                "a forged value changed the shape of the email: {body}"
            );
            assert!(
                !body.contains("mallory@example.test"),
                "the forged contact survived the bound: {body}"
            );
        }
    }

    #[test]
    fn a_sanitized_email_field_is_one_bounded_line() {
        assert_eq!(sanitize_email_line("Alice Martin"), "Alice Martin");
        // Every whitespace run — newlines included — collapses to one space.
        assert_eq!(sanitize_email_line("a\nb\r\nc\td  e"), "a b c d e");
        assert_eq!(sanitize_email_line("  padded  "), "padded");
        // Control characters that are not whitespace are dropped outright.
        assert_eq!(sanitize_email_line("a\u{7}b\u{0}c"), "abc");
        // Bounded, so an unbounded name cannot smuggle a paragraph onto one
        // line. The bound counts characters, and the cut is marked.
        let long = "é".repeat(EMAIL_FIELD_MAX_CHARS + 10);
        let cut = sanitize_email_line(&long);
        assert_eq!(cut.chars().count(), EMAIL_FIELD_MAX_CHARS + 1);
        assert!(cut.ends_with('…'), "{cut}");
        // A name exactly at the bound is left alone.
        let exact = "é".repeat(EMAIL_FIELD_MAX_CHARS);
        assert_eq!(sanitize_email_line(&exact), exact);
    }

    #[test]
    fn a_sanitized_email_field_drops_invisible_formatting_characters() {
        // `char::is_control()` only covers the Cc category, so the whole Cf
        // category used to travel intact into the body (#269). None of these
        // opens a line, so the one-line guarantee held either way — what they
        // do is lie about what the reader sees: a RIGHT-TO-LEFT OVERRIDE
        // reverses the rest of the line in the reader's client, and a ZERO
        // WIDTH SPACE cuts a word (an address, a name) in two without leaving
        // a mark. Both were reproduced on the shipped sanitizer.
        assert_eq!(sanitize_email_line("Alice\u{202e}Martin"), "AliceMartin");
        assert_eq!(sanitize_email_line("ali\u{200b}ce"), "alice");
        // The rest of the category, each one invisible and each one reachable
        // through a display name or a group name.
        for invisible in [
            '\u{00ad}',  // SOFT HYPHEN
            '\u{061c}',  // ARABIC LETTER MARK
            '\u{200b}',  // ZERO WIDTH SPACE
            '\u{200c}',  // ZERO WIDTH NON-JOINER
            '\u{200d}',  // ZERO WIDTH JOINER
            '\u{200e}',  // LEFT-TO-RIGHT MARK
            '\u{200f}',  // RIGHT-TO-LEFT MARK
            '\u{202a}',  // LEFT-TO-RIGHT EMBEDDING
            '\u{202b}',  // RIGHT-TO-LEFT EMBEDDING
            '\u{202c}',  // POP DIRECTIONAL FORMATTING
            '\u{202d}',  // LEFT-TO-RIGHT OVERRIDE
            '\u{202e}',  // RIGHT-TO-LEFT OVERRIDE
            '\u{2060}',  // WORD JOINER
            '\u{2066}',  // LEFT-TO-RIGHT ISOLATE
            '\u{2067}',  // RIGHT-TO-LEFT ISOLATE
            '\u{2068}',  // FIRST STRONG ISOLATE
            '\u{2069}',  // POP DIRECTIONAL ISOLATE
            '\u{feff}',  // ZERO WIDTH NO-BREAK SPACE (BOM)
            '\u{e0041}', // TAG LATIN CAPITAL LETTER A
        ] {
            assert_eq!(
                sanitize_email_line(&format!("ab{invisible}cd")),
                "abcd",
                "U+{:04X} survived the sanitizer",
                invisible as u32
            );
        }
    }

    #[test]
    fn a_sanitized_email_field_keeps_the_names_people_actually_have() {
        // The filter drops two Unicode categories, not "anything unusual": a
        // name written with accents, in a non-Latin script, or carrying an
        // emoji with its variation selector (U+FE0F is Mn, not Cf) must come
        // out untouched, or the sanitizer turns into a script test.
        for name in [
            "Zoé Lefèvre-Ngô",
            "Đặng Thị Hồng",
            "Ελένη Παπαδοπούλου",
            "Алексей Иванов",
            "田中 陽子",
            "نور الهدى",
            "Maison ❤\u{fe0f}",
        ] {
            assert_eq!(sanitize_email_line(name), name);
        }
    }

    #[test]
    fn invisible_characters_are_dropped_before_the_bound_is_measured() {
        // The bound is measured on what is left, so padding a name with
        // invisible characters cannot push its visible tail past the cut —
        // nor make a name that fits look over-long and gain an ellipsis.
        let padded: String = "é"
            .repeat(EMAIL_FIELD_MAX_CHARS)
            .chars()
            .flat_map(|c| [c, '\u{200b}'])
            .collect();
        assert_eq!(
            sanitize_email_line(&padded),
            "é".repeat(EMAIL_FIELD_MAX_CHARS)
        );
    }

    #[test]
    fn invitation_email_carries_no_invisible_formatting_from_its_two_fields() {
        // End of the road for #269: whatever a member types as their display
        // name or their group name, the body that leaves the service's own
        // `From` holds no character the reader cannot see.
        let body = invitation_email_body(
            "Famille\u{202e}Dupont",
            "Al\u{200b}ice\u{200d}Martin",
            INVITE_LINK,
            POLICY_URL,
        );
        assert!(
            !body
                .chars()
                .any(|c| c == '\u{202e}' || c == '\u{200b}' || c == '\u{200d}'),
            "an invisible formatting character reached the email body: {body:?}"
        );
        assert!(body.contains("FamilleDupont"), "{body}");
        assert!(body.contains("AliceMartin"), "{body}");
    }

    #[test]
    fn invitation_email_points_at_no_repository_path() {
        // Same rule as the policy: the reader of an email cannot open a file
        // of this repository.
        assert_eq!(
            repo_path_references(&invitation_sample()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn invitation_email_is_wrapped_for_a_plain_text_reader() {
        // Sent as `text/plain`: nothing re-wraps it. A URL may run past the
        // margin — breaking one breaks the link — but only alone on its line:
        // exempting every line that merely *contains* a URL would wave through
        // a 300-character line of prose with a link in the middle.
        for line in invitation_sample().lines() {
            let is_lone_url = !line.contains(char::is_whitespace) && line.contains("https://");
            assert!(
                line.chars().count() <= 78 || is_lone_url,
                "line too long for a plain-text email: {line}"
            );
        }
    }

    #[test]
    fn invitation_email_interpolates_a_group_name_verbatim() {
        let body =
            invitation_email_body("Coloc' « Rue des Lilas »", "Bob", INVITE_LINK, POLICY_URL);
        assert!(body.contains("Coloc' « Rue des Lilas »"), "{body}");
    }
}
