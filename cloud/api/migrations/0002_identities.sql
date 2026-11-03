-- Browser sign-in with Clerk (docs/sync-protocol.md). A provider identity is
-- bound to one account by issuer and subject, never matched by email.
CREATE TABLE identities (issuer TEXT NOT NULL, subject TEXT NOT NULL, account_id TEXT NOT NULL REFERENCES accounts(id),
  created_at INTEGER NOT NULL, PRIMARY KEY (issuer, subject));
CREATE INDEX identities_account ON identities(account_id);
-- A browser sign-in waiting for the signed-in owner of account_id to confirm
-- the link. Expired rows are purged daily.
CREATE TABLE identity_links (id TEXT PRIMARY KEY, account_id TEXT NOT NULL, issuer TEXT NOT NULL, subject TEXT NOT NULL,
  email TEXT, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
CREATE INDEX identity_links_expires ON identity_links(expires_at);
