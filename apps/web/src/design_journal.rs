//! The guard that reads DESIGN.md's decision journal back (#95).
//!
//! DESIGN.md's journal carries a convention, written by #72 and narrowed by
//! the arbitration of 2026-08-29: **an entry that has already landed on
//! `main` is not rewritten by a later batch.** What may be added to it is a
//! *renvoi* — a pointer to the entry that corrects or completes it — which
//! changes no claim the entry makes.
//!
//! Nothing enforced that. The batch that wrote the rule broke it four times
//! in three commits, and the seventh round of verification found it by
//! diffing revisions by hand. A convention no test reads is a convention
//! that holds only while everyone remembers it.
//!
//! **What compares against what — and what it will compare against.** The
//! landed state is recorded in a versioned companion file,
//! `DESIGN.journal.lock`, one line per entry; the test recomputes those lines
//! from DESIGN.md and demands an exact match.
//!
//! **That file is an interim, and its replacement is already decided.** The
//! comparison point is to become `main` itself —
//! `git show origin/main:DESIGN.md` — which is the only arrangement a pull
//! request cannot edit. It is not here yet because reaching for it means
//! changing the workflow (CI checks out at depth 1, so `origin/main` is not
//! even fetched), which doubles this batch; it goes to a follow-up issue.
//! Read what follows as the state of a thing on its way there, not as a
//! design settled on a file.
//!
//! **What that makes impossible.** A *silent* rewrite: any edit to a landed
//! entry turns the suite red, and the only way past a red test is to edit a
//! file whose single purpose is to record what landed — an edit that shows
//! up in the diff as a changed line in the middle of the lock rather than as
//! lines appended at its end.
//!
//! **What it does not.** Two holes, both real, both written here rather than
//! left for a reader to find:
//!
//! 1. **A *declared* rewrite**, *for as long as the comparison point is a
//!    file*. Nothing stops an author editing the entry and the lock in the
//!    same commit; no file-based check can, the file being as writable as the
//!    document. This is the hole the move to `main` closes, and it is the
//!    reason that move was decided rather than left open: a pull request can
//!    rewrite anything it carries, and it carries the lock.
//! 2. **The body of a well-formed renvoi is not frozen.** Stripping renvois
//!    before comparing is what keeps *adding* one allowed, and the price is
//!    that whatever sits inside one can later be reworded without moving the
//!    fingerprint. `points_at_a_dated_entry` narrows this a great deal — a
//!    parenthesis has to both name itself and name another dated entry, so
//!    `(le Renvoi de ce choix est resté sans suite)` is no longer a free
//!    writing zone — but a well-formed `(Renvoi : … — voir l'entrée du
//!    AAAA-MM-JJ)` still is. A length cap was considered and dropped: any
//!    number picked for it would be arbitrary, and it would buy confidence
//!    without closing the hole, since a sentence is enough to mislead.
//!
//! A third, smaller one is closed rather than declared: a table row that does
//! not split into three cells used to be skipped in silence, which would land
//! an entry on `main` outside the lock for good. It now fails loudly — see
//! `entries`.
//!
//! Compiled under `cfg(test)` only: DESIGN.md is 46 KB of French prose and
//! has no business inside the shipped binary.

/// One row of the journal table.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// The `Date` cell, verbatim.
    pub(crate) date: String,
    /// The `Décision` cell, verbatim.
    pub(crate) decision: String,
    /// The `Motif` cell, verbatim — renvois included.
    pub(crate) motif: String,
}

/// How a renvoi names itself. Two spellings rather than one because the
/// journal already holds both: most segments say `Renvoi`, and the one added
/// to the 2026-08-04 entry on the declarations ceiling says only
/// `voir l'entrée`. Normalising that one would mean rewriting a landed entry
/// to install the guard that forbids rewriting landed entries.
const RENVOI_MARKERS: [&str; 2] = ["Renvoi", "voir l'entrée"];

/// …and where it must point. A renvoi designates **another entry of this
/// journal**, which is what makes it survive the squash and stay checkable;
/// a parenthesis that merely says the word is not one.
///
/// This second condition is the whole difference between a renvoi and a free
/// writing zone inside a frozen entry. Without it, `(le Renvoi de ce choix
/// est resté sans suite)` appended to a landed entry is stripped before
/// comparison — and then *its* contents can be rewritten at will, forever,
/// inside an entry the lock claims to freeze.
fn points_at_a_dated_entry(inner: &str) -> bool {
    let needle = "l'entrée du ";
    let mut rest = inner;
    while let Some(at) = rest.find(needle) {
        rest = &rest[at + needle.len()..];
        let date: Vec<char> = rest.chars().take(10).collect();
        if date.len() == 10
            && date[..4].iter().all(char::is_ascii_digit)
            && date[4] == '-'
            && date[5..7].iter().all(char::is_ascii_digit)
            && date[7] == '-'
            && date[8..].iter().all(char::is_ascii_digit)
        {
            return true;
        }
    }
    false
}

