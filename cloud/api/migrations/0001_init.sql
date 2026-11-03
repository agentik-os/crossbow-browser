-- Crossbow accounts, devices, sessions and sign-in challenges (docs/sync-protocol.md).
-- All timestamps are Unix milliseconds.
CREATE TABLE accounts (id TEXT PRIMARY KEY, email TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL);
CREATE TABLE devices (id TEXT PRIMARY KEY, account_id TEXT NOT NULL REFERENCES accounts(id), name TEXT NOT NULL,
  platform TEXT NOT NULL, created_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL, revoked_at INTEGER);
CREATE TABLE sessions (token_hash TEXT PRIMARY KEY, account_id TEXT NOT NULL, device_id TEXT NOT NULL,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE auth_challenges (id TEXT PRIMARY KEY, email TEXT NOT NULL, code_hash TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
  ip TEXT, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, consumed_at INTEGER);
CREATE INDEX auth_challenges_email ON auth_challenges(email, created_at);
CREATE INDEX auth_challenges_ip ON auth_challenges(ip, created_at);
CREATE INDEX sessions_device ON sessions(device_id);
-- Not in the v1 spec text: lets /v1/me list an account's devices without a scan.
CREATE INDEX devices_account ON devices(account_id, created_at);
