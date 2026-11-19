pub mod barcode;
pub mod gs1;
pub mod items;
pub mod openfoodfacts;
pub mod planned_usage;
pub mod reception;
pub mod scan;
pub mod shelf_life;

use crate::groups::access::role_meets;

/// The full-record-edit / delete permission bar for stock items: the item's
/// creator, or a member whose role meets `min_role`, the family's
/// `stocks_edit` threshold (#866, `admin` by default). Reading, creating and
/// a **quantity-only** adjustment are open to any member whatever the
/// threshold (shared family inventory) — those paths don't call this; see
/// `items::UpdateStockItemRequest::touches_full_record`.
pub(crate) fn can_modify(actor_role: &str, min_role: &str, actor_is_creator: bool) -> bool {
    actor_is_creator || role_meets(actor_role, min_role)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every role × threshold × authorship (#866), written out rather than
    /// derived: the creator always may; anyone else when their role meets
    /// the family's threshold, the owner whatever it says.
    const CASES: [(&str, &str, bool, bool); 18] = [
        ("standard", "standard", false, true),
        ("standard", "admin", false, false),
        ("standard", "owner", false, false),
        ("admin", "standard", false, true),
        ("admin", "admin", false, true),
        ("admin", "owner", false, false),
        ("owner", "standard", false, true),
        ("owner", "admin", false, true),
        ("owner", "owner", false, true),
        ("standard", "standard", true, true),
        ("standard", "admin", true, true),
        ("standard", "owner", true, true),
        ("admin", "standard", true, true),
        ("admin", "admin", true, true),
        ("admin", "owner", true, true),
        ("owner", "standard", true, true),
        ("owner", "admin", true, true),
        ("owner", "owner", true, true),
    ];

    #[test]
    fn can_modify_follows_the_threshold_and_spares_the_creator() {
        for (role, min_role, creator, expected) in CASES {
            assert_eq!(
                can_modify(role, min_role, creator),
                expected,
                "{role} under {min_role}, creator: {creator}"
            );
        }
    }
}
