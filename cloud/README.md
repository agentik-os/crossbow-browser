# Crossbow cloud

**Architecture transition:** this document describes the current custom-auth, JSON-sync development backend. The target is Clerk browser identity and end-to-end-encrypted library/password synchronization, as specified in [security-architecture.md](../docs/security-architecture.md). That migration and password vault sync are not implemented; the desktop app's local password vault is a preview (see the [README](../README.md)). Do not describe the existing opaque-payload API as encrypted storage. Newer desktop source can opt in to sealing library rows as `xchacha20_v1` without any Worker change ([sync protocol](../docs/sync-protocol.md#encrypted-library-rows-desktop-source-opt-in)); it has not been run against this Worker, locally or deployed.

The Cloudflare backend for Crossbow: accounts, devices, sync and an AI plumbing
endpoint. The contract is [`docs/sync-protocol.md`](../docs/sync-protocol.md). The
wire types live in [`crates/crossbow-proto`](../crates/crossbow-proto). The API
Worker and the e2e CLI both depend on that crate by path, so the client and server
cannot drift apart.

```
desktop app ──HTTPS / WSS──▶ API Worker (Rust, workers-rs 0.8.7, *.workers.dev)
                               ├─ D1: accounts, devices, sessions, auth_challenges
                               ├─ SyncRoom Durable Object (SQLite), one per account
                               │    entity store + seq counter + WebSocket fan-out (Hibernation API)
                               ├─ Resend (sign-in codes, prod only)
                               └─ AI_WORKER service binding ──▶ AI Worker (TypeScript, not public)
                                                                  ├─ CrossbowAgent (Agents SDK DO, one per account)
                                                                  └─ Workers AI binding `AI` via AI SDK + workers-ai-provider
```

| Directory | What |
|---|---|
| `api/` | API Worker. `src/logic/` contains pure Rust (validation, hashing, LWW push planner, limits, token format, pull paging) with native unit tests. The other modules (`entry`, `auth`, `db`, `sync`, `room`, `mail`, `ai_proxy`) compile only for wasm32. Migrations are in `api/migrations/`. |
| `ai/` | AI Worker: Agents SDK `CrossbowAgent`, AI SDK `streamText`, `workers-ai-provider`. `src/logic.ts` is pure and tested with `npm test`. |
| `e2e/` | `crossbow-e2e`: a Rust CLI that runs the whole user journey against a deployed environment. |

## Environments

In each `wrangler.toml`, the top level is **dev** and `--env prod` is **prod**. The API Worker is served from `*.workers.dev`, with no routes or custom domains; the AI Worker (`workers_dev = false`) has no public URL at all.

| | dev | prod |
|---|---|---|
| `ENVIRONMENT` | `dev` | `prod` |
| Sign-in codes | returned in the response only to a caller whose `x-crossbow-dev-key` header matches the `DEV_CODE_KEY` secret; then nothing is mailed | mailed through Resend (`RESEND_API_KEY` secret, `MAIL_FROM` var); never returned |
| Browser sign-in | on when the `CLERK_ISSUER` and `CLERK_CLIENT_ID` vars are set; otherwise its routes answer `503 clerk_unavailable` | same |
| Cron | `17 3 * * *` purges challenges older than 24 h and expired sessions | same |

## Routes

