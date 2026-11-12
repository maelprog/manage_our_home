-- #777: the catch-up of thumbnails (`jobs::thumbnail_catch_up`) makes the
-- thumbnail of every picture stored without one (`thumbnail_key IS NULL`,
-- 0053): those uploaded before #774, and those whose decode permit was
-- busy at upload.
--
-- How many times it tried and failed, so that a picture which will never
-- decode (corrupt, past the decode bounds) is not read back from the
-- bucket and decoded again on every pass. An undecodable picture spends
-- every attempt at once; a storage that did not answer, one at a time.
-- Past the job's bound the row is left without a thumbnail for good, and
-- the gallery shows its generic tile.
--
-- NOT NULL DEFAULT 0: every existing row is "never tried", and Postgres
-- adds a constant default without rewriting the table.
--
-- RLS: `event_attachments_isolation` (0002, rewritten by 0042) filters
-- whole rows through the event's group, so it covers this column as is.
ALTER TABLE event_attachments
    ADD COLUMN thumbnail_attempts SMALLINT NOT NULL DEFAULT 0;

COMMENT ON COLUMN event_attachments.thumbnail_attempts IS
    'Failed attempts of the thumbnail catch-up on this attachment (issue #777). Past the job''s bound, the attachment is left without a thumbnail.';
