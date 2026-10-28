//! The profile page's own blocks (#529, maquettes « Mobile — Profil » et
//! « PC — Profil et familles »): who you are, your families with the active
//! one picked by radio buttons, and the account's settings as a list.
//!
//! The family choice is the header switcher's form under another face: the
//! same `POST /groups/switch` (`routes::groups::switch`), which re-checks the
//! posted group against the caller's memberships and sets the
//! `active_group_id` cookie, then comes back here. Picking a radio does not
//! submit by itself: the arrow keys walk a radio group, and a page that
//! reloaded on each step would change context under the keyboard
//! (WCAG 3.2.2). The button does.

use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use uuid::Uuid;

use super::{avatar, can_request_deletion};
use crate::app::{html_escape, member_avatar};
use crate::icons::{icon, Icon};
use crate::routes::groups::role_label;

/// The identity block: your icon (the photo, else the initial), your name
/// as the page's title, your e-mail.
pub(crate) fn profile_head(me: &MeResponse, has_photo: bool) -> String {
    format!(
        r#"<div class="profile-head">{icon}<div><h1>{name}</h1><p class="muted">{email}</p></div></div>"#,
        icon = member_avatar(
            me.user_id,
            &me.display_name,
            has_photo.then_some(avatar::OWN_PHOTO_URL),
        ),
        name = html_escape(&me.display_name),
        email = html_escape(&me.email),
    )
}

/// "1 membre", "4 membres".
pub(crate) fn members_label(count: usize) -> String {
    if count > 1 {
        format!("{count} membres")
    } else {
        format!("{count} membre")
    }
}

