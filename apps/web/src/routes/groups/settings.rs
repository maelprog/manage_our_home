//! `/groups/:id/settings` — rename, colour (#538), calendar (#565), leave,
//! transfer ownership, delete.
//! Permission bar per `apps/api/src/groups/mod.rs`: rename, colour and
//! calendar are owner/admin (403 otherwise); transfer and delete are owner-only;
//! anyone can leave, but an owner leaving must name a successor (422
//! `new_owner_id_required`) and the last remaining member must delete the
//! group instead (409 `last_member_must_delete_group`). Transfer also
//! 404s on a non-member target and 422s on `cannot_transfer_to_self`
//! (both unreachable through this UI's member-only select, still mapped).
//!
//! Since #530 the page also lists the members with their roles and, for
//! owners and admins, the invitation by e-mail and a « Gérer » per member
//! they may act on — the controls themselves stay on `/groups/:id/members`.
//! Since #867 the owner alone also sets, in « Accès », from which role the
//! members reach each restricted feature (`PUT /groups/:id/access-policies`,
//! owner-only: 403 otherwise).

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::{
    AccessPolicies, AccessPolicy, GroupDetailResponse, GroupMember, GroupSummary,
    RenameGroupRequest,
};
use manage_our_home_shared::validation::dietary::{validate_equipment, EQUIPMENT};
use manage_our_home_shared::validation::groups::{
    actor_can_act_on, can_manage_group, validate_group_name, Accent, Feature, SchoolZone,
};
use uuid::Uuid;

use crate::app::{
    html_escape, member_link, shell_with_header, submit_fallback, Icon, Width, APPLIES_ON_CHANGE,
};
use crate::family::{accent_label, accent_of};
use crate::icons::{icon, Icon as Glyph};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::members::{fetch_group_detail, group_not_found_page, service_unavailable_page};
use super::{cookie_of, header_with_groups, role_badge, role_label};

fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "renamed" => Some("Groupe renommé."),
        "ownership_transferred" => Some("Propriété transférée."),
        "accent_changed" => Some("Couleur de la famille enregistrée."),
        "calendar_changed" => Some("Calendrier de la famille enregistré."),
        "equipment_saved" => Some("Équipements enregistrés."),
        _ => None,
    }
}

fn error_text(code: &str) -> Option<&'static str> {
    match code {
        "forbidden" => Some("Vous n'avez pas les droits nécessaires pour cette action."),
        "name_required" => Some("Le nom du groupe ne peut pas être vide."),
        "invalid_accent" => Some("Cette couleur n'est pas proposée."),
        "invalid_school_zone" => Some("Cette zone n'est pas proposée."),
        "unknown_equipment" => Some("Un équipement coché n'est pas proposé. Rechargez la page."),
        "last_member" => Some(
            "Vous êtes le dernier membre : quitter n'est pas possible, supprimez le groupe à la place.",
        ),
        "new_owner_required" => {
            Some("En tant que propriétaire, vous devez désigner un successeur pour quitter le groupe.")
        }
        "target_not_member" => Some("Ce membre ne fait pas (ou plus) partie du groupe."),
        "cannot_transfer_to_self" => Some("Vous êtes déjà propriétaire de ce groupe."),
        "unavailable" => Some("Service momentanément indisponible, merci de réessayer."),
        _ => None,
    }
}

/// One member of the « Membres » card: their icon and name as a link to
/// their profile (#466), their e-mail, their role in a pastille, and
/// « Gérer » when the caller may act on them (`actor_can_act_on`, the API's
/// own bar). « Gérer » leads to the member's row on the members page, where
/// the role select and « Retirer » are.
fn member_row(group_id: Uuid, my_role: &str, m: &GroupMember) -> String {
    let manage = if actor_can_act_on(my_role, &m.role) {
        format!(
            r#"<a class="btn secondary" href="/groups/{group_id}/members#member-{id}">Gérer</a>"#,
            id = m.user_id,
        )
    } else {
        String::new()
    };
    format!(
        r#"<li class="list-row"><span>{link} <span class="muted">{email}</span></span><span class="actions">{badge}{manage}</span></li>"#,
        link = member_link(
            group_id,
            m.user_id,
            &m.display_name,
            Icon::avatar(m.has_avatar)
        ),
        email = html_escape(&m.email),
        badge = role_badge(&m.role),
    )
}

/// « Membres »: every member, then — for owners and admins only — the
/// invitation by e-mail. It posts to the members page's route, which sends
/// the e-mail and shows the link once (`members::invite`).
fn members_card(group: &GroupDetailResponse, my_role: &str) -> String {
    let rows: String = group
        .members
        .iter()
        .map(|m| member_row(group.id, my_role, m))
        .collect();
    let invite = if can_manage_group(my_role) {
        format!(
            r#"<form method="post" action="/groups/{id}/members/invite" class="quick-add">
<label>E-mail à inviter<input type="email" name="invited_email" required></label>
<button type="submit">{icon} Inviter</button>
</form>
<p class="muted">L'invitation est valable 7 jours. Pour un lien à partager sans e-mail : <a href="/groups/{id}/members">membres du groupe</a>.</p>"#,
            id = group.id,
            icon = icon(Glyph::Members),
        )
    } else {
        String::new()
    };
    format!(
        r#"<section class="card">
<h2>Membres</h2>
<ul class="list">{rows}</ul>
{invite}
</section>"#
    )
}

/// The other members, as the options of a successor `<select>`.
fn successor_options(others: &[&GroupMember]) -> String {
    others
        .iter()
        .map(|m| {
            format!(
                r#"<option value="{id}">{name} — {role}</option>"#,
                id = m.user_id,
                name = html_escape(&m.display_name),
                role = role_label(&m.role),
            )
        })
        .collect()
}

/// « Nom et zone sensible »: rename (owners and admins), transfer (the
/// owner, when someone can take it), leave (everyone; the owner names a
/// successor), delete (the owner). The labels are the ones the e2e suite
/// finds the controls by (DESIGN.md, contrainte 5).
fn name_card(group: &GroupDetailResponse, my_role: &str, others: &[&GroupMember]) -> String {
    let id = group.id;
    let is_owner = my_role == "owner";
    let options = successor_options(others);

    let rename = if can_manage_group(my_role) {
        format!(
            r#"<form method="post" action="/groups/{id}/settings/rename">
<label>Nom du groupe<input type="text" name="name" required value="{name}"></label>
<button type="submit">Renommer</button>
</form>"#,
            name = html_escape(&group.name),
        )
    } else {
        String::new()
    };

    let transfer = if is_owner && !others.is_empty() {
        format!(
            r#"<h3>Transférer la propriété</h3>
<p class="muted">Le nouveau propriétaire prend la main sur le groupe ; vous devenez admin.</p>
<form method="post" action="/groups/{id}/settings/transfer">
<label>Nouveau propriétaire<select name="new_owner_id" required>{options}</select></label>
<button type="submit" class="secondary">Transférer</button>
</form>"#
        )
    } else {
        String::new()
    };

    let leave = if is_owner && !others.is_empty() {
        format!(
            r#"<h3>Quitter le groupe</h3>
<p class="muted">En tant que propriétaire, vous devez d'abord désigner un successeur.</p>
<form method="post" action="/groups/{id}/settings/leave">
<label>Successeur<select name="new_owner_id" required>{options}</select></label>
<button type="submit" class="secondary danger">Quitter le groupe</button>
</form>"#
        )
    } else {
        format!(
            r#"<h3>Quitter le groupe</h3>
<form method="post" action="/groups/{id}/settings/leave">
<button type="submit" class="secondary danger">Quitter le groupe</button>
</form>"#
        )
    };

    let delete = if is_owner {
        format!(
            r#"<h3>Supprimer le groupe</h3>
<p class="muted">Supprime définitivement le groupe, ses membres et ses invitations.</p>
<form method="post" action="/groups/{id}/settings/delete">
<button type="submit" class="secondary danger">Supprimer le groupe</button>
</form>"#
        )
    } else {
        String::new()
    };

    format!(
        r#"<section class="card">
<h2>Nom et zone sensible</h2>
{rename}
{transfer}
{leave}
{delete}
</section>"#
    )
}

