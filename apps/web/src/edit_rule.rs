//! The muted line a member reads in place of the edit controls they may not
//! use (#962). Who may edit follows the group's threshold for the feature
//! (`edit_min_role`, #863 to #867) — the creator always may, then every
//! role meeting the threshold (`role_meets`) — so the line says the rule of
//! that threshold instead of a fixed « créateur ou administrateur ».

/// What the line is about: the four screens that show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Guarded {
    /// An article of the stocks (`stocks_edit`).
    StockItem,
    /// A recipe (`recipes_edit`).
    Recipe,
    /// An event of the agenda (`agenda_edit`).
    Event,
    /// A recipe's publication to the library (`recipes_edit`).
    Publication,
}

impl Guarded {
    /// What the line says may be done, after « peut » / « peuvent ».
    fn action(self) -> &'static str {
        match self {
            Self::StockItem => "modifier ou supprimer cet article",
            Self::Recipe => "modifier ou supprimer cette recette",
            Self::Event => "modifier ou supprimer cet événement",
            Self::Publication => "publier cette recette dans la bibliothèque",
        }
    }
}

/// Who may do `what` under the threshold `min_role`, as a sentence. A
/// threshold off the list reads as `owner`: `role_meets` admits the owner
/// alone there (and the creator passes whatever the threshold).
pub(crate) fn who_may(min_role: &str, what: Guarded) -> String {
    let action = what.action();
    match min_role {
        "standard" => format!("Tous les membres peuvent {action}."),
        "admin" => {
            format!("Seuls le créateur, les administrateurs et le propriétaire peuvent {action}.")
        }
        _ => format!("Seul le propriétaire ou le créateur peut {action}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_follows_the_threshold_for_every_object() {
        let cases = [
            (
                "owner",
                Guarded::StockItem,
                "Seul le propriétaire ou le créateur peut modifier ou supprimer cet article.",
            ),
            (
                "owner",
                Guarded::Recipe,
                "Seul le propriétaire ou le créateur peut modifier ou supprimer cette recette.",
            ),
            (
                "owner",
                Guarded::Event,
                "Seul le propriétaire ou le créateur peut modifier ou supprimer cet événement.",
            ),
            (
                "owner",
                Guarded::Publication,
                "Seul le propriétaire ou le créateur peut publier cette recette dans la bibliothèque.",
            ),
            (
                "admin",
                Guarded::StockItem,
                "Seuls le créateur, les administrateurs et le propriétaire peuvent modifier ou supprimer cet article.",
            ),
            (
                "admin",
                Guarded::Recipe,
                "Seuls le créateur, les administrateurs et le propriétaire peuvent modifier ou supprimer cette recette.",
            ),
            (
                "admin",
                Guarded::Event,
                "Seuls le créateur, les administrateurs et le propriétaire peuvent modifier ou supprimer cet événement.",
            ),
            (
                "admin",
                Guarded::Publication,
                "Seuls le créateur, les administrateurs et le propriétaire peuvent publier cette recette dans la bibliothèque.",
            ),
            (
                "standard",
                Guarded::StockItem,
                "Tous les membres peuvent modifier ou supprimer cet article.",
            ),
            (
                "standard",
                Guarded::Recipe,
                "Tous les membres peuvent modifier ou supprimer cette recette.",
            ),
            (
                "standard",
                Guarded::Event,
                "Tous les membres peuvent modifier ou supprimer cet événement.",
            ),
            (
                "standard",
                Guarded::Publication,
                "Tous les membres peuvent publier cette recette dans la bibliothèque.",
            ),
        ];
        for (min_role, what, expected) in cases {
            assert_eq!(who_may(min_role, what), expected, "{min_role}, {what:?}");
        }
    }

    #[test]
    fn a_threshold_off_the_list_admits_the_owner_alone() {
        // `role_meets`: a threshold off the list lets only the owner pass,
        // and the creator passes whatever the threshold.
        for min_role in ["superuser", ""] {
            assert_eq!(
                who_may(min_role, Guarded::Recipe),
                who_may("owner", Guarded::Recipe),
                "{min_role:?}"
            );
        }
    }
}