/// The journal's rows, in table order.
///
/// Anchored on the `## Journal des décisions` heading: the document holds
/// several other three-column tables, and the token scale is not a journal.
pub(crate) fn entries(markdown: &str) -> Vec<Entry> {
    const HEADING: &str = "## Journal des décisions";
    let Some(at) = markdown.find(HEADING) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut inside = false;
    for line in markdown[at..].lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            // Prose before the table is the convention that governs it; the
            // first non-row line after it is the end of the journal.
            if inside {
                break;
            }
            continue;
        }
        inside = true;
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        // The header row and the `|---|` separator are not decisions.
        if cells.len() == 3 && (cells[0] == "Date" || cells[0].starts_with("---")) {
            continue;
        }
        // Anything else that is not three cells is a row this guard cannot
        // freeze — a `\|` inside a motif is the realistic way to get here,
        // and skipping it silently would let an entry land on `main` outside
        // the lock, for good. Loud beats quiet.
        assert_eq!(
            cells.len(),
            3,
            "journal row splits into {} cells, not 3 — the guard cannot \
             fingerprint it and would otherwise skip it in silence. A pipe \
             inside a cell is the usual cause; write it some other way (a \
             slash, or the word). Row: {line}",
            cells.len()
        );
        out.push(Entry {
            date: cells[0].to_string(),
            decision: cells[1].to_string(),
            motif: cells[2].to_string(),
        });
    }
    out
}

/// Whitespace collapsed to single spaces and trimmed, so that lifting a
/// renvoi out of the middle of a motif — or reflowing a row — does not
/// fingerprint the same entry two ways.
fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A motif with its renvois removed — the part of an entry that a later
/// batch may not touch.
pub(crate) fn without_renvois(motif: &str) -> String {
    let mut out = String::with_capacity(motif.len());
    let mut rest = motif;
    while let Some(open) = rest.find('(') {
        // An unclosed parenthesis is prose, not a truncated renvoi: stop
        // scanning rather than swallow everything after it.
        let Some(offset) = rest[open..].find(')') else {
            break;
        };
        let close = open + offset;
        let inner = &rest[open + 1..close];
        let is_renvoi = RENVOI_MARKERS.iter().any(|marker| inner.contains(marker))
            && points_at_a_dated_entry(inner);
        if !is_renvoi {
            out.push_str(&rest[..=close]);
            rest = &rest[close + 1..];
            continue;
        }
        // A renvoi is usually written in italics, so the `*` hugging it goes
        // with the segment: left behind they would form runs that say
        // nothing and would still move the fingerprint.
        let start = rest[..open].trim_end_matches('*').len();
        let after = &rest[close + 1..];
        let end = close + 1 + (after.len() - after.trim_start_matches('*').len());
        out.push_str(&rest[..start]);
        rest = &rest[end..];
    }
    out.push_str(rest);
    normalized(&out)
}

/// One line of `DESIGN.journal.lock`: the fingerprint, the date, and enough
/// of the decision to make a diff legible.
pub(crate) fn lock_line(entry: &Entry) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let date = normalized(&entry.date);
    let decision = normalized(&entry.decision);
    // The unit separator cannot appear in a markdown table cell, so no
    // shuffling of text between the three columns can collide.
    let frozen = format!(
        "{date}\u{1f}{decision}\u{1f}{}",
        without_renvois(&entry.motif)
    );
    let digest = Sha256::digest(frozen.as_bytes());
    let hex = digest.iter().take(8).fold(String::new(), |mut acc, byte| {
        let _ = write!(acc, "{byte:02x}");
        acc
    });
    // Sixty characters of the decision, so a reviewer reading the lock's
    // diff sees *which* entry moved without opening DESIGN.md.
    let label: String = decision.chars().take(60).collect();
    format!("{hex}  {date}  {label}")
}

/// The whole lock file as DESIGN.md says it should be.
pub(crate) fn lock_file(markdown: &str) -> String {
    let mut out = String::from(LOCK_HEADER);
    for entry in entries(markdown) {
        out.push_str(&lock_line(&entry));
        out.push('\n');
    }
    out
}

