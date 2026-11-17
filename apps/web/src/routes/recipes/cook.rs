//! `/recipes/:id/cook` — cooking a recipe step by step (#823), offered on
//! every recipe's page whether it is planned or not.
//!
//! The server writes two things. What any browser shows: the ingredients and
//! the numbered steps, as on the recipe's page, in a larger type. And,
//! `hidden`, the step-by-step view `enhance.js` swaps in (`data-cook`): one
//! step at a time, « Étape 3 sur 6 », a timer button for each duration the
//! step's text gives ([`durations`]), the ingredients in a `<dialog>`, the
//! screen kept on. The timers, their sound, the step shown, kept per recipe
//! in the browser, and the Wake Lock are the script's alone; without it the
//! page is the readable list and nothing else.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use manage_our_home_shared::dto::recipes::RecipeResponse;
use manage_our_home_shared::validation::recipes::{duration_label, servings_label, steps_of};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::icons::{icon, Icon};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::detail::ingredient_html;
use super::{
    can_modify, family_context, recipe_not_found_page, recipes_cookie, service_unavailable_page,
};

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
) -> Response {
    let Some(fam) =
        family_context(&state, &headers, &me, &format!("/recipes/{recipe_id}/cook")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let recipe = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/recipes/{}", fam.gid, recipe_id),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<RecipeResponse>(resp.body) {
                Ok(r) => r,
                Err(_) => return service_unavailable_page().into_response(),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return recipe_not_found_page().into_response()
        }
        _ => return service_unavailable_page().into_response(),
    };
    let can_edit = can_modify(&fam.role, recipe.created_by == me.user_id);
    Html(page(&fam.header, &recipe, can_edit)).into_response()
}

