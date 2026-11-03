# Crossbow account and sync protocol (v1)

**Current development contract, not the target security architecture.** The desktop client emits `Encoding::Json` by default; an opaque server payload or an available encryption enum does not make sync end-to-end encrypted. Newer source can seal library rows as `xchacha20_v1` after the user explicitly turns encryption on (see [Encrypted library rows](#encrypted-library-rows-desktop-source-opt-in)). That path is source-tested only and has had no external review. Passwords are not implemented and must not be added to this plaintext pipeline. Clerk is the selected provider for future browser identity. See [the identity migration and encrypted-sync design](security-architecture.md) before extending authentication or syncing sensitive records.

The wire types live in `crates/crossbow-proto` and are shared by the desktop app
and the Cloudflare Worker. This document is the contract; the crate is the source
of truth for field names.

## Cloudflare layout

Each deployment chooses its own Worker and D1 names. The `<your-...>` names below are the placeholders in `cloud/api/wrangler.example.toml` and `cloud/ai/wrangler.example.toml`; see [Deploy your own](../cloud/README.md#deploy-your-own).

| Piece | Name | Notes |
|---|---|---|
| API Worker (Rust, `workers-rs`) | `<your-worker-name>` (prod), `<your-dev-worker-name>` (dev) | Public front door on `*.workers.dev`. Auth, devices, sync routing, `/v1/ai/*` proxy. |
| AI Worker (TypeScript, Agents SDK + AI SDK) | `<your-ai-worker-name>` (prod), `<your-dev-ai-worker-name>` (dev) | Not public (`workers_dev = false`). Reached only through a service binding from the API Worker. |
| D1 | `<your-d1-database-name>` (prod), `<your-dev-d1-database-name>` (dev) | Accounts, devices, sessions, sign-in challenges, browser identities and held links. |
| Durable Object `SyncRoom` (SQLite storage) | one per account (`idFromName(account_id)`) | Entity store, sequence counter, WebSocket fan-out. |
| Workers AI binding `AI` | on the AI Worker | Future AI features; no app UI uses it yet. |

Dev differs from prod by `ENVIRONMENT = "dev"`, which lets `/v1/auth/start`
return the code in the response when no mail provider is configured (prod never
returns it), and by `CLERK_ISSUER` and `CLERK_CLIENT_ID`, which turn on browser
sign-in. Prod sets neither, so its Clerk routes answer `503 clerk_unavailable`.

## Authentication

Email + 6-digit code, no passwords, or a browser sign-in with Clerk where the
server offers it ([Browser sign-in](#browser-sign-in-clerk)). Both end in the
same device and session.

1. `POST /v1/auth/start {email}` → `{challenge_id, expires_in, dev_code?}`
   - Normalise with `normalize_email`; reject invalid with `400 bad_request`.
   - Rate limits: 5 starts per email per hour, 30 per client IP (`CF-Connecting-IP`) per hour → `429 rate_limited`.
   - Code: 6 random digits from a CSPRNG. Store `sha256(AUTH_PEPPER || challenge_id || code)` hex; never the code.
   - Expiry 10 minutes, max 5 verify attempts.
   - Mail via Resend (`RESEND_API_KEY` secret, `MAIL_FROM` var). If no key: prod → `503 mail_unavailable`; dev → the same answer as a mailed code, and the code goes nowhere.
   - `dev_code` is returned only in dev, and only when the request's `x-crossbow-dev-key` header matches the `DEV_CODE_KEY` secret (compared in constant time, as SHA-256 digests); then nothing is mailed. No secret set, no header or a wrong key: no `dev_code`. Prod never returns one. Test tools read the key from the `CROSSBOW_DEV_KEY` environment variable; it is never in source.
2. `POST /v1/auth/verify {challenge_id, code, device_name, platform}` → `{session_token, account, device}`
   - Wrong code → `401 invalid_code` and `attempts += 1`; after 5 → `429 too_many_attempts`; expired → `401 code_expired`. A challenge is single-use.
   - Creates the account on first sign-in (by normalised email), always creates a new device.
   - Token: `cbs_` + base64url(32 random bytes). Store `sha256(token)` hex only. Sliding expiry 180 days.
3. Every other route takes `Authorization: Bearer <token>`. Missing, unknown, expired or revoked-device token → `401 unauthorized`. Each authenticated call updates `devices.last_seen_at` (at most once per minute).
4. `POST /v1/auth/logout` deletes the current session → `204`.
5. `GET /v1/me` → `MeResponse` (current account, current device, all non-revoked devices, and `browser_sign_in`: whether a browser identity is linked).
6. `DELETE /v1/devices/{id}` revokes a device of the same account and deletes its sessions → `204`; another account's device → `404 not_found`.

### Browser sign-in (Clerk)

A native-app flow (RFC 8252) with a public OAuth client: no client secret
exists in the app or the Worker. The ID token is the only provider token the
Worker reads; Clerk's access and refresh tokens are discarded unread and never
stored or logged. The app never receives any Clerk token.

1. `GET /v1/health` names the configuration when the server offers it:
   `clerk: {issuer, client_id, authorize_endpoint}`. Absent means email codes only.
2. The app makes a PKCE verifier (32 random bytes) with its `S256` challenge,
   a `state` and a `nonce`, and listens on `127.0.0.1` only, on the first free
   port of 47231, 47232 and 47233; all three busy is an error, not a fallback.
   It hands the authorize URL to the system as any link, so the default
   browser opens it; when that is Crossbow, it comes back as a link from
   another app and opens as an ordinary tab under the Links rules, which the
   app never reads, scripts or watches. The URL carries `response_type=code`,
   `client_id`, the exact `http://127.0.0.1:<port>/callback`,
   `scope=openid email profile` (no `offline_access`), `state`, `nonce`,
   `code_challenge` and `code_challenge_method=S256`.
3. The listener answers one `/callback` and then closes: a missing or other
   `state` ends the attempt without a code; `error=access_denied` is a cancel;
   any other `error` is a failure (only its code is shown). It answers
   "You can close this tab and return to Crossbow." Other paths get `404` and
   the wait goes on. The wait ends after 5 minutes, or at once on Cancel.
4. `POST /v1/auth/clerk {code, code_verifier, redirect_uri, nonce, device_name, platform}`
   → `AuthVerifyResponse` with an optional `display_name`. No session needed.
   - The redirect must be one of the four registered; the code, verifier and
     nonce are length- and character-checked.
   - The Worker redeems the code at `<issuer>/oauth/token` with
     `grant_type=authorization_code`, the code, redirect, `client_id` and verifier.
     `400`/`401` → `401 sign_in_rejected` (expired, used or mismatched); anything
     else, or no `id_token` → `502 provider_unavailable`.
   - ID token checks, each refused on its own as `401 sign_in_rejected`: header
     `alg` RS256; `iss` exactly the issuer; `aud` contains the client id; `azp`,
     when present, equals it; `exp` after now and `iat` not after now, with 60 s
     skew; `nonce` equals the request's; `sub` present, at most 255 characters,
     no control characters.
   - The token came straight from the token endpoint over TLS, which OIDC Core
     3.1.3.7 accepts in place of a signature check. The Worker checks the RS256
     signature anyway, with Web Crypto, against the key with the token's `kid`
     in `<issuer>/.well-known/jwks.json`, fetched for each sign-in. No matching
     key refuses; a key set that can't be read is `502 provider_unavailable`.
   - Mapping, by `(issuer, subject)`, never by email:
     - bound to an account → sign in to that account by its id, unchanged;
     - unknown, with a verified email no account uses → a new account and its
       identity row, in one batch that creates nothing if the email or the
       identity was taken meanwhile;
     - unknown, with an email an account uses → `409 link_required` (sign in
       with an email code, then link);
     - unknown, without a verified email → `403 email_unverified`.
5. `POST /v1/auth/clerk/link` (session, same body) redeems and checks a browser
   sign-in the same way, then holds it for 5 minutes for the caller's account
   and the session that asked (`identity_links.session_hash`, migration 0003)
   → `{link_id, email, expires_in}`. `email` is the provider's only when it
   marked it verified, else `null`. `link_id` is `null` when it is already the
   caller's; bound to another account → `409 identity_in_use`.
6. `POST /v1/auth/clerk/link/confirm {link_id}` (session) binds a held,
   unexpired link that this same session of this account made, and uses it up
   → `204`. Gone, expired, already used, or made by another session or account
   → `404 not_found`, and a link made by another session stays held for it;
   bound to another account meanwhile → `409 identity_in_use`. The app asks
   first, naming the browser sign-in's verified email and the account's.
7. Without `CLERK_ISSUER` and `CLERK_CLIENT_ID`, all three routes answer
   `503 clerk_unavailable`. Email codes keep working either way.

Manage Account in Browser (desktop) opens the Clerk Account Portal's profile
page, `https://<accounts host>/user`, where the host is the issuer's with
`clerk.` or `clerk.accounts.` turned into `accounts.`, as Clerk's
`buildAccountsBaseUrl` derives it; the development instance's answered `200`
when checked on 2026-11-01. The app shows it while signed in to an account
with a browser sign-in linked, however this session signed in, and while
`/v1/health` names such an issuer, and hands it to the system as sign-in does.
No Worker route is involved.

## Sync

Entities are `(kind, id)` pairs with an opaque `payload`. The server never parses
payloads. Conflicts resolve last-writer-wins on the HLC string (lexicographic
compare equals clock order).

- `POST /v1/sync/push {changes:[Change], sync_epoch?}` → `PushResponse`
  - `sync_epoch` names the epoch of the device's last pull; absent means 0. Any other value than the account's current epoch → `409 sync_epoch_changed`, and nothing in the request is stored (see [Deleting sync data](#deleting-sync-data)).
  - Up to `MAX_PUSH_CHANGES` (500); each validated with `Change::validate` → `400`/`413` on failure (whole request rejected).
  - For each change: if no row exists or `incoming.hlc > stored.hlc` → `seq += 1`, upsert row with that `seq` and the caller's `device_id`; else add to `stale` with the stored HLC.
  - Response `cursor` = highest `seq` after the push.
  - If anything was accepted, broadcast `ServerEvent::Changed {cursor, by_device}` to every WebSocket of the account.
- `GET /v1/sync/pull?since=<u64>&limit=<n>` → `PullResponse`
  - Rows with `seq > since` ordered by `seq`, at most `min(limit, MAX_PULL_CHANGES)`; `more = true` if rows remain. `cursor` = last returned `seq`, or `since` when empty.
  - Tombstones are returned like other rows (`deleted = true`, `payload = ""`).
  - `sync_epoch` is the account's current epoch (absent from a Worker from before epochs, meaning 0).
- `GET /v1/sync/ws` (WebSocket upgrade, Bearer header) → server sends `ServerEvent::Hello {cursor}` then `Changed` events. Clients pull on `Changed` and reconnect with backoff.

### Deleting sync data

**Deployed 2026-11-01** on the development and production API Workers. A server without the route answers `404`; against one, the desktop app changes nothing and says the server can't delete sync data yet.

- `DELETE /v1/sync/data` (Bearer session) → `DeleteSyncDataResponse {sync_epoch, deleted_rows}`.
  - Deletes every row in the caller's account room: library rows, tombstones and the encryption marker (`e2ee`/`library-v1`). Live rows become 0.
  - Moves `seq` up by one and the account's `sync_epoch` up by one, then broadcasts `Changed {cursor, by_device}`, so every connected device pulls and reads the new epoch.
  - Keeps the account, its devices and sessions, sign-in challenges, AI proxy state and everything else in D1. Only the room of the caller's account is touched.
  - Deleting again is fine: the room stays empty and the epoch moves up again.
- A device whose last pull named an earlier epoch holds deleted data. Its pushes get `409 sync_epoch_changed`. The desktop client also stops when a pull or scan names another epoch, so it never applies the new epoch's rows over the deleted ones or uploads automatically.
- A client from before epochs sends no `sync_epoch`, so after a delete its pushes are refused too. It still pulls, and it can apply rows uploaded in the new epoch.
- The desktop client counts a delete only when the reply's epoch is past the one it held and a readback (`GET /v1/sync/pull?since=0`) names that epoch with no rows. A `4xx` other than `401`, `408` or `429`, or any reply outside this contract, means the server lacks the route; a failed readback is "unconfirmed". In both cases nothing changes on the device.
- The client stores the epoch with its cursor (`sync_epoch` in `kv`). A cursor stored before epochs belongs to epoch 0. From cursor 0 the client uploads nothing until its first pull names the epoch.
- The desktop client keeps the epoch in its local encryption record, also while signed out. On each sign-in and pull, a record older than the server's epoch is retired with its key before anything uploads, so a device that was away during a delete never seals into the new epoch under the old key. After a delete of an encrypted library, the device holds every plaintext upload, to any account, until encryption is on for that account or the user chooses to upload without it.
- The client notes a delete in `kv` before sending it, so after a crash, an unconfirmed readback or a `409` its own delete caused, it reports the delete as its own.

### Deleting the account

Deployed on the development Worker on 2026-11-01 and checked there with a disposable account: a wrong confirmation, a wrong code, the right email with a fresh code, then `401` for the session. Not deployed to production. The desktop app's side is tested in source only. A server without the route answers `404`, and the desktop app says it can't delete accounts yet.

- `DELETE /v1/account {confirm, step_up}` (Bearer session) → `200 DeleteAccountResponse {deleted_rows}`.
  - `confirm` is the account's email as the user typed it; case and surrounding spaces don't matter. Anything else → `400 confirmation_mismatch`.
  - `step_up` shows the user holds the account now, in one of two kinds:
    - `{"kind": "email_code", challenge_id, code}`: a code from `POST /v1/auth/start` for the account's email, started at most 5 minutes ago, and used up here. A wrong code, or one for another email, → `403 invalid_code`, and a wrong code counts as an attempt. Older than 5 minutes or expired → `403 code_expired`; locked → `429 too_many_attempts`.
    - `{"kind": "browser", code, code_verifier, redirect_uri, nonce, device_name, platform}`: a browser sign-in the Worker redeems and checks as at sign-in, of an identity linked to this account, issued at most 5 minutes ago; otherwise `403 step_up_rejected`. A sign-in the provider refuses → `403 sign_in_rejected`. Without Clerk settings, `503 clerk_unavailable`. The device name and platform aren't used.
    - No step-up refusal is a `401`: on this route, `401` means only that the session is gone.
  - Then the account room deletes its rows as [Deleting sync data](#deleting-sync-data) does (library rows, tombstones, the encryption marker; the epoch moves on), and closes every sync socket of the account with `4401`. After that, one D1 batch deletes the account's held links, identities, sessions, devices (revoked ones too) and the account row.
  - Kept: sign-in challenges, which are keyed by email, feed the rate limits and are purged a day after they were made; the AI proxy's per-account daily request counter; and the room's counters. The Clerk user belongs to the provider and is not deleted.
  - Retry: a failure before the D1 batch leaves the session, and a retry, with a new step-up, finishes the delete. Once the batch has run, the session answers `401`. An account id is never reused: signing in again with the same email creates a new, empty account.
- The desktop client counts a delete only when it answers `200` and `GET /v1/me` with the same session then answers `401`. A `401` to the delete itself means the session had ended already. Either way, the app signs out as Sign Out does, removing the stored credential and this account's library key, and keeps the local library. The app sends email-code step-ups, starting the code with the `x-crossbow-dev-key` header as at sign-in when `CROSSBOW_DEV_KEY` is set; browser step-ups are accepted by the Worker but no app control sends one yet.

### Encrypted library rows (desktop source, opt-in)

No server or wire change: the Worker already stores `enc` as given and treats `payload` as opaque text. Its gaps against the target design are listed in [security architecture](security-architecture.md#implemented-source-foundation).

- **Encrypted live rows.** On an account where encryption is on, every live row uses `enc = "xchacha20_v1"`. Its `payload` is `base64url_nopad(0x01 || epoch u32 BE || nonce[24] || ciphertext+tag)`. The AAD binds the internal account id, kind, id, the exact `Hlc::encode()` text and the epoch. Moving a payload to another account, row, clock or epoch fails closed.
- **Marker row.** Kind `e2ee`, id `library-v1`, always sealed. It tells other devices that the account is encrypted.
- **Locked devices.** A device without the key holds its cursor and pushes nothing until the recovery code is entered.
- **Unencrypted parts.** Tombstones keep `enc = "json"` with an empty payload, because the server clears tombstone payloads, so v1 deletions are not authenticated. Kinds, ids, clocks, sizes and timing remain visible to the server.
- **Size limit.** The 64 KiB `MAX_PAYLOAD_BYTES` limit applies to the encoded envelope, so the largest plaintext that can be encrypted is 49,107 bytes.
- **Stability.** Account ids and the HLC text encoding must stay byte-identical for the life of envelope v1, including through the planned Clerk migration (see [security architecture](security-architecture.md#implemented-source-foundation)).

### Profile browsing preferences (desktop source)

The existing `profile` JSON payload now optionally includes `preferences`:

```json
{"name":"Synthetic profile","created_ms":1,"preferences":{"search_engine":"duck_duck_go","auto_archive_hours":24}}
```

- The `preferences` object and each preference field may be absent or `null`; each omitted value independently inherits the legacy shared `Settings` value. The Profile itself must still be a valid object. Reading a legacy profile does not write a migration or materialize defaults.
- Explicit search values use `SearchEngine`'s snake-case JSON names. Explicit archive hours must be one of `1`, `6`, `12`, `24`, `168`, `720`, or `0` (Never). Inherited legacy custom durations are retained without converting them to overrides.
- Local saves change only an existing profile's preference fields. Inside the transaction, the next clock includes the cached clock, persisted library clock and exact target row's clock; even an equal saved payload cannot move its clock backward. Write/commit failure rolls back. Missing/deleted/changed targets or unreadable clocks reject the save.
- Explicit Settings retry refreshes only the retained original profile and its relevant clocks without entity/KV writes, then merges only edited optional fields. It never substitutes the active/default profile. A later payload race still rejects the setter; a later equal-payload clock advance is merged there.
- Unreadable stored policy retains readable identity metadata for display, but its policy is unavailable—not inherited. Unavailable stored IDs also prevent startup from creating replacement profile identities. The invalid payload is not rewritten. Recovery requires a valid same-ID saved record; refresh or accepted sync can then restore usable policy.
- Before applying a remote batch, the desktop simulates sequential per-profile last-writer-wins decisions using stored clocks and earlier page winners, including duplicate IDs and tombstones. Every **live row that can win at its position** must use JSON with a valid object payload and preferences. Obsolete malformed rows cannot block unrelated work or cursor advancement. A malformed intermediate winner still rejects the whole batch before writes, even when a later row would supersede it. A failed sync page keeps its cursor. Unrelated entity handling is unchanged.

These are nonsensitive fields in the current JSON development protocol. Source tests cover payload replay, failure atomicity and library reopen; actual two-device sync, mixed-version behavior and native profile journeys remain unqualified. This is not Clerk migration, encrypted sync, a password vault or a server deployment.

### Durable Object SQLite schema

```sql
CREATE TABLE IF NOT EXISTS entities (
  kind TEXT NOT NULL,
  id TEXT NOT NULL,
  hlc TEXT NOT NULL,
  deleted INTEGER NOT NULL,
  enc TEXT NOT NULL,
  payload TEXT NOT NULL,
  seq INTEGER NOT NULL,
  device_id TEXT NOT NULL,
  PRIMARY KEY (kind, id)
);
CREATE INDEX IF NOT EXISTS entities_seq ON entities(seq);
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v INTEGER NOT NULL); -- k = 'seq', 'live', 'sync_epoch'
```

Quota: 100 000 live rows per account → `413 payload_too_large` once exceeded.

### D1 schema

```sql
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
-- 0002_identities.sql: browser sign-in.
CREATE TABLE identities (issuer TEXT NOT NULL, subject TEXT NOT NULL, account_id TEXT NOT NULL REFERENCES accounts(id),
  created_at INTEGER NOT NULL, PRIMARY KEY (issuer, subject));
CREATE INDEX identities_account ON identities(account_id);
CREATE TABLE identity_links (id TEXT PRIMARY KEY, account_id TEXT NOT NULL, issuer TEXT NOT NULL, subject TEXT NOT NULL,
  email TEXT, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
CREATE INDEX identity_links_expires ON identity_links(expires_at);
```

The daily cron also drops expired `identity_links` rows.

All timestamps are Unix milliseconds.

## AI (plumbing only)

`/v1/ai/*` on the API Worker requires a session, then forwards the request to
the AI Worker through the `AI_WORKER` service binding with the trusted headers
`X-Crossbow-Account` and `X-Crossbow-Device` (any client-supplied copies are
stripped first).

- `GET /v1/ai/health` → `{ok, models:[…], gateway}`
- `POST /v1/ai/complete {messages:[{role, content}], model?}` → streamed text (AI SDK `streamText` + `workers-ai-provider`); 50 requests per account per day.
- The Agents SDK `CrossbowAgent` class (one Durable Object per account) holds future conversation state and tools.

## Errors

Every non-2xx response has the body `{"error":{"code":"…","message":"…"}}` with a
code from `crossbow_proto::error_codes`. Messages never echo secrets or codes.