/// Written into the lock itself, so whoever opens that file next knows what
/// it is without having to find this module first.
const LOCK_HEADER: &str = "\
# Empreintes des entrées atterries du journal de DESIGN.md (#95).
#
# Une ligne par entrée, dans l'ordre du tableau :
#   <empreinte>  <date>  <début de la décision>
# L'empreinte couvre la date, la décision et le motif *privé de ses renvois* :
# ajouter un renvoi à une entrée atterrie est la seule modification que la
# convention autorise, et c'est la seule qui ne bouge pas cette ligne.
#
# Étape intérimaire : le point de comparaison doit devenir `main` lui-même
# (`git show origin/main:DESIGN.md`), seul montage qu'une PR ne puisse pas
# éditer. Décidé, reporté à une issue de suivi — ça touche la CI.
#
# Ce fichier ne s'édite pas à la main. Après avoir ajouté des entrées :
#   cargo test -p manage_our_home_web design_journal_lock_print -- --nocapture
# et recopier la sortie ici. Une ligne *ajoutée à la fin* est une entrée
# nouvelle ; une ligne *modifiée au milieu* est une entrée atterrie réécrite,
# ce que DESIGN.md → Journal des décisions interdit.
#
# Le garde-fou : apps/web/src/design_journal.rs
";

#[cfg(test)]
mod tests {
    use super::*;

    const DESIGN: &str = include_str!("../../../DESIGN.md");
    const LOCK: &str = include_str!("../../../DESIGN.journal.lock");

    // ---- parsing ----------------------------------------------------

    #[test]
    fn a_journal_row_splits_into_its_three_cells() {
        let doc = "\
## Journal des décisions

| Date | Décision | Motif |
|---|---|---|
| 2026-07-29 | Système de design initial | Créé par `/design-consultation` |
";
        assert_eq!(
            entries(doc),
            vec![Entry {
                date: "2026-07-29".into(),
                decision: "Système de design initial".into(),
                motif: "Créé par `/design-consultation`".into(),
            }]
        );
    }

    #[test]
    fn tables_before_the_journal_heading_are_not_journal_entries() {
        let doc = "\
## Layout

| Jeton | Valeur | Pages |
|---|---|---|
| `--w-form` | 28rem | authentification |

## Journal des décisions

| Date | Décision | Motif |
|---|---|---|
| 2026-07-29 | Une décision | Un motif |
";
        let got = entries(doc);
        assert_eq!(got.len(), 1, "got {got:#?}");
        assert_eq!(got[0].date, "2026-07-29");
    }

    #[test]
    fn the_table_ends_at_the_first_line_that_is_not_a_row() {
        let doc = "\
## Journal des décisions

| Date | Décision | Motif |
|---|---|---|
| 2026-07-29 | Une décision | Un motif |

Du texte après la table.

| Autre | Table | Ignorée |
|---|---|---|
| a | b | c |
";
        assert_eq!(entries(doc).len(), 1);
    }

    #[test]
    fn the_real_journal_parses_into_dated_entries() {
        let got = entries(DESIGN);
        assert!(
            got.len() > 40,
            "DESIGN.md's journal should hold every decision ever taken, got {}",
            got.len()
        );
        for entry in &got {
            assert!(
                entry.date.len() == 10 && entry.date.starts_with("2026-"),
                "a journal row must start with an ISO date, got {:?}",
                entry.date
            );
            assert!(!entry.decision.is_empty() && !entry.motif.is_empty());
        }
    }

    // ---- renvois ----------------------------------------------------

    #[test]
    fn a_motif_without_a_renvoi_is_returned_unchanged() {
        assert_eq!(
            without_renvois("Évite une migration ; `GroupMember` reste inchangé"),
            "Évite une migration ; `GroupMember` reste inchangé"
        );
    }

    #[test]
    fn a_parenthesised_segment_that_says_renvoi_and_points_at_an_entry_comes_out() {
        assert_eq!(
            without_renvois("Le motif. *(Renvoi : voir l'entrée du 2026-08-05.)*"),
            "Le motif."
        );
    }

    #[test]
    fn a_renvoi_that_only_points_at_another_entry_comes_out_too() {
        // The form the 2026-08-04 entry already carries: no `Renvoi`, but an
        // unambiguous pointer all the same.
        assert_eq!(
            without_renvois("Les 77 octets sont intacts *(le plafond a été relevé depuis, par #72 — voir l'entrée du 2026-08-05)*"),
            "Les 77 octets sont intacts"
        );
    }

