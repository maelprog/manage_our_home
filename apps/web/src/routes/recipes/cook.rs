//! `/recipes/:id/cook` — cooking a recipe step by step (#823).

/// A duration written in a step: the bytes of the step it covers and the
/// seconds a timer for it counts.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub start: usize,
    pub end: usize,
    pub seconds: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Hour,
    Minute,
    Second,
}

impl Unit {
    fn seconds(self) -> f64 {
        match self {
            Unit::Hour => 3600.0,
            Unit::Minute => 60.0,
            Unit::Second => 1.0,
        }
    }
}

/// The words a duration is written in, the longer before the shorter that
/// begins it (`minutes` before `min`). Read without case.
const UNITS: [(&str, Unit); 13] = [
    ("heures", Unit::Hour),
    ("heure", Unit::Hour),
    ("h", Unit::Hour),
    ("minutes", Unit::Minute),
    ("minute", Unit::Minute),
    ("mins", Unit::Minute),
    ("min", Unit::Minute),
    ("mn", Unit::Minute),
    ("secondes", Unit::Second),
    ("seconde", Unit::Second),
    ("secs", Unit::Second),
    ("sec", Unit::Second),
    ("s", Unit::Second),
];

/// `i` past the blanks at `i`, the narrow and the non-breaking ones
/// included.
fn blanks(s: &str, i: usize) -> usize {
    i + s[i..].len() - s[i..].trim_start_matches([' ', '\t', '\u{a0}', '\u{202f}']).len()
}

/// The digits at `i`: their value and where they end.
fn digits(s: &str, i: usize) -> Option<(u32, usize)> {
    let len = s[i..].len() - s[i..].trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if len == 0 || len > 6 {
        return None;
    }
    s[i..i + len].parse().ok().map(|n| (n, i + len))
}

/// The number starting at `i` — whole, or with a decimal comma or point —
/// unless it is the end of a word or of another number (`x2`, `1/2`,
/// the `5` of `1,5`).
fn number(s: &str, i: usize) -> Option<(f64, usize)> {
    let before = s[..i].chars().next_back();
    if before.is_some_and(|c| c.is_alphanumeric() || matches!(c, '/' | ',' | '.')) {
        return None;
    }
    let (whole, end) = digits(s, i)?;
    if let Some(sep) = s[end..].chars().next().filter(|c| matches!(c, ',' | '.')) {
        if let Some((_, frac_end)) = digits(s, end + sep.len_utf8()) {
            let text = s[i..frac_end].replace(',', ".");
            return text.parse().ok().map(|n| (n, frac_end));
        }
    }
    Some((f64::from(whole), end))
}

/// The unit written at `i`, after blanks, as a whole word, and where it
/// ends.
fn unit(s: &str, i: usize) -> Option<(Unit, usize)> {
    let at = blanks(s, i);
    let rest = &s[at..];
    UNITS.iter().find_map(|&(word, unit)| {
        let head = rest.get(..word.len())?;
        let whole = !rest[word.len()..]
            .chars()
            .next()
            .is_some_and(char::is_alphabetic);
        (head.eq_ignore_ascii_case(word) && whole).then_some((unit, at + word.len()))
    })
}

/// The minutes after an hour, `30` in `1 h 30`: two digits, or any number
/// of them with their unit (`1 h 5 min`). Their value and where they end.
fn minutes_after_hour(s: &str, i: usize) -> Option<(u32, usize)> {
    let at = blanks(s, i);
    let (m, end) = digits(s, at)?;
    if m >= 60 {
        return None;
    }
    match unit(s, end) {
        Some((Unit::Minute, after)) => Some((m, after)),
        _ => (end - at == 2 && !s[end..].starts_with(char::is_alphanumeric)).then_some((m, end)),
    }
}

