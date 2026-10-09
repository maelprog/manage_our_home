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
/// `` `code` `` (opened by any run of backticks, closed by a run of the same
/// length), `**strong**` and `[text](url)` markers.
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
                .map(|c| format!("<th scope=\"col\">{}</th>", inline(c)))
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
                // As in CommonMark: a run of n backticks opens a span that
                // closes on the next run of exactly n, and a run with no such
                // closer is literal text, whole.
                let run = backtick_run(&chars, i);
                match closing_backticks(&chars, i + run, run) {
                    Some(end) => {
                        let content: String = chars[i + run..end].iter().collect();
                        // One leading and one trailing space are stripped
                        // when both are there and the span is not all spaces.
                        let content = if content.len() >= 2
                            && content.starts_with(' ')
                            && content.ends_with(' ')
                            && !content.chars().all(|c| c == ' ')
                        {
                            &content[1..content.len() - 1]
                        } else {
                            &content[..]
                        };
                        out.push_str("<code>");
                        out.push_str(content);
                        out.push_str("</code>");
                        i = end + run;
                    }
                    None => {
                        out.extend(&chars[i..i + run]);
                        i += run;
                    }
                }
                continue;
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

/// The length of the run of backticks that starts at `at`.
fn backtick_run(chars: &[char], at: usize) -> usize {
    chars[at..].iter().take_while(|&&c| c == '`').count()
}