    /// The probe that broke the first version of this guard: a parenthesis
    /// that says the word but points nowhere. It was stripped, which turned
    /// the inside of a landed entry into a zone nothing freezes.
    #[test]
    fn a_parenthesis_that_says_renvoi_but_points_nowhere_is_not_one() {
        assert_eq!(
            without_renvois("Fraunces en titres (le Renvoi de ce choix est resté sans suite)"),
            "Fraunces en titres (le Renvoi de ce choix est resté sans suite)"
        );
    }

    /// Same probe, second turn: once inside such a parenthesis, anything
    /// could be rewritten. It must move the fingerprint.
    #[test]
    fn rewriting_inside_a_pointerless_parenthesis_moves_the_fingerprint() {
        let before = without_renvois("Un motif (le Renvoi de ce choix est resté sans suite)");
        let after = without_renvois(
            "Un motif (Renvoi — en fait ce choix a été annulé et ce paragraphe dit le contraire)",
        );
        assert_ne!(before, after);
    }

    #[test]
    fn a_renvoi_must_name_a_dated_entry_not_just_a_date() {
        // A date on its own is not a pointer to an entry.
        assert_eq!(
            without_renvois("Un motif (Renvoi : mesuré le 2026-08-05.)"),
            "Un motif (Renvoi : mesuré le 2026-08-05.)"
        );
    }

    #[test]
    fn a_malformed_date_after_the_pointer_does_not_count() {
        assert_eq!(
            without_renvois("Un motif (Renvoi : voir l'entrée du 2026-8-5.)"),
            "Un motif (Renvoi : voir l'entrée du 2026-8-5.)"
        );
    }

    #[test]
    fn an_ordinary_parenthesis_is_not_a_renvoi() {
        // The journal is full of these, and dropping one would let a batch
        // rewrite a landed claim by parenthesising it.
        assert_eq!(
            without_renvois("Classes de soutien ajoutées (#68) au tableau (neuf de plus)"),
            "Classes de soutien ajoutées (#68) au tableau (neuf de plus)"
        );
    }

    #[test]
    fn two_renvois_in_one_motif_both_come_out() {
        assert_eq!(
            without_renvois(
                "A (Renvoi : un, voir l'entrée du 2026-08-05) B \
                 *(Renvoi : deux, voir l'entrée du 2026-08-06)* C"
            ),
            "A B C"
        );
    }

    #[test]
    fn stripping_a_renvoi_leaves_no_double_space_behind() {
        // The comparison is on the stripped text, so its whitespace has to be
        // canonical or the same entry fingerprints two ways.
        assert_eq!(
            without_renvois("A  *(Renvoi : voir l'entrée du 2026-08-05)*   B"),
            "A B"
        );
    }

    #[test]
    fn an_unclosed_parenthesis_does_not_eat_the_rest_of_the_motif() {
        assert_eq!(
            without_renvois("Un motif (Renvoi : jamais fermé"),
            "Un motif (Renvoi : jamais fermé"
        );
    }

    // ---- the lock ---------------------------------------------------

    #[test]
    fn a_lock_line_carries_the_date_and_a_readable_label() {
        let entry = Entry {
            date: "2026-07-29".into(),
            decision: "Système de design initial".into(),
            motif: "Un motif".into(),
        };
        let line = lock_line(&entry);
        assert!(line.contains("2026-07-29"), "{line}");
        assert!(line.contains("Système de design initial"), "{line}");
    }

    #[test]
    fn adding_a_renvoi_does_not_change_a_lock_line() {
        // This is the whole point: the convention lets a later batch add a
        // renvoi to a landed entry, and only that.
        let landed = Entry {
            date: "2026-07-29".into(),
            decision: "Une décision".into(),
            motif: "Un motif".into(),
        };
        let with_renvoi = Entry {
            date: "2026-07-29".into(),
            decision: "Une décision".into(),
            motif: "Un motif *(Renvoi : voir l'entrée du 2026-08-05.)*".into(),
        };
        assert_eq!(lock_line(&landed), lock_line(&with_renvoi));
    }

    #[test]
    fn rewording_a_landed_motif_changes_its_lock_line() {
        let landed = Entry {
            date: "2026-07-29".into(),
            decision: "Une décision".into(),
            motif: "La feuille pèse 10 926 o".into(),
        };
        let reworded = Entry {
            date: "2026-07-29".into(),
            decision: "Une décision".into(),
            motif: "La feuille pèse 11 000 o".into(),
        };
        assert_ne!(lock_line(&landed), lock_line(&reworded));
    }

