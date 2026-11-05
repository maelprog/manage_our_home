-- #561: a family's calendar settings, chosen by its owner or admins
-- (`PATCH /groups/:id`) and read by every member (`GET /groups/:id`).
--
-- `school_zone` is the family's zone of the French school calendar, one of
-- `manage_our_home_shared::validation::groups::SchoolZone`'s three letters;
-- `NULL`, the default, means no school holidays are shown. `alsace_moselle`
-- adds the public holidays of Alsace-Moselle; every existing family starts
-- without them. The days themselves are computed elsewhere (#562, #563).
--
-- RLS unchanged: the columns live on `groups`, under its policies.
ALTER TABLE groups
    ADD COLUMN school_zone TEXT NULL
        CONSTRAINT groups_school_zone_check CHECK (school_zone IN ('A', 'B', 'C')),
    ADD COLUMN alsace_moselle BOOLEAN NOT NULL DEFAULT false;