/// The start of the first run of exactly `len` backticks at or after `from`.
fn closing_backticks(chars: &[char], from: usize, len: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '`' {
            let run = backtick_run(chars, i);
            if run == len {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
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
/// lives in the tests below and covers eight documents, each pinned to its
/// exact list: the internal RGPD documents (`docs/privacy-policy.md`,
/// `docs/registre-traitements.md`, `docs/architecture.md`), the public legal
/// documents (`docs/legal-notice.md`, `docs/terms-of-service.md`), and the
/// breach and AIPD documents of #143 (`docs/procedure-violation.md`,
/// `docs/registre-violations.md`, `docs/aipd.md`). A placeholder written
/// anywhere else in the repository — `docs/v2-deployment.md` quotes the form
/// on purpose — turns no test red; only those eight are watched, and filling a
/// real value in one of them has to go through the list its test pins.
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
/// the reader can reach erases an invitation: `apps/api/src/jobs/account_purge.rs`
/// deletes the invitations an account *sent*, never one addressed to it
/// (the row is keyed on its sender, not on the invited address), and
/// `export_account` never reads it, so creating an account opens rights over
/// that account's data and not over this row. Arbitrated 2026-09-19 and confirmed 2026-09-20 — the product
/// adds no no-account path and the email tells the truth instead: any request
/// goes to the controller. Doing nothing is stated first all the same,
/// together with what it costs.
///
/// What does erase the row is the retention purge
/// (`apps/api/src/jobs/retention_purge.rs`, #138): at acceptance, and
/// otherwise 30 days after the invitation was created. That pass runs once an
/// hour **and only while the API is up with a role that bypasses RLS**: it
/// runs on `ADMIN_DATABASE_URL`, falling back to `DATABASE_URL`, and refuses
/// a role without `BYPASSRLS` — which the service survives, staying up while
/// nothing is erased (#276, #284). So the 30th day is when the row becomes
/// purgeable and no bound can be promised at all: an outage, or such a
/// configuration, defers the deletion for as long as it lasts. The notice
/// therefore states the frequency and what either of them does to it, and
/// states no deadline — not "au plus tard 30 jours", which the hourly pass
/// overruns on every invitation, and not a flat hour either, which an outage
/// overruns just as surely. `docs/privacy-policy.md` and
/// `docs/registre-traitements.md` carry that same reserve. Deleting the group
/// takes the row earlier, which breaks no promise: nothing here promises the
/// address stays.
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
dès que le lien est utilisé, sinon 30 jours après cet envoi. Cet
effacement est fait par un passage automatique qui a lieu
toutes les heures quand le service fonctionne et que sa
configuration le permet ; si le service est interrompu, ou si sa
configuration suspend ce passage, il a lieu à son redémarrage ou au
premier passage horaire qui suit le rétablissement de cette
configuration.

Le responsable de traitement est [nom du responsable de traitement — à
renseigner avant la mise en ligne], joignable à [adresse de contact — à
renseigner avant la mise en ligne]. Vous pouvez aussi introduire une
réclamation auprès de la CNIL.

-- Ce que vous pouvez faire --

Si vous ne voulez pas de cette invitation, ignorez cet email : le lien
cesse de fonctionner au bout de 7 jours. Votre adresse, elle, reste
enregistrée avec l'invitation pendant 30 jours après cet envoi, puis est
effacée par ce passage automatique.

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

/// Body of the email warning the holder of an account the superadmin
/// deactivated that the account will be purged (#256): sent once, 30 days
/// before the 2 years of deactivation are up
/// (`apps/api/src/jobs/account_purge.rs`). `purge_at` is the day it becomes
/// purgeable; like every date the policy states, it is when the hourly purge
/// may take the account, not a guaranteed hour.
///
/// The right credentials on a deactivated account open the page that carries
/// the reactivation request form (#289), so the email sends the holder there;
/// the controller, named by the same two placeholders as the invitation
/// email, stays the contact for the other rights.
pub fn deactivation_notice_email_body(
    deactivated_at: DateTime<Utc>,
    purge_at: DateTime<Utc>,
    privacy_policy_url: &str,
) -> String {
    let deactivated_on = format_rgpd_date(deactivated_at);
    let purge_on = format_rgpd_date(purge_at);
    format!(
        "Bonjour,

Votre compte Manage Our Home a été désactivé par l'administrateur du
service le {deactivated_on}. Il est conservé tel quel depuis ; une
connexion avec vos identifiants n'ouvre qu'une page, d'où vous pouvez
demander sa réactivation.

Un compte qui reste désactivé 2 ans est supprimé. Sauf réactivation
d'ici là, le vôtre le sera à partir du {purge_on}, au premier passage de
la purge automatique qui suit cette date (elle passe toutes les heures
quand le service fonctionne et que sa configuration le permet). Une
demande de réactivation suspend cette suppression tant qu'elle est en
attente, sauf si une demande a déjà été refusée depuis la désactivation.

-- Ce que la suppression efface --

Votre adresse email et votre nom sont remplacés, votre mot de passe
et votre connexion avec Google sont effacés, et vous êtes retiré(e) de
vos groupes. Les contenus partagés avec vos groupes (événements,
messages, recettes, listes...) restent, rattachés à un compte anonyme,
comme le prévoient les conditions générales.

-- Ce que vous pouvez faire --

Pour demander sa réactivation, connectez-vous au service avec vos
identifiants habituels : la page qui s'ouvre porte le formulaire de
demande. Pour exercer vos droits sur vos données, écrivez au
responsable de traitement, [nom du responsable de traitement — à
renseigner avant la mise en ligne], à [adresse de contact — à
renseigner avant la mise en ligne]. Vous pouvez aussi introduire une
réclamation auprès de la CNIL.

Si vous ne faites rien, le compte sera supprimé comme indiqué
ci-dessus, et cet email est le seul que vous recevrez à ce sujet.

Politique de confidentialité :
{privacy_policy_url}
"
    )
}

/// Body of the email telling a member they became the owner of a group
/// without asking for it (#323): the previous owner's account was purged,
/// the group had no owner left and a member's account was reactivated, or
/// the superadmin designated them. Sent to that member whatever their
/// reminder channel (#306) — it is not a reminder — once, by the hourly pass
/// of `apps/api/src/jobs/account_purge.rs`; the home page shows the same
/// news until they acknowledge it.
///
/// The group name sits alone on its line and goes through
/// [`sanitize_email_line`], like the invitation's, for the same reason: it
/// is chosen by a member and must not be able to open a paragraph of its
/// own.
pub fn ownership_inherited_email_body(
    group_name: &str,
    reason: crate::validation::groups::OwnershipReason,
    groups_url: &str,
    privacy_policy_url: &str,
) -> String {
    use crate::validation::groups::OwnershipReason;
    let group_name = sanitize_email_line(group_name);
    let why = match reason {
        OwnershipReason::AccountPurged => {
            "Le compte de son ancien propriétaire a été supprimé. La propriété
vous revient : parmi les membres restants, vous étiez le premier dans
l'ordre de succession que prévoient les conditions d'utilisation."
        }
        OwnershipReason::MemberReactivated => {
            "Le groupe n'avait plus de propriétaire. À la réactivation d'un compte
de ses membres, la propriété vous est revenue : vous êtes le premier
dans l'ordre de succession que prévoient les conditions d'utilisation."
        }
        OwnershipReason::DesignatedBySupport => {
            "Le groupe n'avait plus de propriétaire : l'administrateur du service
vous a désigné(e) parmi ses membres actifs."
        }
    };
    format!(
        "Bonjour,

Vous êtes désormais propriétaire, sur Manage Our Home, du groupe
« {group_name} »

{why}

En tant que propriétaire, vous pouvez inviter des membres, nommer des
administrateurs, transférer la propriété du groupe à un autre membre ou
supprimer le groupe. Tant que vous en êtes propriétaire, la
suppression de votre compte n'est possible qu'après avoir transféré la
propriété ou supprimé le groupe.

Pour gérer vos groupes :
{groups_url}

Cet email vous est envoyé une fois, quelles que soient vos préférences
de rappel : la propriété d'un groupe vous a été confiée sans que vous
l'ayez demandée.

Politique de confidentialité :
{privacy_policy_url}
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use unicode_normalization::UnicodeNormalization;

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
            "<div class=\"table-wrap\"><table>\n<thead><tr><th scope=\"col\">Catégorie</th><th scope=\"col\">Base</th></tr></thead>\n<tbody>\n<tr><td>Compte</td><td>Contrat</td></tr>\n</tbody>\n</table></div>"
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

    /// #376: a code span opened by a run of backticks closes on a run of the
    /// same length, as in CommonMark — ` ``x`` ` used to come out as two
    /// empty `<code>` around a plain `x`.
    #[test]
    fn a_code_span_closes_on_a_backtick_run_of_its_own_length() {
        assert_eq!(render_markdown("``x``"), "<p><code>x</code></p>");
        assert_eq!(
            render_markdown("Voir `` a`b `` ici"),
            "<p>Voir <code>a`b</code> ici</p>"
        );
        assert_eq!(
            render_markdown("`a``b` et ```c```"),
            "<p><code>a``b</code> et <code>c</code></p>"
        );
    }

    #[test]
    fn a_backtick_run_left_open_stays_literal() {
        // No closing run of the same length: the backticks are text, which
        // the raw-markdown guard then reports.
        assert_eq!(render_markdown("``x`"), "<p>``x`</p>");
        assert_eq!(render_markdown("`x``"), "<p>`x``</p>");
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
    /// `docs/v2-deployment.md` #16. The other three are the subprocessor
    /// questions still open: what contract actually binds the email
    /// subprocessor, and which transfer mechanism covers Google and the
    /// browsers' push services. Neither the policy nor the registre asserts an
    /// answer, and `docs/v2-deployment.md` #18 is what closes them. The email
    /// relay's own transfer question is no longer one of them: Scaleway TEM
    /// (#328) declares no processing outside the EU, and both documents say so
    /// with a dated source instead of a placeholder. They sit here because
    /// the documents have to keep saying the same thing: a value filled in one
    /// and forgotten in the other turns this suite red.
    fn pending_release_values() -> Vec<String> {
        let mut values = pending_controller_values();
        values.extend([
            "cadre contractuel du sous-traitant email".to_string(),
            "transferts hors UE de Google".to_string(),
            "transferts hors UE des services de notification".to_string(),
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

    /// The values `docs/legal-notice.md` still leaves to fill (#132, #314). A
    /// list of its own, not `pending_release_values`: the legal notice owes the
    /// public the host's name, address and phone number (LCEN art. 1-1, I 4°),
    /// which the privacy policy never had to carry, and the host is not chosen
    /// yet (self-hosting, a VPS later; arbitrated 2026-09-19).
    ///
    /// The publisher's own name, address and phone number are deliberately
    /// absent: the publisher edits on a non-professional basis and keeps the
    /// anonymity LCEN art. 1-1, II allows (arbitrated 2026-10-04), so those go to
    /// the host, never into this public repository — `docs/v2-deployment.md`
    /// #17 carries that step. The contact address is worded the same as in the
    /// policy: it is the same address, and it is filled once.
    fn pending_legal_notice_values() -> Vec<String> {
        vec![
            "adresse de contact".to_string(),
            "nom et adresse de l'hébergeur".to_string(),
            "numéro de téléphone de l'hébergeur".to_string(),
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

    /// The breach procedure, the breach register template and the AIPD (#143)
    /// each leave a value to fill before the public launch. None of them is
    /// served, so nothing else would notice one forgotten: `docs/v2-deployment.md`
    /// #16 and #19 send the future author here. The contact address is the
    /// policy's — the same address, filled once — so the procedure must empty
    /// on the same day as the policy.
    #[test]
    fn the_breach_and_aipd_documents_carry_only_the_placeholders_pinned_here() {
        let procedure = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/procedure-violation.md"
        ));
        let register = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/registre-violations.md"
        ));
        let aipd = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/aipd.md"));
        let policy = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/privacy-policy.md"
        ));
        assert_eq!(
            release_placeholders(procedure),
            vec!["adresse de contact".to_string()]
        );
        assert_eq!(
            release_placeholders(register),
            vec!["emplacement du registre des violations".to_string()]
        );
        assert_eq!(
            release_placeholders(aipd),
            vec!["conclusion de l'AIPD, date et signature".to_string()]
        );
        assert_eq!(
            release_placeholders(procedure).is_empty(),
            !release_placeholders(policy).contains(&"adresse de contact".to_string()),
            "the breach procedure and the privacy policy disagree on whether the \
             contact address is still to be filled"
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
        // The three roles LCEN art. 1-1 names: publisher, publication director,
        // host.
        assert!(html.contains("<h2>Éditeur du service</h2>"));
        assert!(html.contains("<h2>Directeur de la publication</h2>"));
        assert!(html.contains("<h2>Hébergeur</h2>"));
        // #314: the regime actually applied is the non-professional anonymity
        // of art. 1-1, II, not the full disclosure of the I. The notice has to
        // say so, and has to tell a reader where a right-of-reply request goes
        // when the director is not named: to the host (art. 1-1, III).
        let flat = flatten(md);
        assert!(
            flat.contains("article 1-1") && flat.contains("anonymat"),
            "the legal notice does not state the LCEN anonymity regime it applies"
        );
        assert!(
            flat.contains("droit de réponse"),
            "the legal notice does not say where a right-of-reply request goes"
        );
        // Every article the notice cites is LCEN art. 1-1 or one of the two code
        // pénal articles on the host's professional secrecy; see
        // `cited_article_numbers` for what counts as a citation.
        assert_eq!(
            foreign_articles(md),
            Vec::<String>::new(),
            "the legal notice cites an article other than LCEN art. 1-1 and code \
             pénal art. 226-13/226-14: the LCEN obligations live in its art. 1-1 \
             since loi n° 2024-449"
        );
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
        assert_no_raw_markdown(md, &html);
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
        // #319: the version an account accepts is the constant the API
        // records, and the one the document says is in force. Bumping one
        // without the other would record acceptances of a text nobody shows,
        // or show a new text no member is told about.
        let version_lines: Vec<&str> = md
            .lines()
            .filter(|line| line.starts_with("Version en vigueur :"))
            .collect();
        assert_eq!(
            version_lines,
            vec![format!(
                "Version en vigueur : {}.",
                crate::validation::auth::TERMS_VERSION
            )],
            "the CGU state a version the acceptance does not record"
        );
        assert_eq!(
            repo_path_references(md),
            Vec::<String>::new(),
            "the CGU point at repo files"
        );
        assert_no_raw_markdown(md, &html);
    }

    /// #367: a version announced is held to what the text in force is held
    /// to — one `Version en vigueur` line, stating the date it applies from,
    /// no placeholder, no repo path, no leftover markup — and that date
    /// comes after the version in force, or announcing it would announce
    /// nothing. With nothing announced there is nothing to pin.
    #[test]
    fn an_announced_version_states_its_own_date_and_renders_cleanly() {
        let Some(announced) = crate::validation::auth::TERMS_ANNOUNCED else {
            return;
        };
        let md = announced.markdown;
        let version_lines: Vec<&str> = md
            .lines()
            .filter(|line| line.starts_with("Version en vigueur :"))
            .collect();
        assert_eq!(
            version_lines,
            vec![format!("Version en vigueur : {}.", announced.version)],
            "the announced CGU state a version other than their date"
        );
        // Written `YYYY-MM-DD` in full, so that its order as text — the
        // api's — is its order as a date; then, compared as text, later.
        assert!(
            crate::validation::auth::is_terms_version(announced.version),
            "the announced version `{}` is not a full YYYY-MM-DD date",
            announced.version
        );
        assert!(
            announced.version > crate::validation::auth::TERMS_VERSION,
            "the announced version applies before the one in force"
        );
        let html = render_markdown(md);
        assert!(html.starts_with("<h1>Conditions générales d'utilisation"));
        assert_eq!(release_placeholders(md), Vec::<String>::new());
        assert_eq!(repo_path_references(md), Vec::<String>::new());
        assert_no_raw_markdown(md, &html);
    }

    /// Every age `md` states as the service's minimum, in reading order: the
    /// number in `N ans ou plus`, `N ans et plus`, `moins de N ans` or
    /// `au moins N ans` — the four phrasings the RGPD documents use for the
    /// art. 8 GDPR threshold (#137). Markdown emphasis, link text
    /// (`[16 ans ou plus](/x)`, #346) and its target even with a title
    /// (`[16 ans](/x "t") ou plus`), elisions (`d'au moins`) and hard wraps
    /// are seen through.
    ///
    /// A duration is not an age: `après 2 ans`, `au plus tôt 2 ans et
    /// 30 jours` match none of the phrasings. Blind spot, accepted because no
    /// document writes it today and a new one would be written by hand under
    /// review: a threshold phrased otherwise (`15 ans minimum`, `dès 15 ans`).
    fn stated_minimum_ages(md: &str) -> Vec<u32> {
        // A link's target, title and blanks included (`](/x "t")`), is not
        // part of the text: cut out up to its own closing `)` (#376). An
        // unclosed target is left in place.
        let mut text = String::new();
        let mut rest = md;
        while let Some(at) = rest.find("](") {
            let Some(end) = link_target_end(&rest[at + 2..]) else {
                break;
            };
            text.push_str(&rest[..=at]);
            text.push(' ');
            rest = &rest[at + 2 + end + 1..];
        }
        text.push_str(rest);
        let words: Vec<String> = text
            .split_whitespace()
            .map(|w| {
                // `plus][ref]` reads as `plus`: a reference glued to the last
                // word of a link's text is not part of the word. Cut before
                // the elision, which the reference may contain too.
                let w = w.split(']').next().unwrap_or(w);
                // `d'au` reads as `au`: an elision is not part of the word.
                let w = w.rsplit(['\'', '’']).next().unwrap_or(w);
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .collect();
        let at = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
        let mut out = Vec::new();
        for (i, word) in words.iter().enumerate() {
            let Ok(age) = word.parse::<u32>() else {
                continue;
            };
            if at(i + 1) != "ans" {
                continue;
            }
            let or_more = matches!(at(i + 2), "ou" | "et") && at(i + 3) == "plus";
            let before = |back: usize| i.checked_sub(back).map(at).unwrap_or("");
            let under = before(2) == "moins" && before(1) == "de";
            let at_least = before(2) == "au" && before(1) == "moins";
            if or_more || under || at_least {
                out.push(age);
            }
        }
        out
    }

    /// The byte index of the `)` closing a link target that starts at
    /// `target` (just past `](`), as CommonMark reads it: a `<…>` target is
    /// taken whole, parentheses nest, a `"…"` or `'…'` title opened after a
    /// blank is taken whole, and a backslash escapes the next character.
    fn link_target_end(target: &str) -> Option<usize> {
        let mut chars = target.char_indices();
        if target.starts_with('<') {
            chars.find(|&(_, c)| c == '>')?;
        }
        let (mut depth, mut title, mut after_blank) = (0usize, None, false);
        while let Some((i, c)) = chars.next() {
            match (title, c) {
                (_, '\\') => {
                    chars.next();
                }
                (Some(quote), _) if c == quote => title = None,
                (Some(_), _) => {}
                (None, '"' | '\'') if after_blank => title = Some(c),
                (None, '(') => depth += 1,
                (None, ')') if depth == 0 => return Some(i),
                (None, ')') => depth -= 1,
                _ => {}
            }
            after_blank = c.is_whitespace();
        }
        None
    }

    #[test]
    fn stated_minimum_ages_catch_each_phrasing_of_the_threshold() {
        assert_eq!(
            stated_minimum_ages("déclarer avoir 15 ans ou plus"),
            vec![15]
        );
        assert_eq!(
            stated_minimum_ages("(réservent le service aux 15 ans et plus)"),
            vec![15]
        );
        assert_eq!(
            stated_minimum_ages("pas ouvert aux moins de 16 ans. C'est"),
            vec![16]
        );
        assert_eq!(
            stated_minimum_ages("avoir au moins 15 ans et sa date"),
            vec![15]
        );
    }

    #[test]
    fn stated_minimum_ages_see_through_emphasis_and_wraps() {
        assert_eq!(
            stated_minimum_ages("âgées d'au moins **15 ans**. C'est"),
            vec![15]
        );
        assert_eq!(stated_minimum_ages("avoir 15\nans ou\nplus"), vec![15]);
    }

    /// #346: a threshold written as a link's text is still stated — the
    /// target glued to the last word (`plus](/x)`) is not part of it.
    #[test]
    fn stated_minimum_ages_see_through_links() {
        assert_eq!(stated_minimum_ages("[16 ans ou plus](/x)"), vec![16]);
        assert_eq!(
            stated_minimum_ages("être [d'au moins 15 ans](https://example.org/a'b)"),
            vec![15]
        );
        assert_eq!(stated_minimum_ages("aux [moins de 16 ans][ref]"), vec![16]);
    }

    /// #376: a link's whole target is cut out, title and blanks included —
    /// `[16 ans](/x "t") ou plus` used to read `"t")` between `ans` and `ou`.
    #[test]
    fn stated_minimum_ages_see_through_a_link_target_with_blanks() {
        assert_eq!(
            stated_minimum_ages("avoir [16 ans](/x \"titre\") ou plus"),
            vec![16]
        );
        assert_eq!(
            stated_minimum_ages("avoir [16 ans](</a b>) ou plus"),
            vec![16]
        );
        assert_eq!(
            stated_minimum_ages("aux [moins de](/x 'un titre') 16 ans"),
            vec![16]
        );
    }

    /// A target with balanced parentheses, or a title holding one, ends at
    /// its own closing `)`, not at the first one.
    #[test]
    fn stated_minimum_ages_see_through_a_target_with_parentheses() {
        assert_eq!(
            stated_minimum_ages("avoir [16 ans](https://x.org/a_(b)) ou plus"),
            vec![16]
        );
        assert_eq!(
            stated_minimum_ages("avoir [16 ans](/x \"a (b)\") ou plus"),
            vec![16]
        );
        assert_eq!(
            stated_minimum_ages("avoir [16 ans](/x 'a ) b') ou plus"),
            vec![16]
        );
        assert_eq!(
            stated_minimum_ages("avoir [16 ans](</a)b>) ou plus"),
            vec![16]
        );
    }

    #[test]
    fn stated_minimum_ages_keep_every_occurrence_in_order() {
        assert_eq!(
            stated_minimum_ages("moins de 15 ans ; avoir 16 ans ou plus"),
            vec![15, 16]
        );
    }

    #[test]
    fn stated_minimum_ages_ignore_durations() {
        assert!(stated_minimum_ages("purgé après 2 ans de désactivation").is_empty());
        assert!(stated_minimum_ages("au plus tôt 2 ans et 30 jours après").is_empty());
        assert!(stated_minimum_ages("un compte qui reste désactivé 2 ans").is_empty());
    }

    /// #317: the privacy policy and the processing register state the age
    /// threshold too, and until now only the CGU were pinned to the constant
    /// the registration enforces. Every threshold either document states is
    /// that constant, so raising it and forgetting one of them — or editing
    /// one sentence of a document and not the others — turns this red.
    #[test]
    fn the_policy_and_the_registre_state_the_minimum_age_the_registration_enforces() {
        let minimum = crate::validation::auth::MINIMUM_AGE_YEARS;
        for (name, md) in [
            (
                "the policy",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../docs/privacy-policy.md"
                )),
            ),
            (
                "the registre",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../docs/registre-traitements.md"
                )),
            ),
        ] {
            let ages = stated_minimum_ages(md);
            assert!(!ages.is_empty(), "{name} states no minimum age");
            assert!(
                ages.iter().all(|&age| age == minimum),
                "{name} states the minimum ages {ages:?}, the registration enforces {minimum}"
            );
        }
    }

    /// The only articles `docs/legal-notice.md` may cite (#314): LCEN art. 1-1,
    /// and the code pénal articles 226-13 and 226-14 on the host's
    /// professional secrecy.
    const LEGAL_NOTICE_ARTICLES: [&str; 3] = ["1-1", "226-13", "226-14"];

    /// The numbers cited in `md` that are not in `LEGAL_NOTICE_ARTICLES`.
    fn foreign_articles(md: &str) -> Vec<String> {
        cited_article_numbers(md)
            .into_iter()
            .filter(|n| !LEGAL_NOTICE_ARTICLES.contains(&n.as_str()))
            .collect()
    }

    /// Every article number cited in `md`, in reading order, compared exactly
    /// by the caller (`1-10` is not `1-1`).
    ///
    /// A citation is the word `article`, `articles` or `art.` (any case,
    /// after an elided `l'`/`d'`, wrapped in any punctuation such as `(`,
    /// `**` or `«`), followed by a word that starts with a digit — glued
    /// (`art.6`) or not, after a number sign (`n°`, `nº`, `no`, `no.`, `№`,
    /// `n°s`, `nºs`, `nos`, any case, glued or not) or not. An enumeration is followed: after the first
    /// number, each further number separated by `,`, `et`, `ou` or `à` (any
    /// case) is cited too (`articles 1-1 et 6-III` cites both), unless
    /// followed by `°`, `º` or the letter `o` typed for them (`article 6,
    /// 4° du I`: `4°` is an item of a
    /// subdivision). Leading and trailing punctuation is stripped from each
    /// number (`6-IV)` is `6-IV`; `6, III` is `6`), and a hyphen U+2010 or
    /// a non-breaking one U+2011 reads as `-`.
    ///
    /// Blind spots, accepted because the legal notice carries none of these
    /// forms today and a new one would be written by hand under review: a
    /// number glued to `article` without a dot (`article6`), a number that
    /// does not start with a digit (`article L. 34-5`, `article premier`),
    /// a subdivision cited without the word article (`au III du 6`), and an
    /// enumeration continued after a subdivision (`article 1-1, II et 6`
    /// stops at `II`).
    fn cited_article_numbers(md: &str) -> Vec<String> {
        fn bare(word: &str) -> &str {
            word.trim_matches(|c: char| !c.is_alphanumeric())
        }
        /// The number sign before a number, any case — `n°`, `nº`, `no`,
        /// `no.`, `№`, and the plurals `n°s`, `nºs`, `nos` — and what
        /// follows it in `word`. Longest spelling first, so `nos` is not
        /// read as `no` glued to `s`.
        fn number_sign(word: &str) -> Option<&str> {
            let lower = word.to_lowercase();
            ["n°s", "nºs", "nos", "no.", "n°", "nº", "no", "№"]
                .iter()
                .find(|sign| lower.starts_with(*sign))
                .and_then(|sign| word.get(sign.len()..))
        }
        /// The number `words[at]` cites, through a number sign glued to it
        /// (`n°6`) or standing before it (`n° 6`), with the index of the
        /// word that carries it. `subdivision`: a number followed by `°` or
        /// `º` (`4°`, an item of a subdivision) is not an article.
        fn number_at(words: &[&str], at: usize, subdivision: bool) -> Option<(String, usize)> {
            let word = *words.get(at)?;
            let (word, at) = match number_sign(word) {
                Some("") => (*words.get(at + 1)?, at + 1),
                Some(glued) => (glued, at),
                None => (word, at),
            };
            let number = bare(word);
            // Digits then `º`, or a letter `o` typed for it, stay in `bare`;
            // `°` is not a letter and follows it.
            let ordinal = matches!(
                number.trim_start_matches(|c: char| c.is_ascii_digit()),
                "º" | "o" | "O"
            ) || word[word.find(number)? + number.len()..].starts_with('°');
            (number.starts_with(|c: char| c.is_ascii_digit()) && !(subdivision && ordinal))
                .then(|| (number.replace(['\u{2010}', '\u{2011}'], "-"), at))
        }
        let words: Vec<&str> = md.split_whitespace().collect();
        let mut out = Vec::new();
        for (i, raw) in words.iter().enumerate() {
            let word = raw.trim_start_matches(|c: char| !c.is_alphanumeric());
            let word = word.rsplit(['\'', '’']).next().unwrap_or(word);
            // The first number, and the index of the word that carries it.
            let first = if word.len() > 4
                && word.is_char_boundary(4)
                && word[..4].eq_ignore_ascii_case("art.")
            {
                let mut glued = words.clone();
                glued[i] = &word[4..];
                number_at(&glued, i, false)
            } else if matches!(
                bare(word).to_lowercase().as_str(),
                "article" | "articles" | "art"
            ) {
                number_at(&words, i + 1, false)
            } else {
                None
            };
            let Some((number, mut at)) = first else {
                continue;
            };
            out.push(number);
            loop {
                let next = if words[at].ends_with(',') {
                    number_at(&words, at + 1, true)
                } else if words
                    .get(at + 1)
                    .is_some_and(|w| matches!(bare(w).to_lowercase().as_str(), "et" | "ou" | "à"))
                {
                    number_at(&words, at + 2, true)
                } else {
                    None
                };
                let Some((number, next_at)) = next else {
                    break;
                };
                out.push(number);
                at = next_at;
            }
        }
        out
    }

    #[test]
    fn cited_articles_catch_a_number_inside_parentheses() {
        assert_eq!(foreign_articles("(article 6-IV)"), vec!["6-IV".to_string()]);
    }

    #[test]
    fn cited_articles_follow_an_enumeration() {
        assert_eq!(
            foreign_articles("(articles 6 et 7 de la loi)"),
            vec!["6".to_string(), "7".to_string()]
        );
        assert_eq!(
            foreign_articles("articles 6, 7 ou 8"),
            vec!["6".to_string(), "7".to_string(), "8".to_string()]
        );
    }

    #[test]
    fn cited_articles_see_through_markdown_emphasis() {
        assert_eq!(
            foreign_articles("**article 6** de la loi"),
            vec!["6".to_string()]
        );
    }

    #[test]
    fn cited_articles_catch_a_number_glued_to_art() {
        assert_eq!(
            foreign_articles("voir art.6-III"),
            vec!["6-III".to_string()]
        );
    }

    #[test]
    fn cited_articles_check_the_second_number_of_an_enumeration() {
        assert_eq!(
            foreign_articles("articles 1-1 et 6-III"),
            vec!["6-III".to_string()]
        );
    }

    #[test]
    fn cited_articles_compare_the_exact_number() {
        assert_eq!(foreign_articles("article 1-10"), vec!["1-10".to_string()]);
        assert_eq!(foreign_articles("article 6, III"), vec!["6".to_string()]);
    }

    #[test]
    fn cited_articles_accept_what_the_notice_may_cite() {
        assert_eq!(
            cited_article_numbers(
                "L'article 1-1, II de la loi ; Art. 1-1. Les articles 226-13 et \
                 226-14 du code pénal."
            ),
            vec!["1-1", "1-1", "226-13", "226-14"]
        );
        assert!(foreign_articles("Les articles 226-13 et 226-14 s'appliquent.").is_empty());
    }

    #[test]
    fn cited_articles_read_the_conjunctions_in_any_case() {
        assert_eq!(
            foreign_articles("ARTICLES 6 ET 7"),
            vec!["6".to_string(), "7".to_string()]
        );
        assert_eq!(
            foreign_articles("Articles 6 Ou 7"),
            vec!["6".to_string(), "7".to_string()]
        );
        assert_eq!(
            foreign_articles("articles 6 À 9"),
            vec!["6".to_string(), "9".to_string()]
        );
    }

    #[test]
    fn cited_articles_see_through_the_number_sign() {
        for md in [
            "article n° 6",
            "article nº 6",
            "article N° 6",
            "article no 6",
            "article n°6",
        ] {
            assert_eq!(foreign_articles(md), vec!["6".to_string()], "{md:?}");
        }
        assert_eq!(
            foreign_articles("articles n° 6 et n° 7"),
            vec!["6".to_string(), "7".to_string()]
        );
    }

    #[test]
    fn cited_articles_see_through_every_spelling_of_the_number_sign() {
        for md in [
            "article № 6",
            "article №6",
            "article No. 6",
            "article no. 6",
        ] {
            assert_eq!(foreign_articles(md), vec!["6".to_string()], "{md:?}");
        }
        for md in [
            "articles nos 6 et 7",
            "articles n°s 6 et 7",
            "articles Nos 6 et 7",
        ] {
            assert_eq!(
                foreign_articles(md),
                vec!["6".to_string(), "7".to_string()],
                "{md:?}"
            );
        }
    }

    #[test]
    fn cited_articles_strip_the_punctuation_before_the_word() {
        assert_eq!(foreign_articles("(art.6)"), vec!["6".to_string()]);
        assert_eq!(
            foreign_articles("«\u{a0}art.6\u{a0}»"),
            vec!["6".to_string()]
        );
        assert_eq!(foreign_articles("(article 6)"), vec!["6".to_string()]);
    }

    #[test]
    fn cited_articles_do_not_read_a_subdivision_as_an_article() {
        // `4°` numbers an item of a subdivision, not an article.
        assert_eq!(
            foreign_articles("article 6, 4° du I"),
            vec!["6".to_string()]
        );
        assert_eq!(
            foreign_articles("article 6, 4º du I"),
            vec!["6".to_string()]
        );
        assert!(foreign_articles("l'article 1-1, 2° du II").is_empty());
    }

    #[test]
    fn cited_articles_read_an_o_after_a_number_as_the_ordinal_sign() {
        // `4o` is how `4°` is often typed.
        assert_eq!(
            foreign_articles("article 6, 4o du I"),
            vec!["6".to_string()]
        );
        assert_eq!(
            foreign_articles("article 6 et 2O du II"),
            vec!["6".to_string()]
        );
    }

    #[test]
    fn cited_articles_read_a_non_breaking_hyphen_as_a_hyphen() {
        assert_eq!(
            cited_article_numbers("article 1\u{2011}1 et article 226\u{2010}13"),
            vec!["1-1", "226-13"]
        );
        assert!(foreign_articles("articles 226\u{2011}13 et 226\u{2011}14").is_empty());
    }

    #[test]
    fn cited_articles_ignore_a_word_that_is_not_a_number() {
        // The stocks feature, in the notice's own intellectual-property section.
        assert!(cited_article_numbers("recettes, articles de stock, dépenses").is_empty());
    }

    /// The markdown markers of `md` that `render_markdown` left raw in `html`,
    /// one entry per finding. When in doubt it reports: a false positive costs
    /// a reword, a false negative ships raw syntax to the reader.
    ///
    /// Block markers are read on the source lines: the renderer joins a
    /// paragraph's lines with a space, so a numbered list, a quote or a rule
    /// written under a line of text is no longer at the start of anything in
    /// the output. The line-start markers looked for there are exactly:
    ///
    /// - a heading of level 4 or deeper, a bare `#`, a heading whose `#` is
    ///   followed by a tab, or a heading closed by a `#` sequence
    ///   (`## Titre ##`, a tab before it too);
    /// - `>`;
    /// - a number followed by `.` or `)`, then a blank or the line end —
    ///   except on a line that continues a paragraph, where only a `1` is
    ///   refused, the one number CommonMark lets interrupt a paragraph
    ///   (`2024. Son I` wrapped under text is text);
    /// - a line made only of `-` or of `=` (a setext underline, an empty
    ///   bullet), and a line of three or more `*` or `_` (a rule);
    /// - a `+` or `*` bullet, and a `-` followed by a tab (or any whitespace
    ///   other than a space): `-item` or `-->` stay text, as in CommonMark;
    /// - a code fence, ```` ``` ```` or `~~~`;
    /// - a line indented by four columns or more (a tab counts four) that does
    ///   not continue a paragraph or a bullet: indented code;
    /// - a link reference definition, `[label]:` or `[^note]:`;
    /// - inside a `- ` bullet, any of the above, a heading of any level
    ///   (`- # Titre`), a nested `- ` bullet, and a task box `[ ]`/`[x]`;
    /// - an indented `- ` bullet (the renderer drops the nesting);
    /// - under a `- ` bullet, a block CommonMark keeps in the item but the
    ///   renderer moves out of the list: a heading or a table indented under
    ///   it, a table or a line of text flush against it (a lazy
    ///   continuation), a rule, a quote, a fence, raw HTML, another bullet
    ///   or a numbered list from any number, empty or not, flush against it
    ///   (named as that block: `interrupting_block`), any
    ///   indented line after one or more blank lines (a second paragraph of
    ///   the item), and a `- ` bullet after one or more blank lines (GFM
    ///   makes one loose list, the renderer two lists);
    /// - two trailing spaces (a hard line break the renderer joins away).
    ///
    /// Inline markers are read on the text the reader sees — tags dropped,
    /// `<code>` content dropped since it is shown verbatim on purpose: `**`,
    /// any `*`, a `_` opening or closing a word (`snake_case` is left alone),
    /// any backtick (a code span left open), any `~` (strikethrough), `](`,
    /// `][` and `![` (a link the renderer refused, a reference link, an
    /// image), a `!` glued before a rendered link (an image whose URL was
    /// accepted), a rendered link whose `href` holds a `(`, a blank or a
    /// quote (a URL cut at its first `)`, or carrying a title), an escaped
    /// `<` (raw HTML, an autolink), an escaped entity
    /// reference (`&copy;`, `&#169;`), any `\` (a backslash escape or a hard
    /// break), and the pipe syntax of a table that did not render. Outside
    /// links and code spans: a bare `http://`, `https://` or `www.` URL in
    /// any case, and an `@` after a word character, `.`, `-`, `_` or `+` and
    /// before a word character (an address GFM would link).
    fn raw_markdown_markers(md: &str, html: &str) -> Vec<String> {
        let mut found = Vec::new();
        let mut prev = Context::Blank;
        for line in md.lines() {
            let t = line.trim();
            if t.is_empty() {
                prev = match prev {
                    Context::List | Context::BlankAfterList => Context::BlankAfterList,
                    _ => Context::Blank,
                };
                continue;
            }
            if line.ends_with("  ") {
                found.push(format!("hard line break: {t}"));
            }
            let indented = line.starts_with(char::is_whitespace);
            if indented && (t.starts_with("- ") || t.starts_with("-\t")) {
                found.push(format!("nested bullet: {t}"));
                prev = Context::List;
                continue;
            }
            if let Some(block) = block_under_bullet(t, indented, prev) {
                found.push(format!("{block} under a bullet: {t}"));
                prev = Context::Other;
                continue;
            }
            let continues = matches!(prev, Context::Paragraph | Context::List);
            if indent_columns(line) >= 4 && !continues {
                found.push(format!("indented code: {t}"));
                prev = Context::Other;
                continue;
            }
            if t.starts_with('|') {
                prev = Context::Other;
                continue;
            }
            if let Some(marker) = line_start_marker(t, prev == Context::Paragraph) {
                found.push(format!("{marker}: {t}"));
                prev = Context::Other;
                continue;
            }
            prev = if t.starts_with("- ") {
                Context::List
            } else if heading(t).is_some() {
                Context::Other
            } else if prev == Context::List && line.starts_with(char::is_whitespace) {
                Context::List
            } else {
                Context::Paragraph
            };
        }
        // An image whose URL the renderer accepted: its `!` is left before
        // the link, so it never reaches the visible text as `![`.
        for line in html.lines().filter(|l| l.contains("!<a href=")) {
            found.push(format!("image: {line}"));
        }
        // A link the renderer accepted but cut: it ends the URL at the first
        // `)` and keeps a title, so a `(`, a blank or a quote in the `href`
        // means the reader follows something other than what was written.
        for href in html.split("<a href=\"").skip(1) {
            let href = href.split('"').next().unwrap_or_default();
            if href.contains(|c: char| c == '(' || c == '\'' || c.is_whitespace())
                || href.contains("&quot;")
            {
                found.push(format!("link url: {href}"));
            }
        }
        for text in text_outside_links(html) {
            if has_bare_link(&text) {
                found.push(format!("bare url or address: {text}"));
            }
        }
        for text in visible_text(html) {
            let chars: Vec<char> = text.chars().collect();
            let word = |i: usize| chars.get(i).is_some_and(|c| c.is_alphanumeric());
            let stray_underscore = (0..chars.len())
                .any(|i| chars[i] == '_' && (i == 0 || !word(i - 1) || !word(i + 1)));
            if text.contains('*') || stray_underscore {
                found.push(format!("emphasis: {text}"));
            }
            if text.contains(" | ") || text.contains("|---") {
                found.push(format!("table: {text}"));
            }
            if text.contains('`') {
                found.push(format!("code span: {text}"));
            }
            if text.contains('~') {
                found.push(format!("strikethrough: {text}"));
            }
            if text.contains("](") || text.contains("][") || text.contains("![") {
                found.push(format!("link or image: {text}"));
            }
            if text.contains("&lt;") {
                found.push(format!("raw html: {text}"));
            }
            if has_escaped_entity(&text) {
                found.push(format!("entity: {text}"));
            }
            if text.contains('\\') {
                found.push(format!("backslash: {text}"));
            }
        }
        found
    }

    /// What the previous source line left open, as far as the guard needs:
    /// a paragraph, a `- ` bullet (with its indented continuations), nothing
    /// (a blank line, the start), or another block.
    #[derive(Clone, Copy, PartialEq)]
    enum Context {
        Blank,
        /// One or more blank lines after a bullet: CommonMark still lets an
        /// indented line continue the item there, the renderer does not.
        BlankAfterList,
        Paragraph,
        List,
        Other,
    }

    /// The block `t` would put inside the bullet `prev` left open, where the
    /// renderer closes the list and emits it after: a heading or a table
    /// indented under the bullet, a table or a line of text flush against it
    /// (GFM reads it as the item's lazy continuation, or as the block it
    /// opens: `interrupting_block`), any indented line after a blank line (a
    /// second paragraph of the item), and a `- ` bullet after a blank line
    /// (an item of the same, loose, list). A line glued to
    /// the bullet and indented is the continuation the renderer absorbs; a
    /// heading or a `- ` bullet flush against it closes the item in both.
    fn block_under_bullet(t: &str, indented: bool, prev: Context) -> Option<&'static str> {
        match prev {
            Context::BlankAfterList if indented => Some("block after a blank line"),
            Context::BlankAfterList if t.starts_with("- ") => Some("loose list item"),
            Context::List if t.starts_with('|') => Some("table"),
            Context::List => {
                let after_hashes = t.trim_start_matches('#');
                let is_heading = after_hashes.len() < t.len()
                    && (after_hashes.is_empty() || after_hashes.starts_with(char::is_whitespace));
                if indented {
                    is_heading.then_some("heading")
                } else if is_heading || t.starts_with("- ") {
                    None
                } else {
                    Some(interrupting_block(t).unwrap_or("lazy continuation text"))
                }
            }
            _ => None,
        }
    }

    /// The block `t`, flush left under a bullet's text, opens in CommonMark.
    /// Flush left, the line is outside the item: any block start closes the
    /// item — a rule, a quote, a code fence, raw HTML, a `+`/`*`/`-` bullet
    /// or a numbered list from any number, empty or not (cmark-gfm gives
    /// `- a\n2. x` an `<ol start="2">`). The "starts at 1, with content"
    /// rule only binds a list interrupting a paragraph of its own container.
    /// Anything else (`===`, `--`, `[a]: /b`) is lazy continuation text — a
    /// setext underline cannot be lazy, so `---` is a rule there.
    fn interrupting_block(t: &str) -> Option<&'static str> {
        let mut chars = t.chars();
        let (first, second) = (chars.next(), chars.next());
        if is_thematic_break(t) {
            Some("rule")
        } else if t.starts_with('>') {
            Some("blockquote")
        } else if t.starts_with("```") || t.starts_with("~~~") {
            Some("code fence")
        } else if first == Some('<')
            && second.is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'))
        {
            Some("raw html")
        } else if t == "-" {
            // `- ` trimmed: an empty item of the same list.
            Some("empty bullet")
        } else if matches!(first, Some('+' | '*' | '-')) && second.is_none_or(char::is_whitespace) {
            Some("bullet")
        } else if is_ordered_item(t) {
            Some("numbered list")
        } else {
            None
        }
    }

    /// The marker `t` (a trimmed, non-empty, non-table line) opens, if any.
    /// `in_paragraph`: the line would continue a paragraph.
    fn line_start_marker(t: &str, in_paragraph: bool) -> Option<&'static str> {
        let hashes = t.chars().take_while(|&c| c == '#').count();
        let after_hashes = &t[hashes..];
        if hashes > 0 && (after_hashes.is_empty() || after_hashes.starts_with(char::is_whitespace))
        {
            match heading(t) {
                None => return Some("heading"),
                Some((_, text)) => {
                    let open = text.trim_end_matches('#');
                    if open.len() < text.len()
                        && (open.is_empty() || open.ends_with(char::is_whitespace))
                    {
                        return Some("heading closing sequence");
                    }
                }
            }
        }
        if t.starts_with('>') {
            return Some("blockquote");
        }
        if is_ordered_item(t) && (!in_paragraph || ordered_start(t) == Some(1)) {
            return Some("numbered list");
        }
        let marks: Vec<char> = t.chars().filter(|c| !c.is_whitespace()).collect();
        if marks.iter().all(|&c| c == '=') || marks.iter().all(|&c| c == '-') {
            return Some("setext underline or empty bullet");
        }
        if is_thematic_break(t) {
            return Some("rule");
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            return Some("code fence");
        }
        if t.starts_with('[') && t.contains("]:") {
            return Some("reference definition");
        }
        let mut rest = t.chars();
        let first = rest.next();
        let second = rest.next();
        if matches!(first, Some('+' | '*')) && second.is_none_or(char::is_whitespace) {
            return Some("bullet");
        }
        if first == Some('-') && second.is_some_and(|c| c != ' ' && c.is_whitespace()) {
            return Some("bullet");
        }
        let item = t.strip_prefix("- ")?.trim_start();
        if item.starts_with("- ") {
            return Some("nested bullet");
        }
        if ["[ ]", "[x]", "[X]"].iter().any(|b| item.starts_with(b)) {
            return Some("task box");
        }
        // Any heading, even one `heading` accepts on its own line: inside a
        // bullet the renderer shows its `#` as text.
        let after_hashes = item.trim_start_matches('#');
        if after_hashes.len() < item.len()
            && (after_hashes.is_empty() || after_hashes.starts_with(char::is_whitespace))
        {
            return Some("heading in a bullet");
        }
        line_start_marker(item, false)
    }

    /// The width of `line`'s leading whitespace, a tab counting up to the
    /// next multiple of four as CommonMark does.
    fn indent_columns(line: &str) -> usize {
        let mut columns = 0;
        for c in line.chars() {
            match c {
                ' ' => columns += 1,
                '\t' => columns += 4 - columns % 4,
                _ => break,
            }
        }
        columns
    }

    /// `1. `, `1) ` (a tab as good as the space), or the number alone on its
    /// line: an ordered-list item.
    fn is_ordered_item(t: &str) -> bool {
        let digits = t.chars().take_while(char::is_ascii_digit).count();
        let rest = &t[digits..];
        digits > 0
            && (rest.starts_with('.') || rest.starts_with(')'))
            && (rest.len() == 1 || rest[1..].starts_with(char::is_whitespace))
    }

    /// The number an ordered item starts with (`01.` starts at 1).
    fn ordered_start(t: &str) -> Option<u64> {
        let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    }

    /// An entity reference of the source, escaped by the renderer: `&amp;`
    /// followed by a name or `#` number and `;`. A plain `&` is left alone.
    fn has_escaped_entity(text: &str) -> bool {
        text.match_indices("&amp;").any(|(at, m)| {
            let tail = &text[at + m.len()..];
            let name = tail
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '#')
                .count();
            name > 0 && tail[name..].starts_with(';')
        })
    }

    /// Three or more of the same `-`, `*` or `_`, spaces allowed between.
    fn is_thematic_break(t: &str) -> bool {
        let marks: Vec<char> = t.chars().filter(|c| !c.is_whitespace()).collect();
        marks.len() >= 3
            && ['-', '*', '_'].contains(&marks[0])
            && marks.iter().all(|&c| c == marks[0])
    }

    /// Each output line's text as the reader sees it: tags dropped, and the
    /// content of `<code>` dropped with them.
    fn visible_text(html: &str) -> Vec<String> {
        html.lines()
            .map(|line| {
                let mut text = String::new();
                let mut rest = line;
                while let Some(open) = rest.find('<') {
                    text.push_str(&rest[..open]);
                    let tail = &rest[open..];
                    rest = if let Some(code) = tail.strip_prefix("<code>") {
                        code.find("</code>").map_or("", |end| &code[end + 7..])
                    } else {
                        tail.find('>').map_or("", |end| &tail[end + 1..])
                    };
                }
                text.push_str(rest);
                text
            })
            .collect()
    }

    /// Each output line's text outside links and code spans: where a bare
    /// URL or address would be text here and a link under GFM.
    fn text_outside_links(html: &str) -> Vec<String> {
        html.lines()
            .map(|line| {
                let mut text = String::new();
                let mut rest = line;
                while let Some(open) = rest.find('<') {
                    text.push_str(&rest[..open]);
                    let tail = &rest[open..];
                    let close = if tail.starts_with("<a ") {
                        "</a>"
                    } else if tail.starts_with("<code>") {
                        "</code>"
                    } else {
                        ">"
                    };
                    rest = tail
                        .find(close)
                        .map_or("", |end| &tail[end + close.len()..]);
                }
                text.push_str(rest);
                text
            })
            .collect()
    }

    /// What GFM would turn into a link: `http://`, `https://`, `www.`, in any
    /// case, or an `@` between a character of an address's local part (a
    /// word character, `.`, `-`, `_` or `+`) and a word character (an e-mail
    /// address).
    fn has_bare_link(text: &str) -> bool {
        let lower = text.to_lowercase();
        let chars: Vec<char> = text.chars().collect();
        let word = |i: usize| chars.get(i).is_some_and(|c| c.is_alphanumeric());
        let local = |i: usize| word(i) || matches!(chars[i], '.' | '-' | '_' | '+');
        lower.contains("http://")
            || lower.contains("https://")
            || lower.contains("www.")
            || (1..chars.len()).any(|i| chars[i] == '@' && local(i - 1) && word(i + 1))
    }

    /// No markdown marker survived into the output: a shipped document has to
    /// stay inside the subset `render_markdown` supports, or the page shows it
    /// raw. See [`raw_markdown_markers`] for what counts as a marker.
    fn assert_no_raw_markdown(md: &str, html: &str) {
        assert_eq!(
            raw_markdown_markers(md, html),
            Vec::<String>::new(),
            "markdown left raw by the renderer"
        );
    }

    /// Runs the guard on `md` as the renderer actually renders it.
    fn raw_markers_of(md: &str) -> Vec<String> {
        raw_markdown_markers(md, &render_markdown(md))
    }

    #[test]
    fn raw_markdown_guard_accepts_the_supported_subset() {
        let md = "# Titre\n\n## Section\n\n### Sous-section\n\n\
                  Un paragraphe avec **du gras**, du `code *étoilé* et 1. numéroté`,\n\
                  un [lien](https://example.org/a_b) et la loi du 21 mai 2024. Son I\n\
                  vaut aussi pour les identifiants comme snake_case.\n\n\
                  - une puce\n  qui continue\n- une autre\n\n\
                  | A | B |\n|---|---|\n| x | y |\n";
        assert_eq!(raw_markers_of(md), Vec::<String>::new());
    }

    #[test]
    fn raw_markdown_guard_catches_a_numbered_list() {
        // As its own block, and interrupting a paragraph: both flatten into
        // `<p>` text the reader sees with its numbers inline.
        assert!(!raw_markers_of("1. item\n2. autre\n").is_empty());
        assert!(!raw_markers_of("Texte :\n1. item\n2. autre\n").is_empty());
        assert!(!raw_markers_of("1) item\n").is_empty());
        // Indented under a bullet, it is glued to that bullet's text.
        assert!(!raw_markers_of("- puce\n  1. sous-item\n").is_empty());
    }

    #[test]
    fn raw_markdown_guard_catches_a_blockquote() {
        assert!(!raw_markers_of("> citation\n").is_empty());
        assert!(!raw_markers_of("Texte\n> citation\n").is_empty());
    }

    #[test]
    fn raw_markdown_guard_catches_single_emphasis() {
        assert!(!raw_markers_of("Un mot *souligné* ici.\n").is_empty());
        assert!(!raw_markers_of("Un mot _souligné_ ici.\n").is_empty());
        assert!(!raw_markers_of("- une *puce*\n").is_empty());
        assert!(!raw_markers_of("| A |\n|---|\n| *x* |\n").is_empty());
    }

    #[test]
    fn raw_markdown_guard_catches_a_thematic_break() {
        assert!(!raw_markers_of("Avant\n\n---\n\nAprès\n").is_empty());
        // Under a paragraph line it is a setext heading, just as unsupported.
        assert!(!raw_markers_of("Titre\n---\n").is_empty());
        assert!(!raw_markers_of("***\n").is_empty());
        assert!(!raw_markers_of("_ _ _\n").is_empty());
    }

    #[test]
    fn raw_markdown_guard_still_catches_the_original_markers() {
        // Bold left open, a deeper heading, and a heading glued to a paragraph.
        assert!(!raw_markers_of("Du **gras non fermé.\n").is_empty());
        assert!(!raw_markers_of("#### Trop profond\n").is_empty());
        assert!(!raw_markers_of("Texte\n#### Collé\n").is_empty());
    }

    /// Each of `inputs` leaves at least one marker for the guard to report.
    fn assert_each_caught(inputs: &[&str]) {
        for md in inputs {
            assert!(!raw_markers_of(md).is_empty(), "not caught: {md:?}");
        }
    }

    #[test]
    fn raw_markdown_guard_catches_a_link_the_renderer_refused() {
        // A refused URL leaves the whole `[label](url)` on the page.
        assert_each_caught(&[
            "Voir [s](www.example.org).\n",
            "Voir [l](architecture.md).\n",
            "Voir [x](javascript:alert(1)).\n",
            "- une [l](architecture.md)\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_a_link_whose_url_the_renderer_cut() {
        // The renderer ends the URL at the first `)` and keeps a title in the
        // `href`: the link points elsewhere than written, or shows a stray `)`.
        assert_each_caught(&[
            "Voir [x](https://e.org/A_(b)).\n",
            "Voir [x](https://e.org \"titre\").\n",
            "Voir [x](https://e.org 'titre').\n",
            "Voir [x](/a b).\n",
            "Voir [x](https://e.org/a\tb).\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_an_image() {
        // Even with an allowed URL: the renderer makes it `!<a>`.
        assert_each_caught(&[
            "![img](https://example.org/a.png)\n",
            "Logo ![img](/a.png) ici.\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_a_reference_link() {
        assert_each_caught(&[
            "Voir [a][b].\n",
            "Voir [a][].\n",
            "[b]: https://example.org\n",
            "Texte\n[b]: https://example.org\n",
            "[^1]: une note\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_strikethrough() {
        assert_each_caught(&["Du ~~barré~~ ici.\n", "Du ~barré~ ici.\n"]);
    }

    #[test]
    fn raw_markdown_guard_catches_an_unclosed_backtick() {
        assert_each_caught(&["Un `code non fermé.\n", "- une `puce\n"]);
    }

    #[test]
    fn raw_markdown_guard_catches_a_block_nested_in_a_bullet() {
        assert_each_caught(&[
            "- 1. item\n",
            "- 2024. item\n",
            "- > citation\n",
            "- #### titre\n",
            "- # Titre\n",
            "- ## Titre\n",
            "- ### Titre\n",
            "- #\tTitre\n",
            "- - sous-puce\n",
            "- + sous-puce\n",
            "- [ ] tâche\n",
            "- [x] tâche faite\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_a_setext_underline() {
        assert_each_caught(&["Titre\n===\n", "Titre\n=\n", "Titre\n--\n", "Titre\n-\n"]);
    }

    #[test]
    fn raw_markdown_guard_catches_other_bullet_markers() {
        assert_each_caught(&["+ item\n", "Texte\n+ item\n", "* item\n", "-\titem\n"]);
    }

    #[test]
    fn raw_markdown_guard_catches_code_blocks() {
        assert_each_caught(&[
            "```\ncode\n```\n",
            "```rust\nlet a = 1;\n```\n",
            "~~~\ncode\n~~~\n",
            "Avant\n\n    code indenté\n",
            "    code en tête\n",
            "\tcode tabulé\n",
            "# Titre\n    code sous un titre\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_raw_html() {
        assert_each_caught(&[
            "Du <b>gras</b> brut.\n",
            "<div>bloc</div>\n",
            "Lien <https://example.org> automatique.\n",
            "Texte <!-- commentaire -->\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_entities_and_escapes() {
        assert_each_caught(&[
            "&copy; 2026\n",
            "Le signe &#169;.\n",
            "\\*pas d'emphase\\*\n",
            "ligne\\\nsuite\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_a_closing_heading_sequence() {
        assert_each_caught(&["## Titre ##\n", "# Titre #\n"]);
    }

    #[test]
    fn raw_markdown_guard_catches_a_bullet_nested_under_another() {
        // The renderer puts it at the same level: the nesting is lost.
        assert_each_caught(&[
            "- puce\n  - sous-puce\n",
            "- puce\n    - sous-puce\n",
            "- puce\n\t- sous-puce\n",
            "Texte\n  - puce indentée\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_a_heading_under_a_bullet() {
        // CommonMark keeps it in the item; the renderer closes the list and
        // emits the heading after it.
        assert_each_caught(&[
            "- puce\n  # Titre\n",
            "- puce\n  ## Titre\n",
            "- puce\n  ### Titre\n",
            "- puce\n\t## Titre\n",
            "- puce\n  qui continue\n  ## Titre\n",
            "- puce\n\n  ## Titre\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_a_table_under_a_bullet() {
        // Indented, CommonMark keeps it in the item; flush against the bullet,
        // GFM reads its rows as the item's lazy continuation text. Either way
        // the renderer closes the list and emits a table after it.
        assert_each_caught(&[
            "- puce\n  | A | B |\n  |---|---|\n  | x | y |\n",
            "- puce\n| A | B |\n|---|---|\n| x | y |\n",
            "- puce\n  qui continue\n  | A | B |\n  |---|---|\n",
            "- puce\n\n  | A | B |\n  |---|---|\n  | x | y |\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_a_paragraph_indented_under_a_bullet_after_a_blank_line() {
        // CommonMark makes it a second paragraph of the item; the renderer
        // closes the list and emits a paragraph after it.
        assert_each_caught(&[
            "- puce\n\n  second paragraphe\n",
            "- puce\n\n\n  second paragraphe\n",
            "- puce\n  qui continue\n\n  second paragraphe\n",
            "- puce\n\n   second paragraphe\n",
            "- puce\n\n second paragraphe\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_text_flush_against_a_bullet() {
        // GFM reads it as the item's lazy continuation (`<li>puce suite</li>`);
        // the renderer closes the list and emits a paragraph after it.
        assert_each_caught(&[
            "- puce\nsuite\n",
            "- puce\n  qui continue\nsuite\n",
            "- puce\n#hashtag\n",
            "- a\nlazy\n\n  second\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_a_loose_list() {
        // GFM renders one loose `<ul>` (each item in a `<p>`); the renderer
        // renders two lists. Arbitrated 2026-10-04: refuse it.
        assert_each_caught(&["- a\n\n- b\n", "- a\n\n\n- b\n", "- a\n  suite\n\n- b\n"]);
    }

    #[test]
    fn raw_markdown_guard_names_the_block_glued_to_a_bullet() {
        // Flush left, the line is outside the item, so whatever block it
        // opens closes the item in CommonMark (cmark-gfm: `2. x` gives an
        // `<ol start="2">`, `+ ` an empty `<ul>`): it is that block, not
        // lazy continuation text. The "starts at 1, with content" rule only
        // binds a list interrupting a paragraph of its own container.
        for (md, finding) in [
            ("- a\n> cit\n", "blockquote under a bullet: > cit"),
            ("- a\n1. x\n", "numbered list under a bullet: 1. x"),
            ("- a\n2. x\n", "numbered list under a bullet: 2. x"),
            ("- a\n1.\n", "numbered list under a bullet: 1."),
            ("- a\n3)\n", "numbered list under a bullet: 3)"),
            ("- a\n+ \n", "bullet under a bullet: +"),
            ("- a\n* x\n", "bullet under a bullet: * x"),
            ("- a\n- \n", "empty bullet under a bullet: -"),
            ("- a\n<div>\n", "raw html under a bullet: <div>"),
            ("- a\n---\n", "rule under a bullet: ---"),
            ("- a\n***\n", "rule under a bullet: ***"),
            ("- a\n```\n", "code fence under a bullet: ```"),
            ("- a\n===\n", "lazy continuation text under a bullet: ==="),
            ("- a\n--\n", "lazy continuation text under a bullet: --"),
            (
                "- a\n[r]: /b\n",
                "lazy continuation text under a bullet: [r]: /b",
            ),
            (
                "- a\nsuite\n",
                "lazy continuation text under a bullet: suite",
            ),
        ] {
            // The first finding; `***` also leaves its stars in the item.
            assert_eq!(
                raw_markers_of(md).first().map(String::as_str),
                Some(finding),
                "{md:?}"
            );
        }
    }

    #[test]
    fn raw_markdown_guard_lets_a_block_follow_a_list_it_does_not_belong_to() {
        // A line glued to the bullet and indented continues the item, as the
        // renderer absorbs it; a block flush left after a blank line, or a
        // heading flush left, closes the list in both readings.
        for md in [
            "- puce\n  qui continue\n",
            "- puce\n      qui continue\n",
            "- puce\n\nparagraphe\n",
            "- puce\n## Titre\n",
            "- puce\n\n## Titre\n",
            "- puce\n\n| A | B |\n|---|---|\n| x | y |\n",
            "Texte\n\n  paragraphe indenté\n",
            "# Titre\n\n  paragraphe indenté\n",
        ] {
            assert_eq!(raw_markers_of(md), Vec::<String>::new(), "caught: {md:?}");
        }
    }

    #[test]
    fn raw_markdown_guard_catches_a_hard_line_break() {
        // Two trailing spaces break the line in CommonMark; the renderer
        // joins it with the next one.
        assert_each_caught(&["ligne  \nsuite\n", "- puce  \n  suite\n"]);
    }

    #[test]
    fn raw_markdown_guard_catches_a_bare_url_or_address() {
        // GFM turns them into links; the renderer leaves them as text.
        assert_each_caught(&[
            "Voir https://example.org ici.\n",
            "Voir http://example.org.\n",
            "Voir www.example.org.\n",
            "Écrire à contact@example.org.\n",
            "- puce vers www.example.org\n",
            "| A |\n|---|\n| https://example.org |\n",
            "Voir [le site](https://example.org) ou https://example.org.\n",
        ]);
        // Inside a link or a code span, they are what the reader should see.
        assert_eq!(
            raw_markers_of(
                "Écrire à [contact@example.org](mailto:contact@example.org), \
                 voir [www.example.org](https://www.example.org) ou \
                 `https://example.org`.\n"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn raw_markdown_guard_catches_a_bare_url_in_any_case() {
        assert_each_caught(&[
            "Voir HTTPS://EXAMPLE.ORG ici.\n",
            "Voir Http://example.org.\n",
            "Voir WWW.EXAMPLE.ORG.\n",
            "Voir Www.example.org.\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_catches_an_address_whose_local_part_ends_in_punctuation() {
        // GFM's local part admits `.`, `-`, `_` and `+` anywhere.
        assert_each_caught(&[
            "Écrire à a-@b.org.\n",
            "Écrire à a+@b.org.\n",
            "Écrire à a.@b.org.\n",
            "Écrire à a_@b.org.\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_lets_an_indented_line_continue_a_paragraph() {
        // Indented code cannot interrupt a paragraph: this is text.
        assert_eq!(
            raw_markers_of("Texte\n    suite indentée\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            raw_markers_of("- puce\n      suite indentée\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn raw_markdown_guard_reads_a_tab_as_the_blank_after_a_marker() {
        assert_each_caught(&[
            "1.\titem\n",
            "1)\titem\n",
            "- 1.\titem\n",
            "Texte\n1.\titem\n",
            "## Titre\t##\n",
            "##\tTitre\n",
        ]);
    }

    #[test]
    fn raw_markdown_guard_lets_a_wrapped_year_continue_a_paragraph() {
        // Only `1.` interrupts a paragraph in CommonMark: a wrapped line that
        // starts with another number continues the text, as rendered.
        assert_eq!(
            raw_markers_of("La loi du 21 mai\n2024. Son I s'applique.\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            raw_markers_of("Le langage C# reste du texte.\n"),
            Vec::<String>::new()
        );
        // Anywhere a list can start, the number is still refused.
        assert_each_caught(&[
            "2024. Son I\n",
            "Texte\n\n2024. Son I\n",
            "# Titre\n2024. Son I\n",
            "- puce\n2024. Son I\n",
            "Texte\n1. item\n",
            "Texte\n01. item\n",
        ]);
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
        // #131, #136: the controller's identity and the open subprocessor
        // questions are deliberately still placeholders, and exactly those of
        // `pending_release_values`. Filling them in for the public launch has
        // to come through that list.
        assert_eq!(release_placeholders(md), pending_release_values());
        assert!(!md.contains("placeholder_name"));
        // Every placeholder — the two of #131 and the three subprocessor
        // questions of #136 — survives the renderer as readable text rather
        // than being swallowed as a link label.
        for pending in pending_release_values() {
            assert!(
                html.contains(&format!("[{pending}")),
                "`{pending}` is not readable in the rendered policy"
            );
        }
        assert_no_raw_markdown(md, &html);
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
        // The deletion is a pass that runs once an hour
        // (`retention_purge::PURGE_INTERVAL`), and only while the API is up
        // *and* its database role bypasses RLS (#276, #284): the notice states
        // the frequency *and* both ways the pass stops — a service outage,
        // and a configuration that suspends it while the service stays up.
        // That it states no deadline on top is the next test's job.
        assert!(body.contains("toutes les heures"), "{body}");
        assert!(body.contains("interrompu"), "{body}");
        assert!(body.contains("configuration suspend ce passage"), "{body}");
        // The same two durations, repeated where the reader who ignores the
        // email looks (sentences 4 and 5 of `INVITATION_TIME_PHRASES`): each
        // phrase occurs in its own sentence only, so sentences 1 and 2
        // cannot keep these green.
        assert!(
            body.contains("cesse de fonctionner au bout de 7 jours"),
            "{body}"
        );
        assert!(body.contains("pendant 30 jours après cet envoi"), "{body}");
    }

    #[test]
    fn invitation_email_speaks_of_time_only_through_its_sanctioned_phrasings() {
        // The purge pass runs hourly and only while the API is up with a role
        // that bypasses RLS, so the 30th day is when the row becomes
        // purgeable and no bound on the deletion holds — "au plus tard 30
        // jours" and a flat hour both got written and had to be taken out
        // again (#273). What this pins is narrower than "no bound at all"
        // and says so: see `time_words_outside`. The processing register
        // carries the same reserve (`docs/registre-traitements.md`),
        // unguarded by this test.
        let body = invitation_sample();
        assert_eq!(
            time_words_outside(&body, &INVITATION_TIME_PHRASES),
            Vec::<String>::new(),
            "{body}"
        );
    }

    /// The only sentences through which the invitation email may speak of
    /// time, in the order the email carries them:
    ///
    /// 1. the link's lifetime and single use;
    /// 2. when the address is erased: at use, otherwise 30 days after sending;
    /// 3. the purge's frequency, hourly, and what an outage or a
    ///    configuration that suspends it does to it;
    /// 4. the link's lifetime again, for the reader who ignores the email;
    /// 5. the retention period again, in that same paragraph.
    ///
    /// Each entry is a whole sentence of the email, from its first word to
    /// its full stop, compared after whitespace flattening and lowercasing as
    /// `time_words_outside` does. Every one of them carries a word of
    /// [`TIME_WORDS`], so any change inside one other than whitespace or case
    /// — a word inserted, removed or replaced anywhere between its first word
    /// and its full stop — breaks the exact match, and whichever of its time
    /// words remain are then reported. A change that deletes every one of
    /// them leaves nothing to report here. That is why
    /// `invitation_email_states_the_purpose_the_legal_basis_and_the_retention`
    /// asserts present a phrase that no other sentence carries: "valable 7
    /// jours", "dès que le lien est utilisé" and "toutes les heures" for
    /// sentences 1, 2 and 3, "cesse de fonctionner au bout de 7 jours" and
    /// "pendant 30 jours après cet envoi" for sentences 4 and 5. Its other
    /// assertion, "30 jours après cet envoi", matches sentences 2 and 5 both.
    /// Text added before a sentence's first word or after its full stop
    /// leaves the sentence matched and is scanned on its own. Adding an entry
    /// here is a deliberate act, reviewed as such.
    const INVITATION_TIME_PHRASES: [&str; 5] = [
        "ce lien est valable 7 jours et ne sert qu'une fois.",
        "votre adresse est enregistrée avec cette invitation, puis effacée : \
         dès que le lien est utilisé, sinon 30 jours après cet envoi.",
        "cet effacement est fait par un passage automatique qui a lieu toutes \
         les heures quand le service fonctionne et que sa configuration le \
         permet ; si le service est interrompu, ou si sa configuration suspend \
         ce passage, il a lieu à son redémarrage ou au premier passage horaire \
         qui suit le rétablissement de cette configuration.",
        "si vous ne voulez pas de cette invitation, ignorez cet email : le lien \
         cesse de fonctionner au bout de 7 jours.",
        "votre adresse, elle, reste enregistrée avec l'invitation pendant 30 \
         jours après cet envoi, puis est effacée par ce passage automatique.",
    ];

    /// The words `time_words_outside` compares against, as exact lowercase
    /// forms — no lemmatization: a form absent from this list is not seen.
    /// Units of duration and their abbreviations, periods, frequencies, named
    /// days and moments, and words of deadline, deferral or immediacy. `hui`
    /// is what "aujourd'hui" reads as once its elision is dropped; `dès`
    /// appears in the email only inside sentence 2 of
    /// [`INVITATION_TIME_PHRASES`], so "dès que possible" elsewhere is
    /// reported.
    const TIME_WORDS: [&str; 73] = [
        "s",
        "sec",
        "seconde",
        "secondes",
        "mn",
        "min",
        "minute",
        "minutes",
        "h",
        "hr",
        "hrs",
        "heure",
        "heures",
        "horaire",
        "horaires",
        "j",
        "jour",
        "jours",
        "journée",
        "journées",
        "sem",
        "semaine",
        "semaines",
        "huitaine",
        "quinzaine",
        "mois",
        "trimestre",
        "trimestres",
        "semestre",
        "semestres",
        "an",
        "ans",
        "année",
        "années",
        "quotidien",
        "quotidienne",
        "hebdomadaire",
        "mensuel",
        "mensuelle",
        "demain",
        "lendemain",
        "hui",
        "maintenant",
        "soir",
        "minuit",
        "midi",
        "délai",
        "délais",
        "échéance",
        "échéances",
        "tard",
        "tarder",
        "tôt",
        "bientôt",
        "prochainement",
        "incessamment",
        "ultérieurement",
        "maximum",
        "max",
        "maxi",
        "plafond",
        "vite",
        "rapidement",
        "promptement",
        "immédiatement",
        "aussitôt",
        "sitôt",
        "dès",
        "instantanément",
        "instant",
        "instants",
        "suite",
        "champ",
    ];

    /// The words of `text` that are entries of [`TIME_WORDS`] once the
    /// `allowed` sentences are cut out, in reading order. An empty result means
    /// exactly that no such word stands outside those sentences, and nothing
    /// more: this is a scan against a closed list, not an understanding of
    /// the sentence.
    ///
    /// What the code does:
    ///
    /// 1. normalizes the text and each `allowed` sentence alike: NFKC, so
    ///    decomposed accents and compatibility forms (fullwidth letters) are
    ///    composed back; lowercase; every character of [`APOSTROPHE_LIKE`]
    ///    turned into the ASCII apostrophe; the space, the tab and the line
    ///    end flattened, so a sentence hard-wrapped across lines still
    ///    matches. Any other blank (U+00A0, U+202F, U+3000, U+2028, U+0085…)
    ///    is a space beside a separator or a blank, and is kept as is between
    ///    two characters of a word (`heu\u{a0}re`), out of NFKC's reach, for
    ///    step 4 to report (#376). Nothing is dropped;
    /// 2. cuts out every `allowed` sentence, as an exact substring;
    /// 3. splits the rest on the admitted separators only
    ///    (`is_admitted_separator`): the space, ASCII digits and punctuation
    ///    other than the apostrophe, `«`, `»`, `‘`, `“`, `”`, `–`, `—` — so
    ///    `48h`, `J+30`, `1h30`, `18h00` give `h`, `j`, `h`, `h`, `1h30min`
    ///    gives `h` and `min`, `sur-le-champ` gives `sur`, `le`, `champ`;
    /// 4. reports whole any run holding a character that is neither an
    ///    apostrophe nor a letter of a to z, bare or with diacritics, `œ` or
    ///    `æ` (`is_plain_latin`). This is an allow-list: whatever step 1 did
    ///    not fold into it — a look-alike from another script (Cyrillic `е`
    ///    in `hеure`) or from the Latin one (small capital `ʜ`, dotless `ı`,
    ///    IPA `ɑ`), an invisible character (Cf such as U+00AD or U+200D, a
    ///    mark NFKC leaves standing such as U+034F, U+FE0F or U+0334, an
    ///    unassigned code point, the braille blank U+2800), the combining dot
    ///    lowercasing leaves on `i` from `İ`, a symbol — keeps its run
    ///    together and has it reported, whether or not it imitates a list
    ///    word;
    /// 5. in each other run, keeps the last non-empty piece between
    ///    apostrophes: `l'heure` → `heure`, `j'ai` → `ai`, `s'il` → `il`,
    ///    `'heure'` → `heure`;
    /// 6. keeps the pieces equal to an entry of [`TIME_WORDS`].
    ///
    /// Known limits — what passes unreported:
    ///
    /// - a bound worded only with words absent from [`TIME_WORDS`]: "sous
    ///   peu", "sans attendre", "dans la foulée", "d'ici lundi", "avant le
    ///   1er janvier", "de manière immédiate";
    /// - such a bound added before an allowed sentence's first word or after
    ///   its full stop ("Au plus, ce lien est valable 7 jours…"): the
    ///   sentence still matches, and the addition carries no list word;
    /// - an inflected form the list does not spell out;
    /// - in a run joined by apostrophes, every piece but the last non-empty
    ///   one: a list word glued by an apostrophe to a following word with no
    ///   space (`heure'x`) is lost;
    /// - an allowed sentence copied verbatim anywhere else in the text;
    /// - a diacritic added to a list word that composes with its letter
    ///   (`heu\u{301}re` is `heúre` once NFKC has run): another word, like
    ///   any misspelling. A diacritic is not stripped, because `dès` and
    ///   `des` must stay apart.
    ///
    /// Known false positives: `suite` and `champ` outside "tout de suite" and
    /// "sur-le-champ"; `dès` outside sentence 2; `an` in "un an", a duration,
    /// so on purpose; `sec` as "dry", `min` as the short of "minimum", `midi`
    /// as the region; a lone letter cut out by digits or brackets — `2s`,
    /// `donnée(s)` give `s`; any run holding a character refused by step 4, a
    /// time word or not — another script, but also genuine Latin letters
    /// (`ł`, `ø`, `ß`, the small capitals as in `ᴊour`, the IPA letters) and
    /// every symbol outside the admitted separators (`€`, `°`, `✓`); two
    /// words joined by a blank other than the space (`le\u{a0}service`),
    /// since a cut word cannot be told from them. None of them occurs in the
    /// shipped email.
    fn time_words_outside(text: &str, allowed: &[&str]) -> Vec<String> {
        let mut rest = normalize_for_time_words(text);
        for phrase in allowed {
            rest = rest.replace(&normalize_for_time_words(phrase), " ");
        }
        rest.split(is_admitted_separator)
            .filter_map(|run| {
                if run.chars().any(|c| c != '\'' && !is_plain_latin(c)) {
                    return Some(run);
                }
                run.rsplit('\'')
                    .find(|word| !word.is_empty())
                    .filter(|word| TIME_WORDS.contains(word))
            })
            .map(str::to_string)
            .collect()
    }

    /// Step 1 of `time_words_outside`, applied to the text and to each
    /// allowed sentence alike: NFKC, lowercase, every apostrophe-like
    /// character turned into U+0027, the space, the tab and the line end
    /// flattened, any other blank kept inside a word. Nothing is dropped: a
    /// character this does not fold is left in place for step 4 to report.
    fn normalize_for_time_words(text: &str) -> String {
        let fold = |piece: &str| -> String {
            piece
                .nfkc()
                .collect::<String>()
                .to_lowercase()
                .chars()
                .map(|c| {
                    if APOSTROPHE_LIKE.contains(&c) {
                        '\''
                    } else {
                        c
                    }
                })
                .collect()
        };
        // A run of blanks other than the space, the tab and the line end
        // (`is_odd_blank`) between two characters of a word is kept as is,
        // out of NFKC's reach, so step 4 reports the word it would have cut
        // (#376); elsewhere it is a space.
        let chars: Vec<char> = text.chars().collect();
        let in_word =
            |c: Option<&char>| c.is_some_and(|&c| !c.is_whitespace() && !is_admitted_separator(c));
        let mut folded = String::new();
        let mut piece = String::new();
        let mut i = 0;
        while i < chars.len() {
            if !is_odd_blank(chars[i]) {
                piece.push(chars[i]);
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && is_odd_blank(chars[i]) {
                i += 1;
            }
            if start > 0 && in_word(chars.get(start - 1)) && in_word(chars.get(i)) {
                folded.push_str(&fold(&piece));
                piece.clear();
                folded.extend(&chars[start..i]);
            } else {
                piece.push(' ');
            }
        }
        folded.push_str(&fold(&piece));
        folded
            .split([' ', '\t', '\n', '\r'])
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Whitespace other than the space, the tab and the line end: what
    /// `str::split_whitespace` and NFKC turn into a word break unseen.
    fn is_odd_blank(c: char) -> bool {
        c.is_whitespace() && !matches!(c, ' ' | '\t' | '\n' | '\r')
    }

    /// The characters that separate words: the space, ASCII digits and
    /// ASCII punctuation except the apostrophe, and the French quotation
    /// marks and dashes — `«`, `»`, `‘`, `“`, `”`, `–`, `—`. A closed list:
    /// whatever is neither one of these, nor an apostrophe, nor a letter of
    /// `is_plain_latin` stays inside its run and has the run reported.
    fn is_admitted_separator(c: char) -> bool {
        (c == ' ' || c.is_ascii_graphic()) && !c.is_ascii_alphabetic() && c != '\''
            || matches!(
                c,
                '«' | '»' | '\u{2018}' | '\u{201c}' | '\u{201d}' | '\u{2013}' | '\u{2014}'
            )
    }

    /// Characters drawn as a raised comma or tick that the email could elide
    /// with: U+2019, the modifier letters, the Latin saltillo. None of them
    /// is admitted by step 4 of `time_words_outside`, so without this
    /// `l\u{2bd}heure` would be reported whole instead of read as `heure`.
    /// U+2018 is left out on purpose: it opens a quotation and is an
    /// admitted separator.
    const APOSTROPHE_LIKE: [char; 12] = [
        '\u{2019}', '\u{2b9}', '\u{2bb}', '\u{2bc}', '\u{2bd}', '\u{2be}', '\u{2bf}', '\u{2c8}',
        '\u{2ca}', '\u{2cb}', '\u{a78b}', '\u{a78c}',
    ];

    /// Whether a letter is one French is written with: a letter from a to z,
    /// bare or carrying diacritics (its canonical decomposition starts with
    /// an ASCII letter), or one of the ligatures `œ` and `æ`. Run after NFKC
    /// and lowercasing, so fullwidth and other compatibility forms are
    /// already folded. Every other letter is refused, Latin ones included:
    /// the small capitals (`ʜ`, `ᴊ`), the dotless `ı` and the IPA `ɑ` are
    /// Latin and still look-alikes, and `ł`, `ø`, `ß` go down with them.
    fn is_plain_latin(c: char) -> bool {
        c == 'œ'
            || c == 'æ'
            || std::iter::once(c)
                .nfd()
                .next()
                .is_some_and(|base| base.is_ascii_alphabetic())
    }

    #[test]
    fn time_words_outside_accepts_the_sanctioned_phrasings_even_hard_wrapped() {
        assert!(time_words_outside(
            "Ce lien est valable 7 jours et ne sert\nqu'une fois. Votre adresse est \
             enregistrée avec cette invitation, puis effacée :\ndès que le lien est \
             utilisé, sinon 30 jours après cet envoi. Cet\neffacement est fait par un \
             passage automatique qui a lieu\ntoutes les heures quand le service \
             fonctionne et que sa\nconfiguration le permet ; si le service est \
             interrompu, ou si sa\nconfiguration suspend ce passage, il a lieu à \
             son redémarrage ou au\npremier passage horaire qui suit le \
             rétablissement de cette\nconfiguration.",
            &INVITATION_TIME_PHRASES
        )
        .is_empty());
    }

    #[test]
    fn time_words_outside_catches_the_bound_that_slipped_past_the_two_literals() {
        // Reproduced while verifying #273: the former guard forbade two
        // literals and this sentence left it green.
        assert_eq!(
            time_words_outside("Sous une heure au maximum.", &INVITATION_TIME_PHRASES),
            vec!["heure".to_string(), "maximum".to_string()]
        );
    }

    #[test]
    fn time_words_outside_catches_the_two_bounds_already_taken_out_once() {
        assert_eq!(
            time_words_outside("au plus tard 30 jours", &INVITATION_TIME_PHRASES),
            vec!["tard".to_string(), "jours".to_string()]
        );
        assert_eq!(
            time_words_outside("dans l'heure qui suit", &INVITATION_TIME_PHRASES),
            vec!["heure".to_string()]
        );
    }

    #[test]
    fn time_words_outside_catches_a_bound_appended_to_an_allowed_phrasing() {
        assert_eq!(
            time_words_outside(
                "Effacée : dès que le lien est utilisé, sinon 30 jours après cet envoi au plus tard.",
                &INVITATION_TIME_PHRASES
            ),
            vec!["dès".to_string(), "jours".to_string(), "tard".to_string()]
        );
    }

    #[test]
    fn time_words_outside_reads_a_unit_glued_to_digits_on_any_side_and_is_case_blind() {
        // "1h30" and "18h00" carry digits after the unit too, "1h30min" on
        // both sides of one (#281 review).
        assert_eq!(
            time_words_outside(
                "Sous 48h, ou à J+30, sous 1h30, avant 18h00, en 1h30min. Délai garanti.",
                &INVITATION_TIME_PHRASES
            ),
            vec![
                "h".to_string(),
                "j".to_string(),
                "h".to_string(),
                "h".to_string(),
                "h".to_string(),
                "min".to_string(),
                "délai".to_string(),
            ]
        );
    }

    #[test]
    fn time_words_outside_catches_deadlines_worded_as_dates_or_periods() {
        assert_eq!(
            time_words_outside(
                "Demain, prochainement, à l'échéance, sous huitaine, sous quinzaine, ce \
                 trimestre, en 5 mn, sous 2 sem, sitôt reçu, dès que possible, aujourd'hui, \
                 maintenant.",
                &INVITATION_TIME_PHRASES
            ),
            vec![
                "demain".to_string(),
                "prochainement".to_string(),
                "échéance".to_string(),
                "huitaine".to_string(),
                "quinzaine".to_string(),
                "trimestre".to_string(),
                "mn".to_string(),
                "sem".to_string(),
                "sitôt".to_string(),
                "dès".to_string(),
                "hui".to_string(),
                "maintenant".to_string(),
            ]
        );
    }

    #[test]
    fn time_words_outside_pins_its_documented_limits() {
        // The documented limits, pinned so the doc-comment cannot drift from
        // them: bounds worded off the list, a word hidden by an apostrophe.
        assert!(time_words_outside("effacée sans attendre", &INVITATION_TIME_PHRASES).is_empty());
        assert!(time_words_outside("effacée sous peu", &INVITATION_TIME_PHRASES).is_empty());
        assert!(time_words_outside(
            "d'ici lundi, avant le 1er janvier, de manière immédiate",
            &INVITATION_TIME_PHRASES
        )
        .is_empty());
        assert!(time_words_outside("une heure'x", &INVITATION_TIME_PHRASES).is_empty());
        assert!(time_words_outside(
            "Au plus, ce lien est valable 7 jours et ne sert qu'une fois.",
            &INVITATION_TIME_PHRASES
        )
        .is_empty());
        // And documented false positives.
        assert_eq!(
            time_words_outside(
                "vos donnée(s), un temps sec, 3 min",
                &INVITATION_TIME_PHRASES
            ),
            vec!["s".to_string(), "sec".to_string(), "min".to_string()]
        );
    }

    #[test]
    fn time_words_outside_sees_a_list_word_through_its_encoding() {
        // The encoding bypasses #281 had to declare (#282). Those that fold
        // back into a list word: decomposed accents, a compatibility form, an
        // elision written with an apostrophe-like letter.
        for (bypass, word) in [
            ("sans de\u{301}lai", "délai"),
            ("dans l\u{2bd}heure", "heure"),
            ("dans l\u{2b9}heure", "heure"),
            ("dans l\u{a78c}heure", "heure"),
            ("une \u{ff48}eure", "heure"),
        ] {
            assert_eq!(
                time_words_outside(bypass, &INVITATION_TIME_PHRASES),
                vec![word.to_string()],
                "{bypass:?}"
            );
        }
        // Every other character is outside the admitted set and reported
        // with the run that carries it, never dropped (#330 review): Cf,
        // marks NFKC leaves standing, unassigned code points, the braille
        // blank, and the combining dot lowercasing leaves after `i` from `İ`.
        for (bypass, run) in [
            ("une heu\u{ad}re", "heu\u{ad}re"),
            ("une heu\u{200d}re", "heu\u{200d}re"),
            ("une heu\u{200b}re", "heu\u{200b}re"),
            ("une heu\u{34f}re", "heu\u{34f}re"),
            ("une heu\u{fe0f}re", "heu\u{fe0f}re"),
            ("une heu\u{180b}re", "heu\u{180b}re"),
            ("sans de\u{301}l\u{334}ai", "dél\u{334}ai"),
            ("une heu\u{2065}re", "heu\u{2065}re"),
            ("une heu\u{fff0}re", "heu\u{fff0}re"),
            ("une heu\u{e0080}re", "heu\u{e0080}re"),
            ("une heu\u{2800}re", "heu\u{2800}re"),
            ("à m\u{130}di", "mi\u{307}di"),
            ("à M\u{130}D\u{130}", "mi\u{307}di\u{307}"),
            ("une m\u{130}nute", "mi\u{307}nute"),
            ("une heure\u{ad}", "heure\u{ad}"),
        ] {
            assert_eq!(
                time_words_outside(bypass, &INVITATION_TIME_PHRASES),
                vec![run.to_string()],
                "{bypass:?}"
            );
        }
        // The allowed sentences go through the same normalization, so one
        // written with a curly apostrophe and decomposed accents still cuts
        // out its plain twin, and the other way round.
        assert!(time_words_outside(
            "Ce lien est valable 7 jours et ne sert qu'une fois.",
            &["ce lien est valable 7 jours et ne sert qu\u{2019}une fois."]
        )
        .is_empty());
        assert!(time_words_outside(
            "Ce lien est valable 7 jours et ne sert qu\u{2019}une fois.",
            &["CE LIEN EST VALABLE 7 JOURS ET NE SERT QU'UNE FOIS."]
        )
        .is_empty());
        assert!(time_words_outside(
            "dès que le lien est utilisé",
            &["de\u{300}s que le lien est utilise\u{301}"]
        )
        .is_empty());
    }

    #[test]
    fn time_words_outside_reports_a_word_cut_by_a_blank_other_than_the_space() {
        // #376: NFKC turns U+00A0, U+2009, U+202F, U+3000… into a space and
        // `split_whitespace` cuts on U+2028 and U+0085: `heu re` gave two
        // pieces off the list. A blank other than the space, the tab and the
        // line end, between two characters of a word, now keeps the word
        // together and has it reported.
        for blank in [
            '\u{a0}', '\u{1680}', '\u{2000}', '\u{2007}', '\u{2009}', '\u{200a}', '\u{202f}',
            '\u{205f}', '\u{3000}', '\u{2028}', '\u{2029}', '\u{85}', '\u{b}', '\u{c}',
        ] {
            let bypass = format!("une Heu{blank}re");
            assert_eq!(
                time_words_outside(&bypass, &INVITATION_TIME_PHRASES),
                vec![format!("heu{blank}re")],
                "{bypass:?}"
            );
        }
        // Two words joined by such a blank are reported too: a false
        // positive, since a cut word cannot be told from two words.
        assert_eq!(
            time_words_outside("le\u{a0}service", &INVITATION_TIME_PHRASES),
            vec!["le\u{a0}service".to_string()]
        );
        // Beside a digit or punctuation, it stays a separator, as French
        // typography uses it there.
        assert_eq!(
            time_words_outside(
                "30\u{a0}jours\u{202f}; effacée\u{a0}:",
                &INVITATION_TIME_PHRASES
            ),
            vec!["jours".to_string()]
        );
    }

    #[test]
    fn time_words_outside_reports_a_word_written_outside_the_latin_script() {
        // A look-alike letter from another script cannot be folded back to
        // the Latin one it imitates, so the word carrying it is reported
        // whole, list word or not (#282).
        assert_eq!(
            time_words_outside("une h\u{435}ure, un \u{3b1}n", &INVITATION_TIME_PHRASES),
            vec!["h\u{435}ure".to_string(), "\u{3b1}n".to_string()]
        );
        assert_eq!(
            time_words_outside("pour \u{43c}\u{438}\u{440}'heure", &INVITATION_TIME_PHRASES),
            vec!["\u{43c}\u{438}\u{440}'heure".to_string()]
        );
        // Look-alikes from inside the Latin script itself: small capitals,
        // the dotless i, the IPA alpha (#330 review).
        assert_eq!(
            time_words_outside(
                "une \u{29c}eure, à m\u{131}di, un \u{251}n, un \u{1d0a}our",
                &INVITATION_TIME_PHRASES
            ),
            vec![
                "\u{29c}eure".to_string(),
                "m\u{131}di".to_string(),
                "\u{251}n".to_string(),
                "\u{1d0a}our".to_string(),
            ]
        );
        // A letter of a to z, with or without diacritics, and the two French
        // ligatures stay words like any other.
        assert!(time_words_outside(
            "Œuvre, cœur, Æsope, naïve, Ça, Ÿ, élève, à, Ştefan",
            &INVITATION_TIME_PHRASES
        )
        .is_empty());
        // Every other Latin letter is reported too: a false positive, kept
        // because a look-alike cannot be told from a genuine letter.
        assert_eq!(
            time_words_outside("Łódź, Straße, Ærø", &INVITATION_TIME_PHRASES),
            vec!["łódź".to_string(), "straße".to_string(), "ærø".to_string()]
        );
        // So is any symbol outside the admitted punctuation, alone or not.
        assert_eq!(
            time_words_outside("10 €, 20 °C, ok ✓", &INVITATION_TIME_PHRASES),
            vec!["€".to_string(), "°c".to_string(), "✓".to_string()]
        );
        // The admitted punctuation separates words, as ASCII punctuation
        // does.
        assert_eq!(
            time_words_outside(
                "«\u{a0}heure\u{a0}» \u{2018}jour\u{2019} \u{201c}an\u{201d} mois\u{2013}an\u{2014}h",
                &INVITATION_TIME_PHRASES
            ),
            vec![
                "heure".to_string(),
                "jour".to_string(),
                "an".to_string(),
                "mois".to_string(),
                "an".to_string(),
                "h".to_string(),
            ]
        );
    }

    #[test]
    fn time_words_outside_does_not_let_an_allowed_duration_take_a_new_left_context() {
        // Reproduced while verifying #281: with the bare "30 jours après cet
        // envoi" allowed, both sentences below left the guard green. `dès`
        // is reported too: once the sentence is broken, nothing allows it.
        assert_eq!(
            time_words_outside(
                "Effacée : dès que le lien est utilisé, sinon au plus 30 jours après cet envoi.",
                &INVITATION_TIME_PHRASES
            ),
            vec!["dès".to_string(), "jours".to_string()]
        );
        assert_eq!(
            time_words_outside(
                "Effacée : dès que le lien est utilisé, sinon dans les 30 jours après cet envoi.",
                &INVITATION_TIME_PHRASES
            ),
            vec!["dès".to_string(), "jours".to_string()]
        );
    }

    #[test]
    fn time_words_outside_catches_seconds_and_words_of_immediacy() {
        assert_eq!(
            time_words_outside(
                "Sous 3600 secondes, en moins de 60 s, ou 1 hr : bientôt, au plus tôt, \
                 tout de suite, sur-le-champ, à l'instant.",
                &INVITATION_TIME_PHRASES
            ),
            vec![
                "secondes".to_string(),
                "s".to_string(),
                "hr".to_string(),
                "bientôt".to_string(),
                "tôt".to_string(),
                "suite".to_string(),
                "champ".to_string(),
                "instant".to_string(),
            ]
        );
    }

    #[test]
    fn time_words_outside_reads_an_elided_word_as_the_word_after_the_apostrophe() {
        // "j'ai" and "s'il" are not a day and a second; "l'heure" is an hour,
        // whichever of U+0027, U+2019 or U+02BC writes the apostrophe.
        assert_eq!(
            time_words_outside(
                "J'ai reçu, s\u{2019}il le faut, dans l'heure ou dans l\u{2bc}heure.",
                &INVITATION_TIME_PHRASES
            ),
            vec!["heure".to_string(), "heure".to_string()]
        );
    }

    #[test]
    fn a_qualifier_inserted_inside_an_allowed_sentence_breaks_it() {
        // Reproduced while verifying #281: with sentence 5 anchored only up to
        // its comma, this mutation of the shipped email left every test green.
        let body = invitation_sample().replacen("envoi, puis est", "envoi,\nau plus, puis est", 1);
        assert_eq!(
            time_words_outside(&body, &INVITATION_TIME_PHRASES),
            vec!["jours".to_string()]
        );
        // Same for sentence 3, which used to stop at its semicolon.
        let body = invitation_sample().replacen("le permet ; si", "le permet ; au plus, si", 1);
        assert_eq!(
            time_words_outside(&body, &INVITATION_TIME_PHRASES),
            vec!["heures".to_string(), "horaire".to_string()]
        );
    }

    #[test]
    fn time_words_outside_reads_a_word_closed_by_a_quote_or_an_apostrophe() {
        // Reproduced while verifying #281: a word followed by a closing quote
        // was read as the empty string after it, and missed.
        assert_eq!(
            time_words_outside(
                "Sous \u{2018}une heure\u{2019}, sous 'une heure', dans l\u{2bb}heure.",
                &INVITATION_TIME_PHRASES
            ),
            vec![
                "heure".to_string(),
                "heure".to_string(),
                "heure".to_string()
            ]
        );
    }

    #[test]
    fn time_words_outside_catches_the_last_period_and_deferral_words() {
        assert_eq!(
            time_words_outside(
                "Plages horaires, ce semestre, sans tarder, incessamment, \
                 ultérieurement, ce soir.",
                &INVITATION_TIME_PHRASES
            ),
            vec![
                "horaires".to_string(),
                "semestre".to_string(),
                "tarder".to_string(),
                "incessamment".to_string(),
                "ultérieurement".to_string(),
                "soir".to_string(),
            ]
        );
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
        // `account_purge.rs` deletes only the invitations an account sent
        // (#139), not one addressed to it, `export_account` does not export
        // it, and otherwise the retention purge removes it, 30 days on
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

    // -- deactivation_notice_email_body (#256) -------------------------------

    fn notice_sample() -> String {
        // Deactivated on 10/03/2026 in Paris; purgeable from 10/03/2028.
        deactivation_notice_email_body(at(2026, 3, 10, 9, 0), at(2028, 3, 10, 9, 0), POLICY_URL)
    }

    #[test]
    fn deactivation_notice_dates_the_deactivation_and_the_purge() {
        let body = notice_sample();
        assert!(body.contains("10/03/2026"), "{body}");
        assert!(body.contains("10/03/2028"), "{body}");
    }

    #[test]
    fn deactivation_notice_dates_the_purge_on_the_paris_calendar_day() {
        // 23:30 UTC on 9 March is already 10 March in Paris.
        let body = deactivation_notice_email_body(
            at(2026, 3, 10, 9, 0),
            at(2028, 3, 9, 23, 30),
            POLICY_URL,
        );
        assert!(body.contains("10/03/2028"), "{body}");
    }

    #[test]
    fn deactivation_notice_says_what_the_purge_erases_and_what_stays() {
        let body = notice_sample();
        for words in [
            "adresse email",
            "nom",
            "mot de passe",
            "groupes",
            "contenus",
        ] {
            assert!(body.contains(words), "missing {words:?}: {body}");
        }
    }

    #[test]
    fn deactivation_notice_sends_the_reactivation_request_through_a_login() {
        // #289: the right credentials open the page that carries the
        // request form, and a pending request suspends the purge — unless
        // one was already refused since the deactivation, the case in which
        // a second warning can go out (arbitrage of 2026-09-29). The
        // controller, named by the same two placeholders as the RGPD
        // documents' (#131), stays the contact for the other rights.
        let body = notice_sample();
        assert!(body.contains("demander sa réactivation"), "{body}");
        assert!(body.contains("connectez-vous"), "{body}");
        assert!(body.contains("suspend"), "{body}");
        assert!(body.contains("déjà été refusée"), "{body}");
        assert!(
            !body.contains("sans que\npersonne puisse s'y connecter"),
            "{body}"
        );
        assert_eq!(release_placeholders(&body), pending_controller_values());
    }

    #[test]
    fn deactivation_notice_carries_the_right_to_lodge_a_complaint_and_the_policy() {
        let body = notice_sample();
        assert!(body.contains("réclamation auprès de la CNIL"), "{body}");
        assert!(body.contains(POLICY_URL), "{body}");
    }

    #[test]
    fn deactivation_notice_points_at_no_repository_path() {
        assert_eq!(repo_path_references(&notice_sample()), Vec::<String>::new());
    }

    #[test]
    fn deactivation_notice_is_wrapped_for_a_plain_text_reader() {
        for line in notice_sample().lines() {
            let is_lone_url = !line.contains(char::is_whitespace) && line.contains("https://");
            assert!(
                line.chars().count() <= 78 || is_lone_url,
                "line too long for a plain-text email: {line}"
            );
        }
    }

    // -- ownership_inherited_email_body (#323) -------------------------------

    use crate::validation::groups::OwnershipReason;

    const GROUPS_URL: &str = "https://maison.example.org/groups";

    fn ownership_sample(reason: OwnershipReason) -> String {
        ownership_inherited_email_body("Famille Martin", reason, GROUPS_URL, POLICY_URL)
    }

    const REASONS: [OwnershipReason; 3] = [
        OwnershipReason::AccountPurged,
        OwnershipReason::MemberReactivated,
        OwnershipReason::DesignatedBySupport,
    ];

    #[test]
    fn ownership_email_names_the_group_on_its_own_line_and_carries_both_links() {
        for reason in REASONS {
            let body = ownership_sample(reason);
            assert!(body.lines().any(|l| l == "« Famille Martin »"), "{body}");
            assert!(body.lines().any(|l| l == GROUPS_URL), "{body}");
            assert!(body.lines().any(|l| l == POLICY_URL), "{body}");
        }
    }

    #[test]
    fn ownership_email_says_why_the_ownership_came_to_the_reader() {
        let purged = ownership_sample(OwnershipReason::AccountPurged);
        assert!(
            purged.contains("ancien propriétaire a été supprimé"),
            "{purged}"
        );
        let reactivated = ownership_sample(OwnershipReason::MemberReactivated);
        assert!(
            reactivated.contains("n'avait plus de propriétaire"),
            "{reactivated}"
        );
        assert!(reactivated.contains("réactivation"), "{reactivated}");
        let designated = ownership_sample(OwnershipReason::DesignatedBySupport);
        assert!(
            designated.contains("n'avait plus de propriétaire"),
            "{designated}"
        );
        assert!(
            designated.contains("administrateur du service"),
            "{designated}"
        );
        assert_ne!(purged, reactivated);
        assert_ne!(reactivated, designated);
    }

    #[test]
    fn ownership_email_says_what_an_owner_can_do_and_what_it_holds_back() {
        let body = ownership_sample(OwnershipReason::AccountPurged);
        for words in [
            "inviter",
            "transférer la propriété",
            "supprimer le groupe",
            "suppression de votre compte",
        ] {
            assert!(body.contains(words), "missing {words:?}: {body}");
        }
    }

    #[test]
    fn ownership_email_cannot_be_forged_through_the_group_name() {
        let body = ownership_inherited_email_body(
            "Famille\n\n-- Ce que vous pouvez faire --\nÉcrivez à pirate@example.test",
            OwnershipReason::AccountPurged,
            GROUPS_URL,
            POLICY_URL,
        );
        assert!(
            !body
                .lines()
                .any(|l| l.starts_with("-- Ce que vous pouvez faire")),
            "{body}"
        );
        assert!(
            !body.lines().any(|l| l.starts_with("Écrivez à pirate")),
            "{body}"
        );
    }

    #[test]
    fn ownership_email_points_at_no_repository_path() {
        for reason in REASONS {
            assert_eq!(
                repo_path_references(&ownership_sample(reason)),
                Vec::<String>::new()
            );
        }
    }

    #[test]
    fn ownership_email_is_wrapped_for_a_plain_text_reader() {
        for reason in REASONS {
            for line in ownership_sample(reason).lines() {
                let is_lone_url = !line.contains(char::is_whitespace) && line.contains("https://");
                assert!(
                    line.chars().count() <= 78 || is_lone_url,
                    "line too long for a plain-text email: {line}"
                );
            }
        }
    }
}
