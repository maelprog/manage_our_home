-- #563: the school holidays of zones A, B and C, read once a day from the
-- Éducation nationale's open data (`fr-en-calendrier-scolaire`,
-- data.education.gouv.fr, Licence Ouverte) by `jobs::school_holidays`, and
-- served with the national days to the families whose `school_zone` (0046)
-- names the zone.
--
-- One row per period of a zone: its official label (« Vacances de Noël »),
-- its first and last day off, both included, the school year it belongs to,
-- and when the server read it. Nothing else of the dataset is kept.
--
-- RLS posture: none, deliberately, like `off_products` (0026). The rows are
-- the national school calendar, public, and belong to no family and no
-- account: no `group_id` to scope by, no user id, nothing a member types.
-- The same rows serve every family of a zone. Which zone a family chose is
-- `groups.school_zone`, behind the policies of `groups`; this table does not
-- say who reads which zone, or when. `fetched_at` is when the job last read
-- the dataset, which no family causes.
CREATE TABLE school_holidays (
    zone        TEXT NOT NULL CHECK (zone IN ('A', 'B', 'C')),
    label       TEXT NOT NULL CHECK (btrim(label) <> ''),
    starts_on   DATE NOT NULL,
    ends_on     DATE NOT NULL,
    school_year TEXT NOT NULL CHECK (school_year ~ '^[0-9]{4}-[0-9]{4}$'),
    fetched_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (zone, starts_on, label),
    CHECK (ends_on >= starts_on)
);