| Route | Notes |
|---|---|
| `GET /v1/health` | `HealthResponse` |
| `POST /v1/auth/start` | email → challenge, 6-digit code. Limits: 5 per email per hour and 30 per IP per hour. |
| `POST /v1/auth/verify` | code → `cbs_` session token, a new device, and the account (created on first sign-in) |
| `POST /v1/auth/clerk`, `POST /v1/auth/clerk/link`, `POST /v1/auth/clerk/link/confirm` | browser sign-in, and linking it to a signed-in account; see [Browser sign-in](../docs/sync-protocol.md#browser-sign-in-clerk) |
| `POST /v1/auth/logout` | deletes the session and closes that session's WebSockets |
| `GET /v1/me` | account, current device, active devices |
| `DELETE /v1/devices/{id}` | revokes the device, deletes its sessions, closes its sockets (close code `4401`) |
| `POST /v1/sync/push` | LWW on the HLC string, up to 500 changes, atomic per push |
| `GET /v1/sync/pull?since=&limit=` | ordered by `seq`, up to 1000 rows |
| `DELETE /v1/sync/data` | deletes the account's synced library and moves its sync epoch; see [Deleting sync data](../docs/sync-protocol.md#deleting-sync-data) |
| `DELETE /v1/account` | deletes the account after a fresh step-up; see [Deleting the account](../docs/sync-protocol.md#deleting-the-account) |
| `GET /v1/sync/ws` | WebSocket with a `Bearer` header. Sends `Hello{cursor}` and then `Changed{cursor, by_device}`. Text `ping` → `pong`, answered by the runtime without waking the room. |
| `/v1/ai/health`, `POST /v1/ai/complete` | proxied to the AI Worker with trusted `X-Crossbow-Account` / `X-Crossbow-Device`. Client copies of `X-Crossbow-*`, `Authorization` and `Cookie` are stripped. |

Every error body is `{"error":{"code","message"}}` and uses a code from `crossbow_proto::error_codes`.

### Additions to the spec

None of these changes the wire shape of an existing field.

- **SyncRoom meta**: `meta` also stores `k = 'live'`, the live (non-tombstone) row count. The 100 000-row quota uses it, so a push never needs a full `COUNT(*)`. A push is rejected with `413` only if it would *grow* the live set past the quota. Updates and deletes still work at the limit.
- **Pull byte budget**: a pull page also stops at about 4 MiB of payload, with `more = true`, and always returns at least one row. This keeps 1000 × 64 KiB pages from exhausting Worker memory. The spec already says "at most".
- **Tombstones**: the server clears a tombstone's `payload` when storing it, even if the client sent one.
- **Extra index**: D1 has `devices_account(account_id, created_at)`.
- **AI health**: `GET /v1/ai/health` also returns `account`, the id the AI Worker received. The e2e uses it to prove that a spoofed header is replaced.
- **Verify error mapping**: an unknown or already-used challenge returns `401 invalid_code`. A malformed code counts as a wrong code and uses an attempt. The first 5 wrong codes return `401 invalid_code`; every call after that returns `429 too_many_attempts`.
- **Upgrade header**: `GET /v1/sync/ws` without `Upgrade: websocket` returns `400 bad_request`.

## AI Worker

- Model: `@cf/meta/llama-4-scout-17b-16e-instruct`, checked in the Workers AI catalog on 2026-10-23. It is non-reasoning, has a 131k context window and costs $0.27/M input and $0.85/M output. `model` must be in the allowlist in `ai/src/logic.ts`.
- Quota: 50 `complete` requests per account per UTC day, counted in the account's `CrossbowAgent` (SQLite) over DO RPC. Over quota returns `429 rate_limited`. Output is capped at 1024 tokens.
- AI Gateway: set the `AI_GATEWAY_ID` var to route through a gateway. If it is empty, the Worker calls Workers AI directly.
- Trust: the AI Worker accepts only an `X-Crossbow-Account` of the form `acc_<32 hex>`, injected by the API Worker. It has no public URL.
- **Stream shim**: Workers AI now sends the same text in both `response` and `choices[0].delta.content` in each SSE event. Every `workers-ai-provider` release up to 4.0.1 emits both, so every token appeared twice. `dedupingBinding` in `ai/src/index.ts` drops `response` when `delta.content` is present (`dedupeSseLine` in `logic.ts`, unit tested). Remove the shim once the provider handles this.

Pinned npm versions: `agents 0.30.0`, `ai 7.0.150`, `workers-ai-provider 4.0.1`, `typescript 5.9.3`. Wrangler is the global one (4.135).

## Build and test

```sh
# API Worker: pure logic tests (native), then the wasm build
cd cloud/api && cargo test
rustup target add wasm32-unknown-unknown && cargo install worker-build --version 0.8.7 --locked
worker-build --release            # wrangler deploy runs this too

# AI Worker
cd cloud/ai && npm ci && npm test && npm run typecheck
```

`npm run typecheck` runs `wrangler types`, which reads `cloud/ai/wrangler.toml`. In a fresh clone, copy it from `wrangler.example.toml` first (step 3 below).

Gotcha: do not set `strip = true` in `api/Cargo.toml`'s release profile. With it, wasm-bindgen 0.2.129 fails with "externref table required for catch wrappers".

## Deploy your own

1. Create a D1 database for each environment with `wrangler d1 create <name>`.
2. Copy `cloud/api/wrangler.example.toml` to `cloud/api/wrangler.toml` and replace every `<your-...>` value with your own Worker names, D1 names and ids, and mail sender. Keep the bindings, vars and migrations as they are: the Worker's code reads them by these names.
3. Copy `cloud/ai/wrangler.example.toml` to `cloud/ai/wrangler.toml` and replace its two `<your-...>` Worker names. The API Worker's `AI_WORKER` services (`<your-dev-ai-worker-name>` and `<your-ai-worker-name>` in its file) must name these same Workers.
4. Browser sign-in is optional. To offer it, set `CLERK_ISSUER` to your Clerk instance's issuer URL and `CLERK_CLIENT_ID` to a public OAuth client that uses PKCE, and register the loopback redirect URIs listed in [the security architecture](../docs/security-architecture.md#provider-configuration-2026-11-01). The app reads both values from `GET /v1/health`.

Deploy the AI Worker first, because the API Worker's service binding needs it to exist.

```sh
# dev
(cd cloud/ai  && wrangler deploy --env "")
(cd cloud/api && wrangler d1 migrations apply <your-dev-d1-database-name> --remote --env "" && wrangler deploy --env "")
(cd cloud/api && wrangler secret put AUTH_PEPPER --env "")
(cd cloud/api && wrangler secret put DEV_CODE_KEY --env "")    # optional, see below

# prod
(cd cloud/ai  && wrangler deploy --env prod)
(cd cloud/api && wrangler d1 migrations apply <your-d1-database-name> --remote --env prod && wrangler deploy --env prod)
(cd cloud/api && wrangler secret put AUTH_PEPPER --env prod)
(cd cloud/api && wrangler secret put RESEND_API_KEY --env prod)
```

## Secrets (names only)

| Secret | Environment | Purpose |
|---|---|---|
| `AUTH_PEPPER` | both | A long random value, such as 32 random bytes in base64url, mixed into the stored hash of each sign-in code. |
| `DEV_CODE_KEY` | dev, optional | A caller that sends it as `x-crossbow-dev-key` gets the sign-in code in the response. Test tools and the desktop app read it from `CROSSBOW_DEV_KEY`. Without it, dev returns no codes. |
| `RESEND_API_KEY` | prod | Sends sign-in codes through Resend. Without it, prod answers `503 mail_unavailable`. |

Rotating a pepper invalidates every outstanding sign-in code, but sessions keep working because tokens are hashed without the pepper.

## End-to-end check

```sh
cd cloud/e2e && cargo build --release
CROSSBOW_DEV_KEY="<dev key>" ./target/release/crossbow-e2e --base "<dev Worker URL>"
```

The CLI prints `PASS`/`FAIL` for each of 18 steps and exits non-zero if any step fails. The steps are:

1. health
2. auth start with the dev key
3. wrong code returns `401 invalid_code`
4. right code
5. `/v1/me`
6. second device
7. WebSocket `Hello`
8. push
9. `Changed`
10. pull
11. stale push
12. tombstone
13. `/v1/ai/*` without a session returns 401
14. AI health with a spoofed header
15. streamed completion
16. revoking an unknown device returns 404
17. revoking device B makes B's token 401 and closes its socket
18. logout makes A's token 401

It runs only against a dev Worker and needs that Worker's `DEV_CODE_KEY` in `CROSSBOW_DEV_KEY`, so set that secret on your own dev Worker first. Each run creates one account with a random `@example.com` address and uses 2 of the 30 sign-in starts allowed per IP per hour.

`--proxy http://<host>:<port>` sends every request, the WebSocket included, through an HTTP CONNECT proxy, for networks that reset TLS to some Cloudflare edge IPs.

## Limits and known gaps

- Rate limits count rows in D1. Concurrent bursts can go slightly over a limit, which is acceptable for sign-in throttling.
- No CORS: the client is the native app.
- The AI daily quota (429 after 50) and the 100 000-row quota have unit tests only; neither has been exercised live.
- `CrossbowAgent` holds only the usage counter. Conversation state and tools come later.