    #[test]
    fn rewording_a_landed_decision_changes_its_lock_line() {
        let landed = Entry {
            date: "2026-07-29".into(),
            decision: "Une décision".into(),
            motif: "Un motif".into(),
        };
        let reworded = Entry {
            date: "2026-07-29".into(),
            decision: "Une autre décision".into(),
            motif: "Un motif".into(),
        };
        assert_ne!(lock_line(&landed), lock_line(&reworded));
    }

    #[test]
    fn redating_a_landed_entry_changes_its_lock_line() {
        let landed = Entry {
            date: "2026-07-29".into(),
            decision: "Une décision".into(),
            motif: "Un motif".into(),
        };
        let redated = Entry {
            date: "2026-07-30".into(),
            decision: "Une décision".into(),
            motif: "Un motif".into(),
        };
        assert_ne!(lock_line(&landed), lock_line(&redated));
    }

    #[test]
    fn a_lock_line_holds_no_newline_of_its_own() {
        let entry = Entry {
            date: "2026-07-29".into(),
            decision: "Une\ndécision".into(),
            motif: "Un motif".into(),
        };
        assert!(!lock_line(&entry).contains('\n'));
    }

    #[test]
    #[should_panic(expected = "splits into 4 cells")]
    fn a_row_carrying_a_pipe_is_a_failure_not_a_skip() {
        // Silently skipping it would land an entry on `main` outside the
        // lock, unfrozen for good.
        let doc = "\
## Journal des décisions

| Date | Décision | Motif |
|---|---|---|
| 2026-09-01 | Une décision future | Un motif citant un sélecteur `a \\| b` |
";
        entries(doc);
    }

    // ---- the guard itself -------------------------------------------

    /// The one that fails when a landed entry is rewritten.
    #[test]
    fn no_landed_journal_entry_has_been_rewritten() {
        let recomputed = lock_file(DESIGN);
        // Line by line rather than `assert_eq!` on the whole file: the lock
        // is sixty-odd lines, and a diff of two sixty-line blobs buries the
        // one line that moved.
        let mine: Vec<&str> = recomputed.lines().collect();
        let landed: Vec<&str> = LOCK.lines().collect();
        let divergence = mine
            .iter()
            .zip(&landed)
            .position(|(a, b)| a != b)
            .map(|i| {
                format!(
                    "ligne {} :\n  verrou   : {}\n  DESIGN.md: {}",
                    i + 1,
                    landed[i],
                    mine[i]
                )
            })
            .unwrap_or_else(|| {
                let (extra, side) = if mine.len() > landed.len() {
                    (&mine[landed.len()..], "en trop dans DESIGN.md")
                } else {
                    (
                        &landed[mine.len()..],
                        "dans le verrou et plus dans DESIGN.md",
                    )
                };
                format!(
                    "{} ligne(s) {side} :\n  {}",
                    extra.len(),
                    extra.join("\n  ")
                )
            });
        assert!(
            recomputed == LOCK,
            "{}{divergence}\n",
            "\n\nDESIGN.journal.lock no longer matches DESIGN.md's journal.\n\
             \n\
             * Appended lines only, at the end: you added journal entries. \
             Copy the recomputed file over DESIGN.journal.lock and commit \
             both — that is how an entry is frozen.\n\
             * A changed line in the middle: you rewrote an entry that has \
             already landed on `main`, which the convention in DESIGN.md → \
             Journal des décisions forbids. An entry says what was true the \
             day it was taken; a decision that overturns it is a new entry at \
             the end, and what may be added to the old one is a *renvoi*: a \
             parenthesised segment that both names itself (`Renvoi`, or \
             `voir l'entrée`) and points at a dated entry of this journal \
             (`l'entrée du AAAA-MM-JJ`). That shape, and only that shape, is \
             stripped before comparing — which is what keeps adding one \
             allowed while a parenthesis that merely says the word stays \
             frozen with the rest.\n\
             * A removed line: a landed entry is gone. It does not come back \
             out of the record.\n\
             \n\
             Print the recomputed file with:\n\
             cargo test -p manage_our_home_web design_journal_lock_print -- --nocapture\n\
             \n\
             Où ça diverge — "
        );
    }

    /// Not an assertion — the way to regenerate the lock after appending
    /// entries. Named so `--nocapture` on it prints the file and nothing else.
    #[test]
    fn design_journal_lock_print() {
        println!("{}", lock_file(DESIGN));
    }
}