/// One family to pick as the active one. The whole row is the radio's
/// label, so the row is the target a finger aims at, and the radio's name
/// is the family's. `members` is `None` when the count could not be read:
/// the role is shown alone rather than a wrong figure.
pub(crate) fn family_choice(g: &GroupSummary, members: Option<usize>, active: bool) -> String {
    let (checked, badge) = if active {
        (" checked", r#" <span class="badge">Active</span>"#)
    } else {
        ("", "")
    };
    let role = role_label(&g.role);
    let meta = match members {
        Some(n) => format!("{} · {role}", members_label(n)),
        None => role.to_string(),
    };
    format!(
        r#"<label class="list-row family"><input type="radio" name="group_id" value="{id}"{checked}><span><strong>{name}</strong>{badge}<br><span class="muted">{meta}</span></span></label>"#,
        id = g.group_id,
        name = html_escape(&g.name),
    )
}

/// "Mes familles": the radio group, then the ways in and out of a family.
/// Joining goes through the invitation form of `/groups`: an invitation is
/// a link sent by e-mail, and apps/api lists none for the person invited.
pub(crate) fn families_section(
    groups: &[(GroupSummary, Option<usize>)],
    active: Option<Uuid>,
) -> String {
    let choice = if groups.is_empty() {
        r#"<p class="muted">Vous n'appartenez à aucune famille pour le moment.</p>"#.to_string()
    } else {
        let rows: String = groups
            .iter()
            .map(|(g, members)| family_choice(g, *members, active == Some(g.group_id)))
            .collect();
        format!(
            r#"<form method="post" action="/groups/switch">
<div role="radiogroup" aria-labelledby="families">{rows}</div>
<input type="hidden" name="redirect_to" value="/account">
<button type="submit">Utiliser cette famille</button>
</form>"#
        )
    };
    format!(
        r#"<section class="card">
<h2 id="families">Mes familles</h2>
{choice}
<div class="actions">
<a class="btn secondary" href="/groups/new">{plus} Créer une famille</a>
<a class="btn secondary" href="/groups#join">Rejoindre une famille</a>
<a class="btn secondary" href="/groups">{members} Gérer mes familles</a>
</div>
</section>"#,
        plus = icon(Icon::Plus),
        members = icon(Icon::Members),
    )
}

/// One row of the account's settings: a link the width of the card, its
/// name, what lies behind it, and the chevron that says it leads somewhere.
fn setting_row(href: &str, name: &str, hint: &str) -> String {
    format!(
        r#"<li><a class="list-row" href="{href}"><span>{name}<br><span class="muted">{hint}</span></span>{next}</a></li>"#,
        next = icon(Icon::Next),
    )
}

/// The account's settings, one row each, then logging out. The link names
/// are those of the cards they replace (DESIGN.md: the suite finds them by
/// name). Deletion is offered only while no request is pending: the
/// pending panel, at the top of the page, carries the cancel action then.
pub(crate) fn settings_section(me: &MeResponse) -> String {
    let deletion = if can_request_deletion(me.deletion_requested_at) {
        setting_row(
            "/account/delete",
            "Supprimer mon compte",
            "Droit à l'effacement, après un délai de grâce annulable",
        )
    } else {
        String::new()
    };
    format!(
        r#"<section class="card">
<h2>Compte</h2>
<ul class="list">
{notifications}
{sessions}
{export}
{deletion}
</ul>
<form method="post" action="/logout">
<button type="submit" class="secondary">Se déconnecter</button>
</form>
</section>"#,
        notifications = setting_row(
            "/account/notifications",
            "Gérer mes notifications",
            "Rappels de vos événements, par notification ou par e-mail",
        ),
        sessions = setting_row(
            "/account/sessions",
            "Gérer mes sessions",
            "Les appareils connectés à votre compte",
        ),
        export = setting_row(
            "/account/export",
            "Exporter mes données",
            "Droit d'accès et de portabilité : tout ce qui vous concerne, en JSON",
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(n: u128, name: &str, role: &str) -> GroupSummary {
        GroupSummary {
            group_id: Uuid::from_u128(n),
            name: name.to_string(),
            role: role.to_string(),
            ownership_notice: None,
        }
    }

    fn me(deletion_requested: bool) -> MeResponse {
        serde_json::from_value(serde_json::json!({
            "user_id": Uuid::from_u128(9),
            "email": "alex@exemple.fr",
            "display_name": "Alex",
            "email_verified": true,
            "is_superadmin": false,
            "deletion_requested_at": deletion_requested.then(chrono::Utc::now),
        }))
        .expect("a MeResponse")
    }

    #[test]
    fn the_head_names_you_and_shows_your_icon_and_email() {
        let mut who = me(false);
        who.display_name = "<Alex>".to_string();
        let html = profile_head(&who, false);
        assert!(html.contains("<h1>&lt;Alex&gt;</h1>"), "{html}");
        assert!(html.contains("alex@exemple.fr"), "{html}");
        assert!(html.contains(r#"<span class="avatar""#), "{html}");
        assert!(!html.contains("<img"), "{html}");
        let with_photo = profile_head(&me(false), true);
        assert!(
            with_photo.contains(r#"<img src="/account/avatar""#),
            "{with_photo}"
        );
    }

    #[test]
    fn one_member_is_singular_and_more_are_plural() {
        assert_eq!(members_label(1), "1 membre");
        assert_eq!(members_label(2), "2 membres");
        assert_eq!(members_label(12), "12 membres");
    }

    #[test]
    fn a_family_is_a_radio_named_like_the_switcher_field() {
        let g = group(1, "Foyer", "owner");
        let html = family_choice(&g, Some(4), false);
        assert!(
            html.contains(&format!(
                r#"<input type="radio" name="group_id" value="{}">"#,
                g.group_id
            )),
            "{html}"
        );
        assert!(
            html.starts_with(r#"<label class="list-row family">"#),
            "{html}"
        );
    }

    #[test]
    fn only_the_active_family_is_checked_and_says_so() {
        let g = group(1, "Foyer", "member");
        let active = family_choice(&g, Some(2), true);
        assert!(active.contains(" checked"), "{active}");
        assert!(
            active.contains(r#"<span class="badge">Active</span>"#),
            "{active}"
        );
        let other = family_choice(&g, Some(2), false);
        assert!(!other.contains("checked"), "{other}");
        assert!(!other.contains("Active"), "{other}");
    }

    #[test]
    fn a_family_shows_its_member_count_and_your_role() {
        let html = family_choice(&group(1, "Foyer", "owner"), Some(4), false);
        assert!(html.contains("4 membres · Propriétaire"), "{html}");
        let html = family_choice(&group(1, "Foyer", "admin"), Some(1), false);
        assert!(html.contains("1 membre · Admin"), "{html}");
    }

    #[test]
    fn an_unknown_member_count_leaves_the_role_alone() {
        let html = family_choice(&group(1, "Foyer", "member"), None, false);
        assert!(
            html.contains(r#"<span class="muted">Membre</span>"#),
            "{html}"
        );
        assert!(!html.contains("membre "), "{html}");
    }

    #[test]
    fn a_family_name_is_escaped() {
        let html = family_choice(&group(1, "<b>Nous</b>", "member"), Some(2), false);
        assert!(html.contains("&lt;b&gt;Nous&lt;/b&gt;"), "{html}");
        assert!(!html.contains("<b>Nous"), "{html}");
    }

    #[test]
    fn the_choice_posts_to_the_switcher_and_comes_back_here() {
        let groups = [
            (group(1, "Une", "owner"), Some(1)),
            (group(2, "Deux", "member"), Some(3)),
        ];
        let html = families_section(&groups, Some(Uuid::from_u128(2)));
        assert!(
            html.contains(r#"<form method="post" action="/groups/switch">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="redirect_to" value="/account">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"role="radiogroup" aria-labelledby="families""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<h2 id="families">Mes familles</h2>"#),
            "{html}"
        );
        assert_eq!(html.matches(r#"type="radio""#).count(), 2, "{html}");
        assert_eq!(html.matches(" checked").count(), 1, "{html}");
        let checked = html.find(" checked").expect("one checked");
        assert!(
            html[..checked].ends_with(&format!(r#"value="{}""#, Uuid::from_u128(2))),
            "{html}"
        );
        assert!(
            html.contains("<button type=\"submit\">Utiliser cette famille</button>"),
            "{html}"
        );
    }

    #[test]
    fn the_families_card_leads_to_creating_joining_and_managing() {
        let html = families_section(&[(group(1, "Une", "owner"), Some(1))], None);
        assert!(html.contains(r#"href="/groups/new""#), "{html}");
        assert!(html.contains(r#"href="/groups#join""#), "{html}");
        assert!(html.contains(r#"href="/groups""#), "{html}");
    }

    #[test]
    fn without_a_family_there_is_nothing_to_pick() {
        let html = families_section(&[], None);
        assert!(!html.contains("<form"), "{html}");
        assert!(html.contains("aucune famille"), "{html}");
        assert!(html.contains(r#"href="/groups/new""#), "{html}");
    }

    #[test]
    fn the_rgpd_rights_are_one_click_from_the_profile() {
        // #529: export and deletion stay ≤ 2 clicks from anywhere — the
        // header's "Mon compte", then these.
        let html = settings_section(&me(false));
        assert!(html.contains(r#"href="/account/export""#), "{html}");
        assert!(html.contains(r#"href="/account/delete""#), "{html}");
        assert!(html.contains("Exporter mes données"), "{html}");
        assert!(html.contains("Supprimer mon compte"), "{html}");
    }

    #[test]
    fn the_settings_keep_the_link_names_the_suite_finds_them_by() {
        let html = settings_section(&me(false));
        for (href, name) in [
            ("/account/notifications", "Gérer mes notifications"),
            ("/account/sessions", "Gérer mes sessions"),
            ("/account/export", "Exporter mes données"),
            ("/account/delete", "Supprimer mon compte"),
        ] {
            let at = html.find(&format!(r#"href="{href}""#)).expect(href);
            let close = at + html[at..].find("</a>").expect("closed");
            assert!(html[at..close].contains(name), "{href}: {html}");
        }
    }

    #[test]
    fn logging_out_is_on_the_profile() {
        let html = settings_section(&me(false));
        assert!(
            html.contains(r#"<form method="post" action="/logout">"#),
            "{html}"
        );
        assert!(html.contains("Se déconnecter</button>"), "{html}");
    }

    #[test]
    fn a_pending_deletion_replaces_the_deletion_link() {
        let html = settings_section(&me(true));
        assert!(!html.contains(r#"href="/account/delete""#), "{html}");
        assert!(html.contains(r#"href="/account/export""#), "{html}");
    }
}