/// « Couleur de la famille » (#538): the eight accents as native radios
/// of a form, for owners and admins — the bar of `PATCH /groups/:id`
/// (`can_manage_group`) — or the colour and who may change it, for a
/// standard member. A pill sends the form by itself (#844), the button
/// being the fallback without JavaScript (`app::submit_fallback`). Each
/// pill carries its own `data-accent`, so its swatch,
/// its radio and its tint when checked are in that colour; the preview
/// shows the family's colour on what it paints: a tint, a button, a link.
fn colour_card(group_id: Uuid, accent: Accent, my_role: &str) -> String {
    let current = accent.as_str();
    let preview = format!(
        r#"<p class="muted">Aperçu</p>
<div class="preview" data-accent="{current}" aria-hidden="true"><span class="badge accent">{label}</span><span class="btn">Ajouter</span><span class="more">Voir tout</span></div>"#,
        label = accent_label(accent),
    );
    let body = if can_manage_group(my_role) {
        let picks: String = Accent::ALL
            .into_iter()
            .map(|a| {
                format!(
                    r#"<label class="field inline pick" data-accent="{id}"><input type="radio" name="accent" value="{id}"{checked} data-submit-on-change><span class="swatch" aria-hidden="true"></span>{label}</label>"#,
                    id = a.as_str(),
                    checked = if a == accent { " checked" } else { "" },
                    label = accent_label(a),
                )
            })
            .collect();
        format!(
            r#"<form method="post" action="/groups/{group_id}/settings/accent" data-keep-scroll>
{APPLIES_ON_CHANGE}
<div class="picks" role="radiogroup" aria-label="Couleur de la famille">{picks}</div>
{preview}
{fallback}
</form>"#,
            fallback = submit_fallback("Enregistrer la couleur"),
        )
    } else {
        format!(
            r#"<p class="field inline" data-accent="{current}"><span class="swatch" aria-hidden="true"></span>{label}</p>
{preview}
<p class="muted">Réservé aux propriétaires et admins.</p>"#,
            label = accent_label(accent),
        )
    };
    format!(
        r#"<section class="card" id="couleur">
<h2>Couleur de la famille</h2>
{body}
</section>"#
    )
}

/// « Calendrier » (#565): the family's school zone — A, B, C or none — and
/// Alsace-Moselle, which the agenda's national days follow (#561, #562,
/// #563). A form of native radios and a checkbox for owners and admins,
/// the bar of `PATCH /groups/:id`; the settings in words for the others.
/// A zone or the box sends the whole form by itself (#844), so both
/// settings always leave together; the button is the fallback without
/// JavaScript.
fn calendar_card(group: &GroupDetailResponse, my_role: &str) -> String {
    let zone = group.school_zone.as_deref().and_then(SchoolZone::parse);
    let checked = |on: bool| if on { " checked" } else { "" };
    let body = if can_manage_group(my_role) {
        let picks: String = SchoolZone::ALL
            .into_iter()
            .map(|z| (z.as_str(), format!("Zone {}", z.as_str()), zone == Some(z)))
            .chain([("", "Aucune".to_string(), zone.is_none())])
            .map(|(value, label, on)| {
                format!(
                    r#"<label class="field inline pick"><input type="radio" name="school_zone" value="{value}"{on} data-submit-on-change>{label}</label>"#,
                    on = checked(on),
                )
            })
            .collect();
        format!(
            r#"<form method="post" action="/groups/{id}/settings/calendar" data-keep-scroll>
{APPLIES_ON_CHANGE}
<div class="picks" role="radiogroup" aria-label="Zone de vacances scolaires">{picks}</div>
<label class="field inline"><input type="checkbox" name="alsace_moselle" value="on"{am} data-submit-on-change>Alsace-Moselle</label>
<p class="muted">Ajoute le Vendredi saint et la Saint-Étienne aux jours fériés de l'agenda.</p>
{fallback}
</form>"#,
            id = group.id,
            am = checked(group.alsace_moselle),
            fallback = submit_fallback("Enregistrer le calendrier"),
        )
    } else {
        let zone = zone.map_or("Aucune zone".to_string(), |z| {
            format!("Zone {}", z.as_str())
        });
        let am = if group.alsace_moselle {
            "Jours fériés d'Alsace-Moselle"
        } else {
            "Sans les jours fériés d'Alsace-Moselle"
        };
        format!(
            r#"<p>{zone} · {am}</p>
<p class="muted">Réservé aux propriétaires et admins.</p>"#
        )
    };
    format!(
        r#"<section class="card" id="calendrier">
<h2>Calendrier</h2>
<p class="muted">Les vacances scolaires de la zone et les jours fériés s'affichent dans l'agenda.</p>
{body}
</section>"#
    )
}

/// « Équipements de la cuisine » (#819): what the household has, which a
/// recipe may require. A form of boxes for owners and admins — the bar of
/// `PUT /groups/:id/equipment`, arbitrated 2026-11-16 like the colour and
/// the calendar —, the list in pills for the others. `household` is `None`
/// when it could not be read: no form then, which would save an empty list.
fn equipment_card(group_id: Uuid, household: Option<&[String]>, my_role: &str) -> String {
    let body = match household {
        None => r#"<p class="notice error">Impossible de lire les équipements de la famille. Merci de recharger la page.</p>"#.to_string(),
        Some(household) if can_manage_group(my_role) => {
            let boxes: String = EQUIPMENT
                .iter()
                .map(|(code, label)| {
                    let checked = if household.iter().any(|c| c == code) {
                        " checked"
                    } else {
                        ""
                    };
                    format!(
                        r#"<label class="field inline pick"><input type="checkbox" name="equipment" value="{code}"{checked}>{label}</label>"#
                    )
                })
                .collect();
            format!(
                r#"<form method="post" action="/groups/{group_id}/settings/equipment">
<fieldset class="plain">
<legend class="sr-only">Équipements disponibles</legend>
<div class="picks">{boxes}</div>
</fieldset>
<button type="submit">Enregistrer les équipements</button>
</form>"#
            )
        }
        Some(household) => {
            let list = if household.is_empty() {
                r#"<p>Aucun équipement déclaré.</p>"#.to_string()
            } else {
                crate::routes::recipes::equipment::badges(household, None)
            };
            format!(
                r#"{list}
<p class="muted">Seuls le propriétaire et les admins modifient cette liste.</p>"#
            )
        }
    };
    format!(
        r#"<section class="card" id="equipements">
<h2>Équipements de la cuisine</h2>
<p class="muted">Ce dont dispose la famille. Une recette peut en exiger : sur sa fiche, un équipement que la famille n'a pas est signalé, et elle n'est pas proposée dans les suggestions.</p>
{body}
</section>"#
    )
}

/// What « Accès » (#867) shows: the family as `GET /groups` lists it, whose
/// `access_policies` carry the thresholds (`None` when it could not be
/// read), and the banner of the last save, said in the card rather than at
/// the top of the page, since the page comes back at the card.
#[derive(Default)]
struct Access<'a> {
    group: Option<&'a GroupSummary>,
    notice: Option<&'static str>,
    error: Option<&'static str>,
}

fn access_notice_text(code: &str) -> Option<&'static str> {
    match code {
        "access_saved" => {
            Some("Accès enregistré. Les membres voient le changement à leur prochaine page.")
        }
        _ => None,
    }
}

fn access_error_text(code: &str) -> Option<&'static str> {
    match code {
        "access_failed" => Some(
            "Les accès n'ont pas été enregistrés. Vos choix précédents sont conservés ; réessayez dans un instant.",
        ),
        "invalid_access" => Some("Un choix d'accès n'est pas proposé. Rechargez la page."),
        _ => None,
    }
}

/// A feature's legend, what it covers, and — for the two the menu shows —
/// where the change shows (the validated mock-up's words).
fn feature_text(feature: Feature) -> (&'static str, &'static str, Option<&'static str>) {
    match feature {
        Feature::Budget => (
            "Budget",
            "Voir le budget, ses plafonds et ses catégories.",
            Some("Le lien Budget du menu apparaît pour les membres concernés."),
        ),
        Feature::GoogleCalendar => (
            "Google Agenda",
            "Connecter et gérer les agendas Google de la famille.",
            Some("Le lien « Gérer les agendas Google » apparaît pour les membres concernés."),
        ),
        Feature::StocksEdit => (
            "Stocks",
            "Modifier ou supprimer ce que d’autres ont ajouté au stock.",
            None,
        ),
        Feature::RecipesEdit => (
            "Recettes",
            "Modifier ou supprimer les recettes ajoutées par d’autres.",
            None,
        ),
        Feature::GroceryEdit => (
            "Courses",
            "Modifier ou supprimer ce que d’autres ont ajouté à la liste de courses.",
            None,
        ),
        Feature::AgendaEdit => (
            "Agenda",
            "Modifier ou supprimer les événements créés par d’autres.",
            None,
        ),
        Feature::MessagerieModerate => (
            "Messagerie",
            "Supprimer les messages des autres membres.",
            None,
        ),
    }
}

/// The three thresholds, lowest first, as `group_role` values and in words.
const THRESHOLDS: [(&str, &str); 3] = [
    ("standard", "Tous les membres"),
    ("admin", "Admins et propriétaire"),
    ("owner", "Propriétaire seul"),
];

