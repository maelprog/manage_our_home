-- #633: an expense links to zero, one or several grocery items of its
-- family, up to 100 (`budget::entries::GROCERY_ITEMS_MAX`), and a grocery
-- item to one expense at most (user's decisions of 2026-10-30). The link
-- of 0007 — `budget_entries.grocery_item_id`, one item per expense — moves
-- into this table, then goes: one place says which items an expense
-- links to, whether a price set on a checked item made it or the expense
-- form did.
--
-- Deleting a grocery item deletes its link, the expense stays (it keeps
-- the name 0007 copied from the item); deleting the expense deletes its
-- links. No personal data: an expense, an item and their family.

-- Targets of the composite keys below, which make a link of an expense of
-- family A to an item of family B impossible whatever the API checks.
ALTER TABLE budget_entries
    ADD CONSTRAINT budget_entries_id_group_id_key UNIQUE (id, group_id);
ALTER TABLE grocery_items
    ADD CONSTRAINT grocery_items_id_group_id_key UNIQUE (id, group_id);

CREATE TABLE budget_entry_grocery_items (
    entry_id            UUID NOT NULL,
    grocery_item_id     UUID NOT NULL,
    group_id            UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    -- Also serves `WHERE entry_id = $1` through its leading column.
    PRIMARY KEY (entry_id, grocery_item_id),
    -- One expense per item: what 0007's unique index on
    -- `budget_entries.grocery_item_id` held, so that a price set twice on
    -- an item updates its expense instead of counting it twice.
    UNIQUE (grocery_item_id),
    FOREIGN KEY (entry_id, group_id)
        REFERENCES budget_entries (id, group_id) ON DELETE CASCADE,
    FOREIGN KEY (grocery_item_id, group_id)
        REFERENCES grocery_items (id, group_id) ON DELETE CASCADE
);

ALTER TABLE budget_entry_grocery_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE budget_entry_grocery_items FORCE ROW LEVEL SECURITY;
CREATE POLICY budget_entry_grocery_items_isolation ON budget_entry_grocery_items
    USING (group_id::text = current_setting('app.family_id', true));

-- The links of 0007, copied. An item of another family than its expense's
-- was never refused by 0007's key, though the API never wrote one: such a
-- link, if any, is not copied, the composite key above would refuse it.
INSERT INTO budget_entry_grocery_items (entry_id, grocery_item_id, group_id)
SELECT e.id, e.grocery_item_id, e.group_id
FROM budget_entries e
JOIN grocery_items gi ON gi.id = e.grocery_item_id AND gi.group_id = e.group_id;

DROP INDEX budget_entries_grocery_item_id_uidx;
ALTER TABLE budget_entries DROP COLUMN grocery_item_id;
