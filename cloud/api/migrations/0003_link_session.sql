-- A held link is confirmable only by the session that made it
-- (docs/sync-protocol.md). Links are held for 5 minutes, so any made before
-- this column existed are dropped rather than left without a session.
DELETE FROM identity_links;
ALTER TABLE identity_links ADD COLUMN session_hash TEXT NOT NULL DEFAULT '';