/// « Accès » (#867, maquette validée le 2026-11-19): for the owner alone —
/// the bar of `PUT /groups/:id/access-policies` —, one fieldset per
/// restricted feature, its three thresholds as native radios of one form.
/// A radio sends the whole form by itself (#844), the seven thresholds
/// together, and the form is locked while it is sent
/// (`data-lock-on-change`), its fieldset saying so in its `data-busy-status`
/// line; the button is the fallback without JavaScript, `submit_fallback`'s
/// markup with a name (`SENT_BY_BUTTON`) so that its post is told from a
/// radio's (`access_redirect`). No form when the
/// thresholds could not be read: it would send seven defaults.
fn access_card(group_id: Uuid, my_role: &str, access: &Access) -> String {
    if my_role != "owner" {
        return String::new();
    }
    let notice = access
        .notice
        .map(|n| format!(r#"<p class="notice success" role="status">{n}</p>"#))
        .unwrap_or_default();
    let error = access
        .error
        .map(|e| format!(r#"<p class="notice error" role="alert">{e}</p>"#))
        .unwrap_or_default();
    let body = match access.group {
        None => r#"<p class="notice error">Impossible de lire les accès de la famille. Merci de recharger la page.</p>"#.to_string(),
        Some(group) => {
            let fieldsets: String = Feature::ALL
                .into_iter()
                .map(|feature| {
                    let name = feature.as_str();
                    let (legend, covers, effect) = feature_text(feature);
                    let current = group.min_role(feature);
                    let picks: String = THRESHOLDS
                        .iter()
                        .map(|(value, label)| {
                            let on = if *value == current { " checked" } else { "" };
                            format!(
                                r#"<label class="field inline pick"><input type="radio" name="{name}" value="{value}"{on} data-submit-on-change>{label}</label>"#
                            )
                        })
                        .collect();
                    let (effect, described) = match effect {
                        Some(e) => (
                            format!("\n<p class=\"hint\" id=\"acces-{name}-effet\">{e}</p>"),
                            format!("acces-{name} acces-{name}-effet"),
                        ),
                        None => (String::new(), format!("acces-{name}")),
                    };
                    format!(
                        r#"<fieldset aria-describedby="{described}">
<legend>{legend}</legend>
<p class="muted" id="acces-{name}">{covers}</p>{effect}
<div class="picks">{picks}</div>
<p class="status" role="status" data-busy-status></p>
</fieldset>
"#
                    )
                })
                .collect();
            format!(
                r#"<form method="post" action="/groups/{group_id}/settings/access" data-keep-scroll data-lock-on-change>
{APPLIES_ON_CHANGE}
{fieldsets}<p class="muted" data-hidden-with-js>Sans JavaScript, le choix s'envoie avec le bouton.</p>
<button type="submit" name="{sent}" value="{by}" data-hidden-with-js>Enregistrer l'accès</button>
</form>"#,
                sent = SENT_BY_BUTTON.0,
                by = SENT_BY_BUTTON.1,
            )
        }
    };
    format!(
        r#"<section class="card access" id="acces" aria-labelledby="acces-titre">
<h2 id="acces-titre">Accès</h2>
<p class="muted">Choisissez qui peut utiliser chaque fonction. Le propriétaire garde toujours la main.</p>
{notice}{error}
{body}
</section>"#
    )
}

/// The body of `PUT /groups/:id/access-policies` for what the form posted:
/// each feature's threshold, in the order posted. A feature or a threshold
/// off the lists, a feature twice, or nothing at all is refused here,
/// before apps/api, which would refuse it too (400).
/// The name and value « Enregistrer l'accès » posts, which a radio's post
/// (`form.submit()`, no submitter) does not carry.
const SENT_BY_BUTTON: (&str, &str) = ("sent", "button");

/// The query, and the anchor, the page comes back with after a post of
/// « Accès ». A radio's post goes back to the page as it was: `enhance.js`
/// puts the reader and the focus back on the radio (`data-keep-scroll`),
/// which an anchor would take away from it. The button's post, without
/// JavaScript, and any failure — which `data-keep-scroll` leaves at the
/// top — go to the card, whose banner says how it went.
fn access_redirect(pairs: &[(String, String)], query: &str) -> String {
    let by_button = pairs
        .iter()
        .any(|(k, v)| (k.as_str(), v.as_str()) == SENT_BY_BUTTON);
    if by_button || query.starts_with("error=") {
        format!("?{query}#acces")
    } else {
        format!("?{query}")
    }
}

fn access_request(pairs: &[(String, String)]) -> Result<AccessPolicies, &'static str> {
    let mut policies: Vec<AccessPolicy> = Vec::with_capacity(pairs.len());
    for (key, value) in pairs {
        if key == SENT_BY_BUTTON.0 {
            continue;
        }
        let feature = Feature::parse(key).ok_or("invalid_access")?;
        if !THRESHOLDS.iter().any(|(t, _)| t == value) {
            return Err("invalid_access");
        }
        if policies.iter().any(|p| p.feature == feature.as_str()) {
            return Err("invalid_access");
        }
        policies.push(AccessPolicy {
            feature: feature.as_str().to_string(),
            min_role: value.clone(),
        });
    }
    if policies.is_empty() {
        return Err("invalid_access");
    }
    Ok(AccessPolicies { policies })
}

#[derive(serde::Deserialize)]
pub struct CalendarForm {
    /// `""` is « Aucune »; absent, the zone is kept.
    #[serde(default)]
    school_zone: Option<String>,
    /// A checkbox: sent when ticked, absent otherwise.
    #[serde(default)]
    alsace_moselle: Option<String>,
}

/// The body of `PATCH /groups/:id` for what the calendar form posted: the
/// two calendar settings, nothing else. A zone outside A, B, C is refused
/// here, before apps/api, which would refuse it too.
fn calendar_request(form: &CalendarForm) -> Result<RenameGroupRequest, &'static str> {
    let school_zone = match form.school_zone.as_deref() {
        None => None,
        Some("") => Some(None),
        Some(value) => match SchoolZone::parse(value) {
            Some(zone) => Some(Some(zone.as_str().to_string())),
            None => return Err("invalid_school_zone"),
        },
    };
    Ok(RenameGroupRequest {
        school_zone,
        alsace_moselle: Some(form.alsace_moselle.is_some()),
        ..Default::default()
    })
}

/// The family settings (#530, maquettes « Mobile — Famille » et « PC —
/// Famille »): the family's name under « Paramètres de la famille » and
/// your role, then the members and their roles with the invitation, then
/// the name and the sensitive zone; beside them on a PC, under them on a
/// phone, the column of « Couleur de la famille » (#538), « Calendrier »,
/// « Équipements de la cuisine », « Accès » for the owner (#867), then the
/// Google calendars. `accent` is this family's, which may not be the active
/// one's.
#[allow(clippy::too_many_arguments)]
fn page(
    header: &str,
    me: &MeResponse,
    group: &GroupDetailResponse,
    accent: Accent,
    equipment: Option<&[String]>,
    access: Access,
    notice: Option<&str>,
    error: Option<&str>,
) -> String {
    let my_role = group
        .members
        .iter()
        .find(|m| m.user_id == me.user_id)
        .map(|m| m.role.clone())
        .unwrap_or_default();
    let others = group
        .members
        .iter()
        .filter(|m| m.user_id != me.user_id)
        .collect::<Vec<_>>();

    let notice = notice
        .map(|n| format!(r#"<p class="notice success" role="status">{n}</p>"#))
        .unwrap_or_default();
    let error = error
        .map(|e| format!(r#"<p class="notice error">{e}</p>"#))
        .unwrap_or_default();

    // Cross-link only, no controls: the Google Calendar screens (front epic
    // F11) live under `/agenda/imports` because what they produce is agenda
    // data, but "connecter un agenda" is the kind of thing people come
    // looking for in the family settings, so the door is signposted here too.
    let body = format!(
        r#"<hgroup>
<p class="eyebrow">Paramètres de la famille</p>
<h1>{name}</h1>
<p class="muted">Votre rôle : {role}</p>
</hgroup>
{notice}{error}
<div class="settings">
<div>
{members}
{name_card}
</div>
<div>
{colour}
{calendar}
{equipment}
{access}
<section class="card">
<h2>Agendas Google</h2>
<p class="muted">Les agendas Google connectés à cette famille se gèrent depuis l'agenda : leurs événements y sont recopiés à la demande.</p>
<p><a href="/agenda/imports">Gérer les agendas Google</a></p>
</section>
</div>
</div>
<div class="links">
<a href="/groups">Retour à mes groupes</a>
<a href="/groups/{id}/members">Membres du groupe</a>
</div>"#,
        name = html_escape(&group.name),
        role = role_badge(&my_role),
        members = members_card(group, &my_role),
        name_card = name_card(group, &my_role, &others),
        colour = colour_card(group.id, accent, &my_role),
        calendar = calendar_card(group, &my_role),
        equipment = equipment_card(group.id, equipment, &my_role),
        access = access_card(group.id, &my_role, &access),
        id = group.id,
    );
    shell_with_header(
        Width::Full,
        &format!("Paramètres — {}", group.name),
        header,
        &body,
    )
}

#[derive(serde::Deserialize)]
pub struct SettingsQuery {
    notice: Option<String>,
    error: Option<String>,
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Query(query): Query<SettingsQuery>,
) -> Response {
    let cookie = cookie_of(&headers);
    let group = match fetch_group_detail(&state, cookie.as_deref(), group_id).await {
        Ok(Some(group)) => group,
        Ok(None) => return group_not_found_page().into_response(),
        Err(_) => return service_unavailable_page().into_response(),
    };
    let redirect_to = format!("/groups/{group_id}/settings");
    let (groups, header) = header_with_groups(&state, &headers, &me, &redirect_to).await;
    // `GET /groups/:id` does not carry the accent (#537); the caller's list
    // does, for every family they belong to.
    let accent = groups
        .iter()
        .find(|g| g.group_id == group_id)
        .map(accent_of)
        .unwrap_or_default();
    let equipment =
        crate::routes::recipes::equipment::fetch_household(&state, group_id, cookie.as_deref())
            .await;
    let notice = query.notice.as_deref().and_then(notice_text);
    let error = query.error.as_deref().and_then(error_text);
    // The thresholds come with the family in the caller's list (#865):
    // `GET /groups/:id` does not carry them either.
    let access = Access {
        group: groups.iter().find(|g| g.group_id == group_id),
        notice: query.notice.as_deref().and_then(access_notice_text),
        error: query.error.as_deref().and_then(access_error_text),
    };
    Html(page(
        &header,
        &me,
        &group,
        accent,
        equipment.as_deref(),
        access,
        notice,
        error,
    ))
    .into_response()
}

/// « Enregistrer l'accès » (#867): `PUT /groups/:id/access-policies` with
/// the seven thresholds the form holds. Where the page comes back:
/// `access_redirect`.
pub async fn access(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let settings_url = format!("/groups/{group_id}/settings");
    let back = |query: &str| format!("{settings_url}{}", access_redirect(&pairs, query));
    let body = match access_request(&pairs) {
        Ok(body) => body,
        Err(code) => return Redirect::to(&back(&format!("error={code}"))).into_response(),
    };

    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PUT,
        &format!("/groups/{group_id}/access-policies"),
        cookie.as_deref(),
        Some(serde_json::json!(body)),
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => back("notice=access_saved"),
        // The page's own banner, at the top: no card for a non-owner.
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{settings_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => back("error=invalid_access"),
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return group_not_found_page().into_response()
        }
        Ok(_) | Err(_) => back("error=access_failed"),
    };
    Redirect::to(&target).into_response()
}

/// « Enregistrer les équipements » (#819): `PUT /groups/:id/equipment`
/// with the boxes ticked, none ticked being an empty list.
pub async fn equipment(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let settings_url = format!("/groups/{group_id}/settings");
    let ticked: Vec<String> = pairs
        .into_iter()
        .filter(|(key, _)| key == "equipment")
        .map(|(_, code)| code)
        .collect();
    let Ok(codes) = validate_equipment(&ticked) else {
        return Redirect::to(&format!(
            "{settings_url}?error=unknown_equipment#equipements"
        ))
        .into_response();
    };

    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PUT,
        &format!("/groups/{group_id}/equipment"),
        cookie.as_deref(),
        Some(serde_json::json!({ "codes": codes })),
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            format!("{settings_url}?notice=equipment_saved#equipements")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{settings_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            format!("{settings_url}?error=unknown_equipment#equipements")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return group_not_found_page().into_response()
        }
        Ok(_) | Err(_) => format!("{settings_url}?error=unavailable#equipements"),
    };
    Redirect::to(&target).into_response()
}

#[derive(serde::Deserialize)]
pub struct RenameForm {
    name: String,
}

pub async fn rename(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Form(form): Form<RenameForm>,
) -> Response {
    let settings_url = format!("/groups/{group_id}/settings");
    if validate_group_name(&form.name).is_err() {
        return Redirect::to(&format!("{settings_url}?error=name_required")).into_response();
    }

    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{group_id}"),
        cookie.as_deref(),
        Some(serde_json::json!(RenameGroupRequest {
            name: Some(form.name.trim().to_string()),
            ..Default::default()
        })),
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            format!("{settings_url}?notice=renamed")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{settings_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            format!("{settings_url}?error=name_required")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return group_not_found_page().into_response()
        }
        Ok(_) | Err(_) => format!("{settings_url}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

#[derive(serde::Deserialize)]
pub struct AccentForm {
    #[serde(default)]
    accent: String,
}

/// « Enregistrer la couleur » (#538): `PATCH /groups/:id` with the accent
/// alone, the name kept. A value outside the eight is refused here, before
/// apps/api, which would refuse it too (400 `invalid_accent`).
pub async fn accent(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Form(form): Form<AccentForm>,
) -> Response {
    let settings_url = format!("/groups/{group_id}/settings");
    let Some(accent) = Accent::parse(&form.accent) else {
        return Redirect::to(&format!("{settings_url}?error=invalid_accent")).into_response();
    };

    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{group_id}"),
        cookie.as_deref(),
        Some(serde_json::json!(RenameGroupRequest {
            accent: Some(accent.as_str().to_string()),
            ..Default::default()
        })),
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            format!("{settings_url}?notice=accent_changed")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{settings_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            format!("{settings_url}?error=invalid_accent")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return group_not_found_page().into_response()
        }
        Ok(_) | Err(_) => format!("{settings_url}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

/// « Enregistrer le calendrier » (#565): `PATCH /groups/:id` with the
/// school zone and Alsace-Moselle, the name and the colour kept.
pub async fn calendar(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Form(form): Form<CalendarForm>,
) -> Response {
    let settings_url = format!("/groups/{group_id}/settings");
    let body = match calendar_request(&form) {
        Ok(body) => body,
        Err(code) => return Redirect::to(&format!("{settings_url}?error={code}")).into_response(),
    };

    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{group_id}"),
        cookie.as_deref(),
        Some(serde_json::json!(body)),
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            format!("{settings_url}?notice=calendar_changed")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{settings_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            format!("{settings_url}?error=invalid_school_zone")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return group_not_found_page().into_response()
        }
        Ok(_) | Err(_) => format!("{settings_url}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

#[derive(serde::Deserialize)]
pub struct TransferForm {
    new_owner_id: Uuid,
}

pub async fn transfer(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Form(form): Form<TransferForm>,
) -> Response {
    let settings_url = format!("/groups/{group_id}/settings");
    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{group_id}/transfer-ownership"),
        cookie.as_deref(),
        Some(serde_json::json!({ "new_owner_id": form.new_owner_id })),
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            format!("{settings_url}?notice=ownership_transferred")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{settings_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            format!("{settings_url}?error=target_not_member")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            format!("{settings_url}?error=cannot_transfer_to_self")
        }
        Ok(_) | Err(_) => format!("{settings_url}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

#[derive(serde::Deserialize)]
pub struct LeaveForm {
    /// Empty string when the plain (non-owner) leave form posts — mapped
    /// to `None` before hitting the API.
    #[serde(default)]
    new_owner_id: String,
}

pub async fn leave(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Form(form): Form<LeaveForm>,
) -> Response {
    let settings_url = format!("/groups/{group_id}/settings");
    let new_owner_id = form.new_owner_id.trim().parse::<Uuid>().ok();

    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{group_id}/leave"),
        cookie.as_deref(),
        Some(serde_json::json!({ "new_owner_id": new_owner_id })),
    )
    .await;

    let target = match result {
        // The stale `active_group_id` cookie is fine: every page resolves
        // it against the fresh `GET /groups` list and falls back.
        Ok(resp) if resp.status == reqwest::StatusCode::OK => "/groups?notice=left".to_string(),
        Ok(resp) if resp.status == reqwest::StatusCode::CONFLICT => {
            format!("{settings_url}?error=last_member")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            format!("{settings_url}?error=new_owner_required")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{settings_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return group_not_found_page().into_response()
        }
        Ok(_) | Err(_) => format!("{settings_url}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

pub async fn delete(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
) -> Response {
    let settings_url = format!("/groups/{group_id}/settings");
    let cookie = cookie_of(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!("/groups/{group_id}"),
        cookie.as_deref(),
        None,
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            "/groups?notice=group_deleted".to_string()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{settings_url}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return group_not_found_page().into_response()
        }
        Ok(_) | Err(_) => format!("{settings_url}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: Uuid = Uuid::from_u128(1);
    const OWNER: Uuid = Uuid::from_u128(2);
    const ADMIN: Uuid = Uuid::from_u128(3);
    const STANDARD: Uuid = Uuid::from_u128(4);

    fn me(id: Uuid) -> MeResponse {
        MeResponse {
            user_id: id,
            email: "me@example.test".into(),
            display_name: "Me".into(),
            email_verified: true,
            is_superadmin: false,
            has_password: true,
            deletion_requested_at: None,
            terms_accepted_version: None,
        }
    }

    fn member(id: Uuid, name: &str, role: &str) -> GroupMember {
        GroupMember {
            user_id: id,
            role: role.to_string(),
            display_name: name.to_string(),
            email: format!("{}@example.test", name.to_lowercase()),
            has_avatar: false,
        }
    }

    fn family() -> GroupDetailResponse {
        GroupDetailResponse {
            id: G,
            name: "Famille Martin".to_string(),
            school_zone: None,
            alsace_moselle: false,
            members: vec![
                member(OWNER, "Alex", "owner"),
                member(ADMIN, "Sam", "admin"),
                member(STANDARD, "Chloé", "standard"),
            ],
        }
    }

    fn page_as(id: Uuid) -> String {
        page(
            "",
            &me(id),
            &family(),
            Accent::Sarcelle,
            Some(&[]),
            Access::default(),
            None,
            None,
        )
    }

    fn manage_link(user: Uuid) -> String {
        format!(r#"href="/groups/{G}/members#member-{user}">Gérer</a>"#)
    }

    /// The title is the family's name, under « Paramètres de la famille »,
    /// with the caller's role beside it.
    #[test]
    fn the_page_is_titled_by_the_family_and_says_your_role() {
        let html = page_as(ADMIN);
        assert!(
            html.contains(r#"<p class="eyebrow">Paramètres de la famille</p>"#),
            "{html}"
        );
        assert!(html.contains("<h1>Famille Martin</h1>"), "{html}");
        assert!(
            html.contains(r#"Votre rôle : <span class="badge accent">Admin</span>"#),
            "{html}"
        );
    }

    #[test]
    fn the_family_name_is_escaped() {
        let mut group = family();
        group.name = "<b>F</b>".to_string();
        let html = page(
            "",
            &me(OWNER),
            &group,
            Accent::Sarcelle,
            Some(&[]),
            Access::default(),
            None,
            None,
        );
        assert!(html.contains("&lt;b&gt;F&lt;/b&gt;"), "{html}");
        assert!(!html.contains("<b>F</b>"), "{html}");
    }

    /// Every member, linked to their profile, with their e-mail and their
    /// role in a pastille.
    #[test]
    fn every_member_is_listed_with_a_role_pastille() {
        let html = page_as(STANDARD);
        for (id, name, role) in [
            (OWNER, "Alex", "owner"),
            (ADMIN, "Sam", "admin"),
            (STANDARD, "Chloé", "standard"),
        ] {
            assert!(
                html.contains(&crate::app::member_link(
                    G,
                    id,
                    name,
                    crate::app::Icon::Initial
                )),
                "{name}: {html}"
            );
            assert!(
                html.contains(&format!("{}@example.test", name.to_lowercase())),
                "{name}: {html}"
            );
            assert!(
                html.contains(&super::super::role_badge(role)),
                "{role}: {html}"
            );
        }
    }

    /// The acceptance criterion of #530: a standard member sees neither
    /// « Gérer » nor the invitation.
    #[test]
    fn a_standard_member_sees_neither_manage_nor_the_invitation() {
        let html = page_as(STANDARD);
        assert!(!html.contains(">Gérer<"), "{html}");
        assert!(!html.contains("/members/invite"), "{html}");
        assert!(!html.contains("invited_email"), "{html}");
    }

    /// « Gérer » follows the API's bar (`actor_can_act_on`): the owner on
    /// everyone but themselves…
    #[test]
    fn the_owner_manages_everyone_but_themselves() {
        let html = page_as(OWNER);
        assert!(html.contains(&manage_link(ADMIN)), "{html}");
        assert!(html.contains(&manage_link(STANDARD)), "{html}");
        assert!(!html.contains(&manage_link(OWNER)), "{html}");
    }

    /// … an admin on the standard members only.
    #[test]
    fn an_admin_manages_the_standard_members_only() {
        let html = page_as(ADMIN);
        assert!(html.contains(&manage_link(STANDARD)), "{html}");
        assert!(!html.contains(&manage_link(OWNER)), "{html}");
        assert!(!html.contains(&manage_link(ADMIN)), "{html}");
    }

    /// Owners and admins invite by e-mail, from the members' card, through
    /// the members page's own route.
    #[test]
    fn owners_and_admins_invite_by_email() {
        for who in [OWNER, ADMIN] {
            let html = page_as(who);
            assert!(
                html.contains(&format!(r#"action="/groups/{G}/members/invite""#)),
                "{html}"
            );
            assert!(
                html.contains(r#"<input type="email" name="invited_email" required>"#),
                "{html}"
            );
            assert!(html.contains("</svg> Inviter</button>"), "{html}");
        }
    }

    /// The rest of the bar, unchanged by the redesign: rename for owners
    /// and admins, transfer and delete for the owner, leave for everyone —
    /// the owner naming a successor.
    #[test]
    fn the_controls_follow_the_permission_bar() {
        let rename = format!(r#"action="/groups/{G}/settings/rename""#);
        let transfer = format!(r#"action="/groups/{G}/settings/transfer""#);
        let delete = format!(r#"action="/groups/{G}/settings/delete""#);
        let leave = format!(r#"action="/groups/{G}/settings/leave""#);

        let owner = page_as(OWNER);
        for action in [&rename, &transfer, &delete, &leave] {
            assert!(owner.contains(action.as_str()), "{action}: {owner}");
        }
        assert!(
            owner.contains("vous devez d'abord désigner un successeur"),
            "{owner}"
        );
        assert_eq!(
            owner.matches(r#"name="new_owner_id""#).count(),
            2,
            "{owner}"
        );

        let admin = page_as(ADMIN);
        assert!(admin.contains(&rename), "{admin}");
        assert!(admin.contains(&leave), "{admin}");
        assert!(!admin.contains(&transfer), "{admin}");
        assert!(!admin.contains(&delete), "{admin}");

        let standard = page_as(STANDARD);
        assert!(standard.contains(&leave), "{standard}");
        for action in [&rename, &transfer, &delete] {
            assert!(!standard.contains(action.as_str()), "{action}: {standard}");
        }
        assert!(!standard.contains("new_owner_id"), "{standard}");
    }

    /// An owner alone in the family has no one to hand it to: no transfer,
    /// and leaving asks for no successor (apps/api answers 409, mapped).
    #[test]
    fn an_owner_alone_has_no_transfer_and_no_successor_to_name() {
        let group = GroupDetailResponse {
            id: G,
            name: "Solo".to_string(),
            school_zone: None,
            alsace_moselle: false,
            members: vec![member(OWNER, "Alex", "owner")],
        };
        let html = page(
            "",
            &me(OWNER),
            &group,
            Accent::Sarcelle,
            Some(&[]),
            Access::default(),
            None,
            None,
        );
        assert!(!html.contains("/settings/transfer"), "{html}");
        assert!(!html.contains("new_owner_id"), "{html}");
        assert!(html.contains("/settings/leave"), "{html}");
    }

    /// A successor is picked among the others, named with their role.
    #[test]
    fn a_successor_is_one_of_the_other_members() {
        let html = page_as(OWNER);
        assert!(
            html.contains(&format!(r#"<option value="{ADMIN}">Sam — Admin</option>"#)),
            "{html}"
        );
        assert!(
            !html.contains(&format!(r#"<option value="{OWNER}">"#)),
            "{html}"
        );
    }

    /// Another member's name and e-mail are theirs to choose: escaped in
    /// their row and in the owner's successor options alike.
    #[test]
    fn a_members_name_and_email_are_escaped() {
        let mut group = family();
        group.members[1].display_name = "<i>Sam</i>".to_string();
        group.members[1].email = "<s>sam@example.test".to_string();
        let html = page(
            "",
            &me(OWNER),
            &group,
            Accent::Sarcelle,
            Some(&[]),
            Access::default(),
            None,
            None,
        );
        assert!(
            html.contains(&format!(
                r#"<option value="{ADMIN}">&lt;i&gt;Sam&lt;/i&gt; — Admin</option>"#
            )),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="muted">&lt;s&gt;sam@example.test</span>"#),
            "{html}"
        );
        assert!(!html.contains("<i>Sam</i>"), "{html}");
        assert!(!html.contains("<s>sam@"), "{html}");
    }

    // -- « Couleur de la famille » (#538) ----------------------------------

    fn colour_card(html: &str) -> &str {
        let at = html
            .find(r#"<section class="card" id="couleur">"#)
            .expect("the colour card");
        &html[at..at + html[at..].find("</section>").unwrap()]
    }

    /// Owners and admins pick among the eight, as native radios of a form
    /// that posts to the page's own route; the family's colour is checked.
    #[test]
    fn owners_and_admins_pick_among_the_eight_accents() {
        for who in [OWNER, ADMIN] {
            let html = page(
                "",
                &me(who),
                &family(),
                Accent::Prune,
                Some(&[]),
                Access::default(),
                None,
                None,
            );
            let card = colour_card(&html);
            assert!(card.contains("<h2>Couleur de la famille</h2>"), "{card}");
            assert!(
                card.contains(&format!(
                    r#"<form method="post" action="/groups/{G}/settings/accent""#
                )),
                "{card}"
            );
            assert_eq!(card.matches(r#"type="radio" name="accent""#).count(), 8);
            for accent in Accent::ALL {
                let id = accent.as_str();
                let checked = if accent == Accent::Prune {
                    " checked"
                } else {
                    ""
                };
                assert!(
                    card.contains(&format!(
                        r#"<label class="field inline pick" data-accent="{id}"><input type="radio" name="accent" value="{id}"{checked} data-submit-on-change><span class="swatch" aria-hidden="true"></span>{}</label>"#,
                        crate::family::accent_label(accent)
                    )),
                    "{id}: {card}"
                );
            }
            assert_eq!(card.matches(" checked").count(), 1, "{card}");
            // #844: a pastille sends the form by itself; the button is
            // the fallback without JavaScript, which hides it.
            assert!(
                card.contains(&format!(
                    r#"<form method="post" action="/groups/{G}/settings/accent" data-keep-scroll>"#
                )),
                "{card}"
            );
            assert!(
                card.contains(r#"<p class="muted" data-shown-with-js hidden>Le choix s'applique dès que vous le touchez.</p>"#),
                "{card}"
            );
            assert!(
                card.contains(r#"<p class="muted" data-hidden-with-js>Sans JavaScript, le choix s'envoie avec le bouton.</p>
<button type="submit" data-hidden-with-js>Enregistrer la couleur</button>"#),
                "{card}"
            );
            assert!(!card.contains("Réservé aux"), "{card}");
        }
    }

    /// The preview is painted in the family's colour, whatever the active
    /// family's: `data-accent` on it re-points the accent tokens.
    #[test]
    fn the_preview_wears_the_familys_colour() {
        for who in [OWNER, STANDARD] {
            let html = page(
                "",
                &me(who),
                &family(),
                Accent::Miel,
                Some(&[]),
                Access::default(),
                None,
                None,
            );
            let card = colour_card(&html);
            assert!(
                card.contains(r#"<div class="preview" data-accent="miel" aria-hidden="true">"#),
                "{card}"
            );
        }
    }

    /// A standard member sees the colour and who may change it, and no
    /// control to change it with.
    #[test]
    fn a_standard_member_sees_the_colour_but_cannot_change_it() {
        let html = page(
            "",
            &me(STANDARD),
            &family(),
            Accent::Ocean,
            Some(&[]),
            Access::default(),
            None,
            None,
        );
        let card = colour_card(&html);
        assert!(card.contains("<h2>Couleur de la famille</h2>"), "{card}");
        assert!(card.contains("Océan"), "{card}");
        assert!(
            card.contains("Réservé aux propriétaires et admins."),
            "{card}"
        );
        assert!(!card.contains("<form"), "{card}");
        assert!(!card.contains(r#"name="accent""#), "{card}");
        assert!(!card.contains("data-shown-with-js"), "{card}");
        assert!(!html.contains("/settings/accent"), "{html}");
    }

    #[test]
    fn the_accent_notice_and_errors_are_in_french() {
        assert_eq!(
            notice_text("accent_changed"),
            Some("Couleur de la famille enregistrée.")
        );
        assert_eq!(
            error_text("invalid_accent"),
            Some("Cette couleur n'est pas proposée.")
        );
    }

    // -- « Calendrier » (#565) -------------------------------------------

    fn calendar_card(html: &str) -> &str {
        let at = html
            .find(r#"<section class="card" id="calendrier">"#)
            .expect("the calendar card");
        &html[at..at + html[at..].find("</section>").unwrap()]
    }

    fn family_with(zone: Option<&str>, alsace_moselle: bool) -> GroupDetailResponse {
        GroupDetailResponse {
            school_zone: zone.map(str::to_string),
            alsace_moselle,
            ..family()
        }
    }

    /// Owners and admins choose the zone among A, B, C or none, and tick
    /// Alsace-Moselle, in a form that posts without JavaScript too; the
    /// family's settings are the ones checked.
    #[test]
    fn owners_and_admins_set_the_zone_and_alsace_moselle() {
        for who in [OWNER, ADMIN] {
            let group = family_with(Some("B"), true);
            let html = page(
                "",
                &me(who),
                &group,
                Accent::Sarcelle,
                Some(&[]),
                Access::default(),
                None,
                None,
            );
            let card = calendar_card(&html);
            assert!(card.contains("<h2>Calendrier</h2>"), "{card}");
            assert!(
                card.contains(r#"role="radiogroup" aria-label="Zone de vacances scolaires""#),
                "{card}"
            );
            for (value, label) in [
                ("A", "Zone A"),
                ("B", "Zone B"),
                ("C", "Zone C"),
                ("", "Aucune"),
            ] {
                let checked = if value == "B" { " checked" } else { "" };
                assert!(
                    card.contains(&format!(
                        r#"<input type="radio" name="school_zone" value="{value}"{checked} data-submit-on-change>{label}</label>"#
                    )),
                    "{value}: {card}"
                );
            }
            assert!(
                card.contains(r#"<input type="checkbox" name="alsace_moselle" value="on" checked data-submit-on-change>Alsace-Moselle</label>"#),
                "{card}"
            );
            assert_eq!(card.matches(" checked").count(), 2, "{card}");
            // #844: a zone or the box sends the whole form — both
            // settings together; the button is the fallback without
            // JavaScript.
            assert!(
                card.contains(&format!(
                    r#"<form method="post" action="/groups/{G}/settings/calendar" data-keep-scroll>"#
                )),
                "{card}"
            );
            assert_eq!(card.matches(" data-submit-on-change").count(), 5, "{card}");
            assert!(
                card.contains(r#"<p class="muted" data-shown-with-js hidden>Le choix s'applique dès que vous le touchez.</p>"#),
                "{card}"
            );
            assert!(
                card.contains(r#"<p class="muted" data-hidden-with-js>Sans JavaScript, le choix s'envoie avec le bouton.</p>
<button type="submit" data-hidden-with-js>Enregistrer le calendrier</button>"#),
                "{card}"
            );
            assert!(!card.contains("<script"), "{card}");
        }
    }

    /// A family with no zone has « Aucune » checked, and Alsace-Moselle not.
    #[test]
    fn no_zone_is_aucune_checked() {
        let html = page(
            "",
            &me(OWNER),
            &family_with(None, false),
            Accent::Sarcelle,
            Some(&[]),
            Access::default(),
            None,
            None,
        );
        let card = calendar_card(&html);
        assert!(
            card.contains(
                r#"<input type="radio" name="school_zone" value="" checked data-submit-on-change>Aucune</label>"#
            ),
            "{card}"
        );
        assert!(
            card.contains(
                r#"<input type="checkbox" name="alsace_moselle" value="on" data-submit-on-change>Alsace-Moselle</label>"#
            ),
            "{card}"
        );
        assert_eq!(card.matches(" checked").count(), 1, "{card}");
    }

    /// A standard member reads the settings and cannot change them.
    #[test]
    fn a_standard_member_reads_the_calendar_settings_only() {
        let html = page(
            "",
            &me(STANDARD),
            &family_with(Some("C"), true),
            Accent::Sarcelle,
            Some(&[]),
            Access::default(),
            None,
            None,
        );
        let card = calendar_card(&html);
        assert!(card.contains("Zone C"), "{card}");
        assert!(card.contains("Alsace-Moselle"), "{card}");
        assert!(
            card.contains("Réservé aux propriétaires et admins."),
            "{card}"
        );
        assert!(!card.contains("<form"), "{card}");
        assert!(!card.contains("<input"), "{card}");
        assert!(!html.contains("/settings/calendar"), "{html}");
        let none = page(
            "",
            &me(STANDARD),
            &family_with(None, false),
            Accent::Sarcelle,
            Some(&[]),
            Access::default(),
            None,
            None,
        );
        assert!(calendar_card(&none).contains("Aucune zone"), "{none}");
    }

    /// What the form posts becomes the body of `PATCH /groups/:id`: both
    /// calendar settings, and nothing else — the name and the colour kept.
    #[test]
    fn the_calendar_form_sends_the_zone_and_alsace_moselle_only() {
        let body = |zone: Option<&str>, am: Option<&str>| {
            calendar_request(&CalendarForm {
                school_zone: zone.map(str::to_string),
                alsace_moselle: am.map(str::to_string),
            })
            .map(|r| serde_json::to_value(r).unwrap())
        };
        assert_eq!(
            body(Some("B"), Some("on")),
            Ok(serde_json::json!({"school_zone": "B", "alsace_moselle": true}))
        );
        // « Aucune » clears the zone; an unticked box is not sent by the
        // browser, and means no Alsace-Moselle.
        assert_eq!(
            body(Some(""), None),
            Ok(serde_json::json!({"school_zone": null, "alsace_moselle": false}))
        );
        // No zone field at all keeps the zone.
        assert_eq!(
            body(None, Some("on")),
            Ok(serde_json::json!({"alsace_moselle": true}))
        );
        for zone in ["D", "b", "Zone B", " B"] {
            assert_eq!(body(Some(zone), None), Err("invalid_school_zone"), "{zone}");
        }
    }

    /// #844: the banner a saved setting brings back is announced.
    #[test]
    fn the_saved_banner_is_a_status() {
        let html = page(
            "",
            &me(OWNER),
            &family(),
            Accent::Sarcelle,
            Some(&[]),
            Access::default(),
            Some("Calendrier de la famille enregistré."),
            None,
        );
        assert!(
            html.contains(r#"<p class="notice success" role="status">Calendrier de la famille enregistré.</p>"#),
            "{html}"
        );
    }

    #[test]
    fn the_calendar_notice_and_error_are_in_french() {
        assert_eq!(
            notice_text("calendar_changed"),
            Some("Calendrier de la famille enregistré.")
        );
        assert_eq!(
            error_text("invalid_school_zone"),
            Some("Cette zone n'est pas proposée.")
        );
    }

    // -- RenameGroupRequest on the wire (#561) --------------------------------

    /// The rename and accent forms send their one setting: a calendar
    /// setting they leave out is absent from the body, so apps/api keeps it
    /// rather than reading `null` (which would clear the school zone).
    #[test]
    fn a_rename_body_carries_only_the_settings_it_sets() {
        let rename = RenameGroupRequest {
            name: Some("Maison".into()),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&rename).unwrap(),
            serde_json::json!({"name": "Maison"})
        );
        let accent = RenameGroupRequest {
            accent: Some("ocean".into()),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&accent).unwrap(),
            serde_json::json!({"accent": "ocean"})
        );
    }

    /// `Some(None)` is the explicit "no zone": sent as `null`, not dropped.
    #[test]
    fn clearing_the_school_zone_is_sent_as_null() {
        let clear = RenameGroupRequest {
            school_zone: Some(None),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&clear).unwrap(),
            serde_json::json!({"school_zone": null})
        );
        let set = RenameGroupRequest {
            school_zone: Some(Some("B".into())),
            alsace_moselle: Some(true),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&set).unwrap(),
            serde_json::json!({"school_zone": "B", "alsace_moselle": true})
        );
    }

    /// Read back, an absent key stays `None` and `null` stays `Some(None)`.
    #[test]
    fn a_rename_body_round_trips_absent_and_null_apart() {
        let absent: RenameGroupRequest =
            serde_json::from_value(serde_json::json!({"name": "Maison"})).unwrap();
        assert_eq!(absent.school_zone, None);
        assert_eq!(absent.alsace_moselle, None);
        let cleared: RenameGroupRequest =
            serde_json::from_value(serde_json::json!({"school_zone": null})).unwrap();
        assert_eq!(cleared.school_zone, Some(None));
        let set: RenameGroupRequest = serde_json::from_value(
            serde_json::json!({"school_zone": "C", "alsace_moselle": false}),
        )
        .unwrap();
        assert_eq!(set.school_zone, Some(Some("C".into())));
        assert_eq!(set.alsace_moselle, Some(false));
    }

    // -- « Équipements de la cuisine » (#819) -------------------------------

    fn equipment_card(html: &str) -> &str {
        let at = html
            .find(r#"<section class="card" id="equipements">"#)
            .expect("the equipment card");
        &html[at..at + html[at..].find("</section>").unwrap()]
    }

    #[test]
    fn owners_and_admins_tick_the_households_equipment() {
        let household = vec!["oven".to_string(), "hob".to_string()];
        for who in [OWNER, ADMIN] {
            let html = page(
                "",
                &me(who),
                &family(),
                Accent::Sarcelle,
                Some(&household),
                Access::default(),
                None,
                None,
            );
            let card = equipment_card(&html);
            assert!(
                card.contains("<h2>Équipements de la cuisine</h2>"),
                "{card}"
            );
            assert!(
                card.contains(&format!(
                    r#"<form method="post" action="/groups/{}/settings/equipment">"#,
                    family().id
                )),
                "{card}"
            );
            assert_eq!(
                card.matches(r#"type="checkbox" name="equipment""#).count(),
                EQUIPMENT.len(),
                "{card}"
            );
            assert!(
                card.contains(r#"value="oven" checked>Four</label>"#),
                "{card}"
            );
            assert!(
                card.contains(r#"value="microwave">Micro-ondes</label>"#),
                "{card}"
            );
            assert_eq!(card.matches(" checked").count(), 2, "{card}");
            assert!(card.contains("Enregistrer les équipements"), "{card}");
        }
        // Between « Calendrier » and the Google calendars.
        let html = page(
            "",
            &me(OWNER),
            &family(),
            Accent::Sarcelle,
            Some(&[]),
            Access::default(),
            None,
            None,
        );
        let calendar = html.find(r#"id="calendrier""#).unwrap();
        let equipment = html.find(r#"id="equipements""#).unwrap();
        let google = html.find("<h2>Agendas Google</h2>").unwrap();
        assert!(calendar < equipment && equipment < google);
    }

    #[test]
    fn a_standard_member_reads_the_equipment_only() {
        let household = vec!["oven".to_string()];
        let html = page(
            "",
            &me(STANDARD),
            &family(),
            Accent::Sarcelle,
            Some(&household),
            Access::default(),
            None,
            None,
        );
        let card = equipment_card(&html);
        assert!(
            card.contains(r#"<span class="badge">Four</span>"#),
            "{card}"
        );
        assert!(
            card.contains("Seuls le propriétaire et les admins modifient cette liste."),
            "{card}"
        );
        assert!(!card.contains("<form"), "{card}");
        assert!(!card.contains("<input"), "{card}");
        let html = page(
            "",
            &me(STANDARD),
            &family(),
            Accent::Sarcelle,
            Some(&[]),
            Access::default(),
            None,
            None,
        );
        assert!(equipment_card(&html).contains("Aucun équipement déclaré."));
    }

    #[test]
    fn an_unread_equipment_list_offers_no_form_to_empty_it() {
        let html = page(
            "",
            &me(OWNER),
            &family(),
            Accent::Sarcelle,
            None,
            Access::default(),
            None,
            None,
        );
        let card = equipment_card(&html);
        assert!(!card.contains("<form"), "{card}");
        assert!(card.contains(r#"<p class="notice error">"#), "{card}");
    }

    // -- « Accès » (#867) ----------------------------------------------------

    fn summary(policies: &[(&str, &str)]) -> GroupSummary {
        GroupSummary {
            group_id: G,
            name: "Famille Martin".to_string(),
            role: "owner".to_string(),
            accent: "sarcelle".to_string(),
            ownership_notice: None,
            access_policies: policies
                .iter()
                .map(|(feature, min_role)| AccessPolicy {
                    feature: feature.to_string(),
                    min_role: min_role.to_string(),
                })
                .collect(),
        }
    }

    fn page_with_access(id: Uuid, access: Access) -> String {
        page(
            "",
            &me(id),
            &family(),
            Accent::Sarcelle,
            Some(&[]),
            access,
            None,
            None,
        )
    }

    fn access_card(html: &str) -> &str {
        let at = html
            .find(r#"<section class="card access" id="acces""#)
            .expect("the access card");
        &html[at..at + html[at..].find("</section>").unwrap()]
    }

    /// The owner alone sees « Accès »: an admin and a standard member get
    /// the page they had, with no card and no form posting to it.
    #[test]
    fn the_access_card_is_the_owners_alone() {
        let family = summary(&[]);
        let access = || Access {
            group: Some(&family),
            notice: None,
            error: None,
        };
        let owner = page_with_access(OWNER, access());
        assert!(access_card(&owner).contains("<h2 id=\"acces-titre\">Accès</h2>"));
        for who in [ADMIN, STANDARD] {
            let html = page_with_access(who, access());
            assert!(!html.contains(r#"id="acces""#), "{html}");
            assert!(!html.contains("/settings/access"), "{html}");
        }
    }

    /// Seven fieldsets, one per feature in the API's order, each with three
    /// radios named after the feature; the family's threshold is checked,
    /// `admin` where the owner has set none.
    #[test]
    fn each_feature_offers_the_three_thresholds_with_the_familys_checked() {
        let family = summary(&[("budget", "standard"), ("google_calendar", "owner")]);
        let html = page_with_access(
            OWNER,
            Access {
                group: Some(&family),
                notice: None,
                error: None,
            },
        );
        let card = access_card(&html);
        assert!(
            card.contains(&format!(
                r#"<form method="post" action="/groups/{G}/settings/access" data-keep-scroll data-lock-on-change>"#
            )),
            "{card}"
        );
        assert_eq!(card.matches("<fieldset").count(), 7, "{card}");
        assert_eq!(card.matches(r#"type="radio""#).count(), 21, "{card}");
        assert_eq!(card.matches(" checked").count(), 7, "{card}");
        assert_eq!(card.matches(" data-submit-on-change").count(), 21, "{card}");
        let mut last = 0;
        for (feature, legend) in [
            ("budget", "Budget"),
            ("google_calendar", "Google Agenda"),
            ("stocks_edit", "Stocks"),
            ("recipes_edit", "Recettes"),
            ("grocery_edit", "Courses"),
            ("agenda_edit", "Agenda"),
            ("messagerie_moderate", "Messagerie"),
        ] {
            let at = card
                .find(&format!("<legend>{legend}</legend>"))
                .unwrap_or_else(|| panic!("{legend}: {card}"));
            assert!(at > last, "{legend} out of order: {card}");
            last = at;
            let checked = match feature {
                "budget" => "standard",
                "google_calendar" => "owner",
                _ => "admin",
            };
            for (value, label) in [
                ("standard", "Tous les membres"),
                ("admin", "Admins et propriétaire"),
                ("owner", "Propriétaire seul"),
            ] {
                let on = if value == checked { " checked" } else { "" };
                assert!(
                    card.contains(&format!(
                        r#"<label class="field inline pick"><input type="radio" name="{feature}" value="{value}"{on} data-submit-on-change>{label}</label>"#
                    )),
                    "{feature} {value}: {card}"
                );
            }
        }
        // What each feature covers, said once per fieldset.
        assert!(
            card.contains("Modifier ou supprimer ce que d’autres ont ajouté au stock."),
            "{card}"
        );
        assert!(
            card.contains("Supprimer les messages des autres membres."),
            "{card}"
        );
        // #844: applies on the click, the button being the fallback; it
        // names itself, so that its post is told from a radio's.
        assert!(card.contains(APPLIES_ON_CHANGE), "{card}");
        assert!(
            card.contains(r#"<p class="muted" data-hidden-with-js>Sans JavaScript, le choix s'envoie avec le bouton.</p>
<button type="submit" name="sent" value="button" data-hidden-with-js>Enregistrer l'accès</button>"#),
            "{card}"
        );
    }

    /// Where the page comes back: a radio's post (JavaScript, which puts
    /// the reader and the focus back by itself) to the page as it was; the
    /// button's, or a failure, to the card (`#acces`), whose banner says it.
    #[test]
    fn the_page_comes_back_at_the_card_after_the_button_or_a_failure() {
        let by = |sent: bool| {
            let mut p = vec![("budget".to_string(), "admin".to_string())];
            if sent {
                p.push(("sent".to_string(), "button".to_string()));
            }
            p
        };
        assert_eq!(
            access_redirect(&by(false), "notice=access_saved"),
            "?notice=access_saved"
        );
        assert_eq!(
            access_redirect(&by(true), "notice=access_saved"),
            "?notice=access_saved#acces"
        );
        for sent in [false, true] {
            assert_eq!(
                access_redirect(&by(sent), "error=access_failed"),
                "?error=access_failed#acces"
            );
        }
    }

    /// Placed between « Équipements de la cuisine » and the Google
    /// calendars, as the validated mock-up has it.
    #[test]
    fn the_access_card_follows_the_equipment() {
        let family = summary(&[]);
        let html = page_with_access(
            OWNER,
            Access {
                group: Some(&family),
                notice: None,
                error: None,
            },
        );
        let equipment = html.find(r#"id="equipements""#).unwrap();
        let access = html.find(r#"id="acces""#).unwrap();
        let google = html.find("<h2>Agendas Google</h2>").unwrap();
        assert!(equipment < access && access < google);
    }

    /// The thresholds unread (the family missing from `GET /groups`): no
    /// form, which would send seven defaults over the owner's settings.
    #[test]
    fn unread_thresholds_offer_no_form() {
        let html = page_with_access(
            OWNER,
            Access {
                group: None,
                notice: None,
                error: None,
            },
        );
        let card = access_card(&html);
        assert!(!card.contains("<form"), "{card}");
        assert!(card.contains(r#"<p class="notice error">"#), "{card}");
    }

    /// The banner of a save or of its failure is in the card, where the
    /// page comes back (`#acces`), not at the top of the page.
    #[test]
    fn the_access_banners_are_in_the_card() {
        let family = summary(&[]);
        let saved = page_with_access(
            OWNER,
            Access {
                group: Some(&family),
                notice: access_notice_text("access_saved"),
                error: None,
            },
        );
        assert!(
            access_card(&saved).contains(r#"<p class="notice success" role="status">Accès enregistré. Les membres voient le changement à leur prochaine page.</p>"#),
            "{saved}"
        );
        let failed = page_with_access(
            OWNER,
            Access {
                group: Some(&family),
                notice: None,
                error: access_error_text("access_failed"),
            },
        );
        let card = access_card(&failed);
        assert!(
            card.contains(r#"<p class="notice error" role="alert">Les accès n'ont pas été enregistrés. Vos choix précédents sont conservés ; réessayez dans un instant.</p>"#),
            "{card}"
        );
        assert!(card.contains("<form"), "{card}");
        assert!(access_error_text("invalid_access").is_some());
        // Not the page's own codes.
        assert_eq!(access_notice_text("renamed"), None);
        assert_eq!(access_error_text("forbidden"), None);
    }

    /// Each fieldset holds the line `enhance.js` fills while the choice is
    /// sent (`data-busy-status`), a live region present from the start.
    #[test]
    fn each_feature_has_a_busy_status() {
        let family = summary(&[]);
        let html = page_with_access(
            OWNER,
            Access {
                group: Some(&family),
                notice: None,
                error: None,
            },
        );
        let card = access_card(&html);
        assert_eq!(
            card.matches(r#"<p class="status" role="status" data-busy-status></p>"#)
                .count(),
            7,
            "{card}"
        );
    }

    /// What the form posts becomes `PUT /groups/:id/access-policies`' body:
    /// every feature sent with its threshold, in the order sent.
    #[test]
    fn the_access_form_sends_every_threshold_posted() {
        let pairs = |p: &[(&str, &str)]| {
            p.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<Vec<_>>()
        };
        let body = access_request(&pairs(&[("budget", "standard"), ("stocks_edit", "owner")]))
            .map(|r| serde_json::to_value(r).unwrap());
        assert_eq!(
            body,
            Ok(serde_json::json!({"policies": [
                {"feature": "budget", "min_role": "standard"},
                {"feature": "stocks_edit", "min_role": "owner"},
            ]}))
        );
        // Nothing to send.
        // The button's own name is not a feature.
        assert_eq!(
            access_request(&pairs(&[("budget", "owner"), ("sent", "button")])),
            Ok(AccessPolicies {
                policies: vec![AccessPolicy {
                    feature: "budget".into(),
                    min_role: "owner".into()
                }]
            })
        );
        assert_eq!(access_request(&pairs(&[])), Err("invalid_access"));
        assert_eq!(
            access_request(&pairs(&[("sent", "button")])),
            Err("invalid_access")
        );
        // A threshold off the list, a feature twice, a feature unknown.
        for bad in [
            &[("budget", "member")][..],
            &[("budget", "Admin")][..],
            &[("budget", "admin"), ("budget", "owner")][..],
            &[("chores", "admin")][..],
        ] {
            assert_eq!(
                access_request(&pairs(bad)),
                Err("invalid_access"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_equipment_notice_and_error_are_in_french() {
        assert_eq!(
            notice_text("equipment_saved"),
            Some("Équipements enregistrés.")
        );
        assert!(error_text("unknown_equipment").is_some());
    }
}