/// The page: the step-by-step view when the recipe has steps, a card saying
/// it has none otherwise — with « Modifier la recette » for whoever may.
fn page(header: &str, recipe: &RecipeResponse, can_edit: bool) -> String {
    let id = recipe.id;
    let name = html_escape(&recipe.name);
    let steps = steps_of(recipe.instructions.as_deref().unwrap_or_default());
    let quit = format!(
        r#"<a class="quit" href="/recipes/{id}">{back}Quitter le mode cuisine</a>"#,
        back = icon(Icon::Previous),
    );
    let body = if steps.is_empty() {
        let edit = if can_edit {
            format!(r#"<a class="btn" href="/recipes/{id}/edit">Modifier la recette</a>"#)
        } else {
            String::new()
        };
        format!(
            r#"{quit}<div class="cook-head"><h1 class="cook-title">{name}</h1></div>
<section class="card"><h2>Pas d'étapes pour cette recette</h2><p>Ajoutez-en pour cuisiner pas à pas : une étape par ligne, avec les durées (« 10 min »).</p>
<div class="actions">{edit}<a class="btn secondary" href="/recipes/{id}">Retour à la recette</a></div></section>"#
        )
    } else {
        format!(
            r#"<div data-cook="{id}">{plain}{ui}</div>"#,
            plain = plain_html(recipe, &steps),
            ui = stepper_html(recipe, &steps, &quit),
        )
    };
    shell_with_header(
        Width::Read,
        &format!("Cuisiner : {}", recipe.name),
        header,
        &body,
    )
}

/// « Temps : 1 h 30. Pour 6 personnes. », as much of it as the recipe says.
fn facts(recipe: &RecipeResponse) -> String {
    let time = recipe
        .duration_minutes
        .map(|m| format!("Temps : {}.", duration_label(m)));
    let servings = recipe
        .servings
        .map(|s| format!("Pour {}.", servings_label(s)));
    let said: Vec<String> = time.into_iter().chain(servings).collect();
    if said.is_empty() {
        String::new()
    } else {
        format!(r#"<p class="muted">{}</p>"#, said.join(" "))
    }
}

/// The ingredients' `<li>`s, as on the recipe's page.
fn ingredient_items(recipe: &RecipeResponse) -> String {
    if recipe.ingredients.is_empty() {
        r#"<li class="muted">Aucun ingrédient renseigné.</li>"#.to_string()
    } else {
        recipe.ingredients.iter().map(ingredient_html).collect()
    }
}

/// What the page is without JavaScript, and until the script has run: the
/// recipe's ingredients and steps, readable from the hob.
fn plain_html(recipe: &RecipeResponse, steps: &[String]) -> String {
    let id = recipe.id;
    let items: String = steps
        .iter()
        .map(|s| format!("<li>{}</li>", html_escape(s)))
        .collect();
    format!(
        r#"<div data-cook-plain>
<a class="quit" href="/recipes/{id}">{back}Retour à la recette</a>
<h1>Cuisiner : {name}</h1>
{facts}
<h2 id="ingredients">Ingrédients</h2><ul class="nojs-list">{ingredients}</ul>
<h2>Étapes</h2><ol class="nojs-list">{items}</ol>
<div class="links"><a href="/recipes/{id}">Retour à la recette</a></div>
</div>"#,
        back = icon(Icon::Previous),
        name = html_escape(&recipe.name),
        facts = facts(recipe),
        ingredients = ingredient_items(recipe),
    )
}

/// The step-by-step view, `hidden` until `enhance.js` shows it in place of
/// [`plain_html`]: the head (« Étape 1 sur N », its gauge, the screen kept
/// on), the timers, « Ingrédients », every step (the first one shown), the
/// buttons to move, the ingredients' dialog and the row of a timer.
fn stepper_html(recipe: &RecipeResponse, steps: &[String], quit: &str) -> String {
    let id = recipe.id;
    let n = steps.len();
    let cards: String = steps
        .iter()
        .enumerate()
        .map(|(i, s)| step_card(i + 1, s))
        .collect();
    format!(
        r##"<div data-cook-ui hidden>{quit}
<div class="cook-head"><h1 class="cook-title">{name}</h1>
<p class="eyebrow" aria-live="polite" data-cook-at>Étape 1 sur {n}</p>
<progress max="{n}" value="1" aria-hidden="true"></progress>
<p class="wake" role="status" hidden>{check}Écran maintenu allumé</p></div>
<div class="cook-layout">
<section class="timers" aria-label="Minuteurs" hidden><h2>Minuteurs</h2><div data-cook-timers></div></section>
<div><a class="btn secondary ing-btn" href="#ingredients" data-dialog="cook-ingredients" aria-haspopup="dialog">{list}Ingrédients</a>
{cards}</div>
</div>
<div class="cook-nav"><button type="button" class="secondary" data-cook-prev>{prev}Précédent</button><button type="button" data-cook-next>Suivant{next}</button><a class="btn" href="/recipes/{id}" data-cook-end hidden>{check}Terminer</a></div>
<dialog id="cook-ingredients" class="cook-ing" aria-labelledby="cook-ingredients-title"><h2 id="cook-ingredients-title">Ingrédients</h2>{facts}<ul>{ingredients}</ul><form method="dialog"><button type="submit" class="secondary">Fermer</button></form></dialog>
<template data-cook-row><div class="trow"><p class="what"></p><span class="left" role="timer"></span><div class="tbtns"><button type="button" class="secondary" data-cook-pause>Pause</button><button type="button" class="secondary" data-cook-stop>{close}</button><button type="button" data-cook-ring>Arrêter</button></div></div></template>
</div>"##,
        name = html_escape(&recipe.name),
        check = icon(Icon::Check),
        list = icon(Icon::List),
        prev = icon(Icon::Previous),
        next = icon(Icon::Next),
        close = icon(Icon::Close),
        facts = facts(recipe),
        ingredients = ingredient_items(recipe),
    )
}

/// Step `n`, `hidden` but for the first: its text in large type, each
/// duration in it marked, and a button that starts a timer for each — or
/// the words saying it has none.
fn step_card(n: usize, step: &str) -> String {
    let found = durations(step);
    let mut text = String::new();
    let mut at = 0;
    for f in &found {
        text.push_str(&html_escape(&step[at..f.start]));
        text.push_str(&format!(
            r#"<span class="dur">{}</span>"#,
            html_escape(&step[f.start..f.end])
        ));
        at = f.end;
    }
    text.push_str(&html_escape(&step[at..]));
    let timers = if found.is_empty() {
        r#"<p class="none">Pas de durée dans cette étape.</p>"#.to_string()
    } else {
        let buttons: String = found
            .iter()
            .map(|f| {
                let label = timer_label(f.seconds);
                format!(
                    r#"<button type="button" class="secondary" data-cook-start="{s}" data-cook-what="Étape {n} · {label}" aria-label="Lancer un minuteur de {words}">{clock}Minuteur {label}</button>"#,
                    s = f.seconds,
                    words = timer_words(f.seconds),
                    clock = icon(Icon::Clock),
                )
            })
            .collect();
        format!(r#"<div class="timers-start">{buttons}</div>"#)
    };
    let hidden = if n == 1 { "" } else { " hidden" };
    format!(
        r#"<section class="card step" aria-label="Étape {n}" data-step{hidden}><p class="big">{text}</p>{timers}</section>"#
    )
}

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

    /// The unit a number written bare after this one counts: the minutes
    /// of `1 h 30`, the seconds of `1 min 30`.
    fn next(self) -> Option<Unit> {
        match self {
            Unit::Hour => Some(Unit::Minute),
            Unit::Minute => Some(Unit::Second),
            Unit::Second => None,
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
    i + s[i..].len()
        - s[i..]
            .trim_start_matches([' ', '\t', '\u{a0}', '\u{202f}'])
            .len()
}

/// The digits at `i`: their value and where they end.
fn digits(s: &str, i: usize) -> Option<(u32, usize)> {
    let len = s[i..].len()
        - s[i..]
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
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

/// The part of a duration that follows its `last` unit at `i`, `30` in
/// `1 h 30` or `1 min 30 s`: under 60, with a smaller unit than `last`, or
/// two digits without one, which count the next unit down. Its seconds, its
/// unit — `None` when it was bare, which ends the duration — and where it
/// ends.
fn part_after(s: &str, i: usize, last: Unit) -> Option<(u32, Option<Unit>, usize)> {
    let at = blanks(s, i);
    let (n, end) = digits(s, at)?;
    if n >= 60 {
        return None;
    }
    match unit(s, end) {
        Some((u, after)) if u.seconds() < last.seconds() => {
            Some((n * u.seconds() as u32, Some(u), after))
        }
        Some(_) => None,
        None => {
            let bare = end - at == 2 && !s[end..].starts_with(char::is_alphanumeric);
            let u = last.next().filter(|_| bare)?;
            Some((n * u.seconds() as u32, None, end))
        }
    }
}

/// The duration written at `i` — a number and its unit, then any smaller
/// units after it (`1 h 30`, `1 min 30 s`, `2 h 15 secondes`) — in
/// seconds, its first unit and where it ends.
fn duration_at(s: &str, i: usize) -> Option<(f64, Unit, usize)> {
    let (n, end) = number(s, i)?;
    let (first, mut end) = unit(s, end)?;
    let mut seconds = n * first.seconds();
    let mut last = first;
    while let Some((more, unit, after)) = part_after(s, end, last) {
        seconds += f64::from(more);
        end = after;
        match unit {
            Some(u) => last = u,
            None => break,
        }
    }
    Some((seconds, first, end))
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
/// « 1 h 30 », « 1h30 », « 1,5 h », « 1 min 30 s » — one timer for one
/// duration however many units it is written in. A range counts its low end
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
            None => range_end(after_n).map(|(high, unit, end)| (high.min(n * unit.seconds()), end)),
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
    fn a_duration_written_in_several_units_is_one_timer() {
        let cases: &[(&str, &[(&str, u32)])] = &[
            // The seconds after the minutes, as the minutes after the hour.
            ("Cuire 1 min 30", &[("1 min 30", 90)]),
            ("Cuire 1 min 30 s", &[("1 min 30 s", 90)]),
            ("Cuire 1 min 30 secondes", &[("1 min 30 secondes", 90)]),
            ("Cuire 1 h 30 min 20 s", &[("1 h 30 min 20 s", 5420)]),
            // Its unit says what the number after the hour counts.
            ("Cuire 2 h 15 secondes", &[("2 h 15 secondes", 7215)]),
            // Past 59, or a lone digit without its unit, it is not part of
            // the duration before it.
            ("Cuire 1 h 90", &[("1 h", 3600)]),
            ("Cuire 1 h 60", &[("1 h", 3600)]),
            ("Cuire 1 min 75 s", &[("1 min", 60), ("75 s", 75)]),
            ("Cuire 1 h 3 tomates", &[("1 h", 3600)]),
            ("Cuire 1 min 5", &[("1 min", 60)]),
            // A smaller unit only: « 10 min 2 h » is two durations.
            ("Cuire 10 min 2 h", &[("10 min", 600), ("2 h", 7200)]),
            ("Cuire 10 min 2 min", &[("10 min", 600), ("2 min", 120)]),
            ("Cuire 1 min 30 à 2 min", &[("1 min 30 à 2 min", 90)]),
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

    #[test]
    fn a_step_marks_its_durations_and_offers_a_timer_for_each() {
        let html = step_card(
            3,
            "Ajouter l'ail <vert>, 45 secondes, puis mijoter 8-10 min.",
        );
        assert_eq!(
            html,
            concat!(
                r#"<section class="card step" aria-label="Étape 3" data-step hidden>"#,
                r#"<p class="big">Ajouter l'ail &lt;vert&gt;, <span class="dur">45 secondes</span>, puis mijoter <span class="dur">8-10 min</span>.</p>"#,
                r#"<div class="timers-start">"#,
                r#"<button type="button" class="secondary" data-cook-start="45" data-cook-what="Étape 3 · 45 s" aria-label="Lancer un minuteur de 45 secondes">"#,
                r#"<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/></svg>Minuteur 45 s</button>"#,
                r#"<button type="button" class="secondary" data-cook-start="480" data-cook-what="Étape 3 · 8 min" aria-label="Lancer un minuteur de 8 minutes">"#,
                r#"<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/></svg>Minuteur 8 min</button>"#,
                r#"</div></section>"#,
            )
        );
    }

    #[test]
    fn the_first_step_is_shown_and_a_step_without_duration_says_so() {
        let html = step_card(1, "Servir tiède.");
        assert_eq!(
            html,
            r#"<section class="card step" aria-label="Étape 1" data-step><p class="big">Servir tiède.</p><p class="none">Pas de durée dans cette étape.</p></section>"#
        );
    }

    fn recipe(instructions: Option<&str>) -> RecipeResponse {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::from_u128(7),
            "group_id": Uuid::nil(),
            "name": "Ratatouille",
            "instructions": instructions,
            "duration_minutes": 90,
            "servings": 6,
            "created_by": Uuid::nil(),
            "created_at": "2026-11-14T10:00:00Z",
            "updated_at": "2026-11-14T10:00:00Z",
            "ingredients": [{
                "id": Uuid::nil(),
                "name": "Aubergines",
                "quantity": 2.0,
                "unit": null,
                "is_optional": false,
                "seasonal_months": null,
            }],
        }))
        .unwrap()
    }

    #[test]
    fn the_page_is_a_readable_list_and_a_hidden_stepper() {
        let r = recipe(Some("Tailler.\nCuire 10 min.\nServir."));
        let id = r.id;
        let html = page("", &r, false);
        let at = |s: &str| html.find(s).unwrap_or_else(|| panic!("{s} in {html}"));
        // Without JavaScript: the ingredients and every step, numbered.
        assert!(at(&format!(r#"<div data-cook="{id}"><div data-cook-plain>"#)) > 0);
        assert!(html.contains("<h1>Cuisiner : Ratatouille</h1>"), "{html}");
        assert!(html.contains("Temps : 1 h 30. Pour 6 personnes."), "{html}");
        assert!(
            html.contains(r#"<h2 id="ingredients">Ingrédients</h2><ul class="nojs-list"><li><strong>Aubergines</strong> — 2</li></ul>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<ol class="nojs-list"><li>Tailler.</li><li>Cuire 10 min.</li><li>Servir.</li></ol>"#),
            "{html}"
        );
        // The stepper waits, hidden, for the script: three steps, the first
        // shown, the ingredients in their dialog, the way back to the recipe.
        assert!(at("<div data-cook-ui hidden>") > at("data-cook-plain"));
        assert!(html.contains("Étape 1 sur 3</p>"), "{html}");
        assert!(html.contains(r#"<progress max="3" value="1""#), "{html}");
        assert_eq!(html.matches("data-step").count(), 3, "{html}");
        assert_eq!(html.matches("data-step hidden").count(), 2, "{html}");
        assert!(html.contains(r#"data-cook-start="600""#), "{html}");
        assert!(
            html.contains(r#"data-dialog="cook-ingredients" aria-haspopup="dialog""#),
            "{html}"
        );
        assert!(html.contains(r#"<dialog id="cook-ingredients""#), "{html}");
        assert!(
            html.contains(&format!(
                r#"<a class="btn" href="/recipes/{id}" data-cook-end hidden>"#
            )),
            "{html}"
        );
        assert!(html.contains("Quitter le mode cuisine</a>"), "{html}");
        assert!(html.contains("<title>Cuisiner : Ratatouille"), "{html}");
    }

    #[test]
    fn a_recipe_without_steps_says_so_and_offers_its_edit_to_whoever_may() {
        let r = recipe(Some(" \n "));
        let id = r.id;
        let html = page("", &r, true);
        assert!(html.contains("Pas d'étapes pour cette recette"), "{html}");
        assert!(!html.contains("data-cook"), "{html}");
        assert!(
            html.contains(&format!(
                r#"<a class="btn" href="/recipes/{id}/edit">Modifier la recette</a>"#
            )),
            "{html}"
        );
        let html = page("", &recipe(None), false);
        assert!(html.contains("Pas d'étapes pour cette recette"), "{html}");
        assert!(!html.contains("/edit"), "{html}");
        assert!(html.contains(&format!(r#"href="/recipes/{id}">Retour à la recette</a>"#)));
    }
}