/// The duration written at `i` — a number and its unit, an hour with its
/// minutes — in seconds, its unit and where it ends.
fn duration_at(s: &str, i: usize) -> Option<(f64, Unit, usize)> {
    let (n, end) = number(s, i)?;
    let (unit, end) = unit(s, end)?;
    let seconds = n * unit.seconds();
    if unit == Unit::Hour {
        if let Some((m, end)) = minutes_after_hour(s, end) {
            return Some((seconds + f64::from(m) * 60.0, unit, end));
        }
    }
    Some((seconds, unit, end))
}

/// Past the word joining the two ends of a range at `i` — a dash, « à »,
/// « ou » — and the blanks around it.
fn range_joint(s: &str, i: usize) -> Option<usize> {
    let at = blanks(s, i);
    let rest = &s[at..];
    let joint = ['-', '–', '—']
        .into_iter()
        .find(|&d| rest.starts_with(d))
        .map(char::len_utf8)
        .or_else(|| {
            ["à ", "ou ", "à\u{a0}"]
                .into_iter()
                .find(|w| rest.starts_with(w))
                .map(str::len)
        })?;
    Some(blanks(s, at + joint))
}

/// The durations written in a step, in order: « 10 min », « 45 secondes »,
/// « 1 h 30 », « 1h30 », « 1,5 h ». A range counts its low end
/// (arbitrage of 2026-11-17: one can always cook longer, not less):
/// « 8-10 min », « 8 à 10 minutes », « 1 h à 1 h 30 ». A number that is
/// not followed by a unit of time is not one (« 2 cm », « 180 °C »), and
/// neither is a duration of zero.
pub(crate) fn durations(step: &str) -> Vec<Found> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < step.len() {
        let Some((n, after_n)) = step[i..]
            .starts_with(|c: char| c.is_ascii_digit())
            .then(|| number(step, i))
            .flatten()
        else {
            i += step[i..].chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let range_end = |from: usize| {
            let j = range_joint(step, from)?;
            duration_at(step, j)
        };
        let found = match duration_at(step, i) {
            // « 1 h à 1 h 30 »: both ends written whole.
            Some((low, _, end)) => match range_end(end) {
                Some((high, _, end)) => Some((low.min(high), end)),
                None => Some((low, end)),
            },
            // « 8-10 min »: the low end takes the unit of the high one.
            None => {
                range_end(after_n).map(|(high, unit, end)| (high.min(n * unit.seconds()), end))
            }
        };
        match found {
            Some((seconds, end)) => {
                let seconds = seconds.round();
                if seconds >= 1.0 && seconds <= f64::from(u32::MAX) {
                    out.push(Found {
                        start: i,
                        end,
                        seconds: seconds as u32,
                    });
                }
                i = end;
            }
            None => i = after_n,
        }
    }
    out
}

/// A timer's length as its button writes it: « 45 s », « 8 min »,
/// « 1 h 30 », « 1 min 30 s ».
pub(crate) fn timer_label(seconds: u32) -> String {
    let (h, m, s) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    let mut out = match (h, m) {
        (0, 0) => String::new(),
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m:02}"),
    };
    if s > 0 {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&format!("{s} s"));
    }
    out
}

/// A timer's length in words, for a screen reader: « 1 heure 30 minutes ».
pub(crate) fn timer_words(seconds: u32) -> String {
    let (h, m, s) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    [(h, "heure"), (m, "minute"), (s, "seconde")]
        .into_iter()
        .filter(|&(n, _)| n > 0)
        .map(|(n, word)| format!("{n} {word}{}", if n > 1 { "s" } else { "" }))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `durations` finds in `step`: the text of each and its seconds.
    fn found(step: &str) -> Vec<(&str, u32)> {
        durations(step)
            .into_iter()
            .map(|f| (&step[f.start..f.end], f.seconds))
            .collect()
    }

    #[test]
    fn the_durations_of_a_step_are_found_with_their_unit() {
        // The table of #823, written before the parser.
        let cases: &[(&str, &[(&str, u32)])] = &[
            ("Faire revenir 10 min.", &[("10 min", 600)]),
            ("Laisser mijoter 45 minutes", &[("45 minutes", 2700)]),
            ("Cuire 1 minute", &[("1 minute", 60)]),
            ("Cuire 15 mn", &[("15 mn", 900)]),
            ("Cuire 20 MIN", &[("20 MIN", 1200)]),
            ("Cuire 1 h 30", &[("1 h 30", 5400)]),
            ("Cuire 1h30 au four", &[("1h30", 5400)]),
            ("Cuire 1 h 30 min", &[("1 h 30 min", 5400)]),
            ("Cuire 2 h 05 minutes", &[("2 h 05 minutes", 7500)]),
            ("Cuire 1 heure", &[("1 heure", 3600)]),
            ("Cuire 2 heures", &[("2 heures", 7200)]),
            ("Cuire 1 h", &[("1 h", 3600)]),
            ("Cuire 1,5 h", &[("1,5 h", 5400)]),
            ("Remuer 45 secondes", &[("45 secondes", 45)]),
            ("Remuer 30 s", &[("30 s", 30)]),
            ("Remuer 30 sec.", &[("30 sec", 30)]),
            ("Cuire 10\u{a0}min", &[("10\u{a0}min", 600)]),
        ];
        for (step, want) in cases {
            assert_eq!(found(step), *want, "{step}");
        }
    }

    #[test]
    fn a_range_counts_its_low_end() {
        // Arbitrage of 2026-11-17: one can always cook longer, never less.
        let cases: &[(&str, &[(&str, u32)])] = &[
            ("Faire revenir 8-10 min", &[("8-10 min", 480)]),
            ("Cuire 2-3 min", &[("2-3 min", 120)]),
            ("Cuire 8 à 10 minutes", &[("8 à 10 minutes", 480)]),
            ("Cuire 8 – 10 min", &[("8 – 10 min", 480)]),
            ("Cuire 8 ou 10 min", &[("8 ou 10 min", 480)]),
            ("Cuire 1 à 2 h", &[("1 à 2 h", 3600)]),
            ("Cuire 1 h à 1 h 30", &[("1 h à 1 h 30", 3600)]),
            ("Cuire 50 min à 1 h", &[("50 min à 1 h", 3000)]),
            // Written high to low, the low end is still the one counted.
            ("Cuire 10-8 min", &[("10-8 min", 480)]),
        ];
        for (step, want) in cases {
            assert_eq!(found(step), *want, "{step}");
        }
    }

    #[test]
    fn a_step_can_hold_several_durations() {
        assert_eq!(
            found("Ajouter l'ail, faire revenir 45 secondes, puis couvrir et laisser mijoter 45 minutes."),
            [("45 secondes", 45), ("45 minutes", 2700)]
        );
        // A duration, then a temperature: not a range.
        assert_eq!(found("Cuire 25 min à 180 °C"), [("25 min", 1500)]);
    }

    #[test]
    fn a_step_without_a_duration_finds_none() {
        for step in [
            "Servir tiède.",
            "",
            "Tailler en cubes de 2 cm.",
            "Préchauffer le four à 180 °C.",
            "Pour 2 hommes et 3 enfants.",
            "Ajouter 5 sachets de levure.",
            "Couper 3 mini-tomates.",
            "Ajouter 200 g de farine.",
            "Cuire 0 min.",
            "Ajouter 1/2 h de repos",
            "Ajouter 2 à 3 personnes",
            "Th6 puis x2 min",
        ] {
            assert_eq!(found(step), [], "{step}");
        }
    }

    #[test]
    fn a_timer_is_labelled_short_and_said_in_words() {
        let cases = [
            (45, "45 s", "45 secondes"),
            (60, "1 min", "1 minute"),
            (90, "1 min 30 s", "1 minute 30 secondes"),
            (480, "8 min", "8 minutes"),
            (3600, "1 h", "1 heure"),
            (5400, "1 h 30", "1 heure 30 minutes"),
            (7500, "2 h 05", "2 heures 5 minutes"),
            (1, "1 s", "1 seconde"),
        ];
        for (seconds, label, words) in cases {
            assert_eq!(timer_label(seconds), label, "{seconds}");
            assert_eq!(timer_words(seconds), words, "{seconds}");
        }
    }
}
