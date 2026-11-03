# Crossbow website

A static landing page with a separate Rust Cloudflare Worker for a Clerk-backed waitlist. This does **not** replace or share the desktop browser's account/sync API in [`cloud/api`](../cloud/api). A waitlist entry is not a Crossbow browser account.

**Current state:** source implemented; collection disabled. Native validation tests and the release WebAssembly build pass. No deployment, DNS change, Clerk app setup, Turnstile widget setup, secret provisioning, real enrollment or provider read-back has been performed as part of this implementation. Do not describe the waitlist as collecting addresses until those journeys pass.

## Layout and local development

- `public/`: landing HTML, styles and browser JavaScript.
- `worker/`: standalone `crossbow-site` Cargo workspace, using `worker = 0.8.7`.
- `worker/wrangler.toml`: preview-only `crossbow-site-dev`, static `ASSETS` binding, and `WAITLIST_RATE_LIMITER`. No custom-domain routes or production environment are enabled.
- `worker/build/`: generated Workers JavaScript wrapper and Rust Wasm; ignored by Git. There is no Node backend or React application.

Requirements: Rust, the `wasm32-unknown-unknown` target, `worker-build` 0.8.7, and Wrangler 4.36.0 or later (4.135.0 used for tooling checks). Run from this repository:

```sh
cargo test --manifest-path website/worker/Cargo.toml --locked
worker-build --release "$PWD/website/worker" -- --locked
wrangler --cwd website/worker dev \
  --local --ip 127.0.0.1 --port 8798 \
  --var SITE_ORIGIN:http://127.0.0.1:8798
```

Use **`--cwd website/worker`**, not just `--config website/worker/wrangler.toml`: Wrangler 4.135.0 resolves the custom build's working directory from the invocation directory, so `--config` alone tries to build the repository's root workspace and fails with `missing field package`. The corrected command runs the build inside the standalone Worker crate. An absolute `--cwd` path works when invoking it from another directory.

Open `http://127.0.0.1:8798` in a browser. Do not use `--remote` for offline verification. The website's disabled state is intentional, not a simulated successful enrollment. The local override is an exact origin: no trailing slash. The production domain in the checked-in vars is not a route or evidence of a deployment.

The Worker runs **before every asset**, so a cached asset cannot bypass the legacy-domain redirect or security headers. `html_handling = "auto-trailing-slash"` serves `/privacy` from `public/privacy.html`. `not_found_handling = "404-page"` would serve a `public/404.html` for unknown asset paths with HTTP 404, not an SPA fallback. There is no such file, so unknown asset paths get HTTP 404 from the assets binding with no HTML page of their own, never the landing page; add `public/404.html` for an HTML not-found page. Unknown API paths get a JSON 404 and never fall back to landing HTML.

## Browser contract

### `GET /api/config`

Returns only:

```json
{"enabled":false,"turnstileSiteKey":null,"privacyContact":null}
```

When correctly enabled on the configured origin, `enabled` is `true` and `turnstileSiteKey` is the public widget key. `privacyContact` is the validated public email address configured by the maintainers, or null while none is configured. A configured contact remains available even when enrollment is switched off, so existing entrants can still request removal. The secret key, Clerk key, provider errors and account details never appear here. Responses use `Cache-Control: no-store`.

The gate requires **all** of the following:

1. `WAITLIST_ENABLED` is exactly `true`.
2. `ENVIRONMENT` is `preview` with a `sk_test_` Clerk key, or `production` with a `sk_live_` Clerk key. Unknown environments fail closed.
3. `SITE_ORIGIN` is an exact HTTPS origin. HTTP loopback is permitted only for preview testing; production requires HTTPS. The redirect-only hosts cannot be the collection origin.
4. Nonempty `TURNSTILE_SITE_KEY`, `TURNSTILE_SECRET_KEY` and `CLERK_SECRET_KEY`, with valid configuration syntax. Cloudflare's published dummy challenge keys are explicitly rejected; a dummy key must never turn real collection on.
5. The `WAITLIST_RATE_LIMITER` binding exists.
6. The request's URL origin exactly matches `SITE_ORIGIN`.
7. `PRIVACY_CONTACT` is a valid configured email address for access, correction and removal requests. No inbox is selected in the checked-in config. Do not invent an address or use a maintainer's personal address; the maintainers must choose a monitored, verified contact before activation.

This is a **configuration gate**, not an upstream health check or mailbox-delivery verification. Syntactically valid but revoked credentials can still make the form visible; an actual enrollment will return an honest retryable error, not success. Contact reachability must be verified separately before setting its value. Missing/disabled configuration, including a missing or invalid privacy contact, produces HTTP 503 on submission, with `Retry-After: 60`.

### `POST /api/waitlist`

```json
{
  "email":"person@example.com",
  "turnstileToken":"a-fresh-widget-token",
  "website":""
}
```

The frontend must set widget `action` to **`waitlist`**, use same-origin fetch, submit JSON and obtain a fresh challenge after any failed/ambiguous attempt. Do not retry a consumed token automatically. Turnstile tokens expire after five minutes and are single-use.

Validation and protections:

- The entire JSON body is limited to **4,096 bytes**, checked against any length header and again while streaming. Body reads time out after ten seconds.
- `Content-Type` must be `application/json`, optionally `charset=utf-8`. Other encodings are not supported. The three fields are required strings; unknown or duplicate fields and malformed JSON are rejected.
- Email input is at most **254 ASCII bytes**, with a local part of at most **64 bytes** and DNS labels of at most **63 bytes**. Plain surrounding spaces are trimmed, local-part case is preserved, and the domain is lowercased. Quoted local parts, address literals and Unicode email addresses are intentionally not supported; ASCII/Punycode domains are accepted. The frontend must not claim that all RFC-valid email syntax is supported.
- The token is **1–2,048 ASCII graphic bytes**. `website` is the honeypot and must be empty. A filled honeypot is rejected; it never produces fake enrollment success.
- The request URL must match the configured scheme/host/port. `Origin` is mandatory and must exactly equal `SITE_ORIGIN`. When present, `Sec-Fetch-Site` must be `same-origin` (not merely `same-site`). No cross-origin CORS access or OPTIONS preflight is enabled.
- Cloudflare's `CF-Connecting-IP` must parse as an IP address; there is no `X-Forwarded-For` or user-supplied-body fallback. A missing IP or unavailable limiter returns 503.
- The configured limiter allows approximately **5 attempts per IP per 60 seconds per Cloudflare location**. It runs before body parsing and provider calls; exhaustion returns 429 with `Retry-After: 60`. This Cloudflare limiter is eventually consistent, not a strict global quota or a complete DDoS defense. Confirm the self-selected namespace ID is unused before deploying; production needs a different ID.
- Turnstile verification must report `success: true`, the **exact configured hostname**, and `action: "waitlist"`. A successful challenge for another host/action is rejected.
- Provider requests use fixed HTTPS URLs, refuse redirects, cap response reads at **16 KiB**, and time out after **10 seconds each**. Error bodies are never returned or logged. Provider 429/5xx, credential errors, malformed success responses and network failures remain retryable 503 failures.
- Only a confirmed Clerk 200/201 response containing a nonempty entry ID and the matching email address produces success. There is no local storage fallback or client-side success simulation.

Success, including an existing entry:

```json
{"ok":true,"message":"You're on the waitlist. Thanks for your interest in Crossbow."}
```

Failure:

```json
{"ok":false,"message":"The waitlist is not available right now. Please try again later."}
```

Failures use the appropriate 400/403/405/408/413/415/429/503 status. Invalid tokens and challenge hostname/action mismatches return 400; same-origin violations on an enabled origin return 403. A request to the wrong configured host remains disabled and returns 503. Unknown API paths return 404. Public messages contain no submitted input or provider response details.

## Verified Clerk HTTP contract

The JavaScript SDK's `waitlistEntries.create({emailAddress, notify})` calls `POST /waitlist_entries`. Its request layer converts SDK camelCase body keys to snake_case and adds API version `v1`. The Rust Worker therefore sends:

```http
POST https://api.clerk.com/v1/waitlist_entries
Authorization: Bearer <dedicated waitlist app secret>
Content-Type: application/json
```

```json
{"email_address":"person@example.com","notify":false}
```

`notify` defaults to true in Clerk; this Worker deliberately sets **false**. Enrollment does not send a confirmation email. This limits unwanted mail and makes retries safer; the UI must not promise a message has just been sent. Existing addresses return the existing entry, so successful responses do not reveal whether someone had already joined. The Worker does not convert an arbitrary 409 into success.

Official sources checked during implementation:

- [Clerk create documentation](https://clerk.com/docs/reference/backend/waitlist-entries/create)
- [Official SDK endpoint and parameters](https://github.com/clerk/javascript/blob/main/packages/backend/src/api/endpoints/WaitlistEntryApi.ts)
- [Official SDK HTTP serialization](https://github.com/clerk/javascript/blob/main/packages/backend/src/api/request.ts)
- [Official SDK API URL/version](https://github.com/clerk/javascript/blob/main/packages/backend/src/constants.ts)
- [Official waitlist resource response](https://github.com/clerk/javascript/blob/main/packages/backend/src/api/resources/WaitlistEntry.ts)
- [Clerk access modes](https://clerk.com/docs/guides/secure/restricting-access)
- [Turnstile server validation](https://developers.cloudflare.com/turnstile/get-started/server-side-validation/)
- [Turnstile testing keys](https://developers.cloudflare.com/turnstile/troubleshooting/testing/)
- [Cloudflare rate-limiting binding](https://developers.cloudflare.com/workers/runtime-apis/bindings/rate-limit/)

The direct Clerk BAPI URL linked from the create guide returned 404 during the check; the endpoint, version and wire field names were cross-checked against the official SDK source instead. These source checks are not evidence of a successful provider write.

## Authorized activation checklist — not yet performed

Do not run account mutations, secret writes, a deployment or DNS changes without the maintainers' explicit go-live approval. Do not reuse the browser sync API's secrets or repurpose its authentication.

1. Create or select a **dedicated Crossbow waitlist Clerk application**. Keep its development and production instances separate from browser auth and from other products. Set its **Access mode to Waitlist** and save. Do not automatically approve/invite entrants. Enable Email only if the maintainers later want Clerk's invitation flow; enrollment notifications remain off in this integration.
2. Create a managed Turnstile widget restricted to the exact approved hostname(s). Use a separate widget for localhost/preview if necessary. The frontend uses the public site key and `action: waitlist`; the secret exists only in the Worker. The five published dummy sitekeys and three dummy secrets cannot enable collection.
3. Set the exact preview `SITE_ORIGIN`, preview site key, dedicated **test** Clerk secret, and a maintainer-approved, verified `PRIVACY_CONTACT` inbox for removal requests. This work does not select an inbox or create email routing. Keep `WAITLIST_ENABLED=false` until the setup is ready for a deliberately authorized provider test. Production must use its own Clerk live instance, widget, rate-limit namespace and `crossbow-site` Worker name.
4. Provision secrets through **stdin**, and back them up only to the verified Crossbow GitHub repository's encrypted Actions secrets. Do not paste secret values into commands, committed files, logs or responses. The following is an example for an **authorized Bash session only**, after checking the active Cloudflare account and exact GitHub repo. It is not an instruction to provision them during development:

   ```sh
   # Set CROSSBOW_REPO to your verified <org>/<repo> Crossbow repository first.
   # Enter the dedicated preview Clerk key at the silent prompt.
   read -r -s CLERK_SECRET_KEY
   printf '%s' "$CLERK_SECRET_KEY" | wrangler secret put CLERK_SECRET_KEY \
     --config website/worker/wrangler.toml
   printf '%s' "$CLERK_SECRET_KEY" | gh secret set CROSSBOW_WAITLIST_PREVIEW_CLERK_SECRET_KEY \
     --repo "$CROSSBOW_REPO"
   unset CLERK_SECRET_KEY

   read -r -s TURNSTILE_SECRET_KEY
   printf '%s' "$TURNSTILE_SECRET_KEY" | wrangler secret put TURNSTILE_SECRET_KEY \
     --config website/worker/wrangler.toml
   printf '%s' "$TURNSTILE_SECRET_KEY" | gh secret set CROSSBOW_WAITLIST_PREVIEW_TURNSTILE_SECRET_KEY \
     --repo "$CROSSBOW_REPO"
   unset TURNSTILE_SECRET_KEY
   ```

   Secret writes are external changes and can create/modify a Worker; approval is required even if no page deploy follows. GitHub Actions secrets are write-only backup inputs for automation, not a way to retrieve plaintext later. Keep a maintainer-controlled credential-manager copy. A separately approved production configuration needs its own explicit environment flag and production-named GitHub secrets; never run the preview commands against production credentials.
5. Test a real challenge and a disposable `@example.com` identity in the dedicated test instance. Read the resulting entry in Clerk, reload the provider view, retry the same email with a **fresh** challenge, and confirm one persistent entry and the same neutral response. Verify no enrollment email was sent. Exercise invalid/expired token, wrong hostname/action, failed provider, revoked key, rate limit and retry states with isolated fixtures.
6. Only after approval, add a production environment and domain routes. The intended canonical landing host is `crossbow.zone`. Its DNS zone was reported active; that does not mean this website is deployed. Whether `legacy.example` is held and available for this site still needs an exact registrar check. No redirect is live merely because the Worker contains a redirect handler.
7. Verify the exact production build over the real HTTPS hostname, then re-read Clerk. Confirm the UI, API, provider instance and deployed version agree. Turn collection off again if any acceptance gate fails. Never report provider activation or live collection from a local build alone.

## Legacy-domain behavior

When routed to this Worker after approval, `GET` and `HEAD` requests for `legacy.example` or `www.legacy.example` return **308** to `https://crossbow.zone`, preserving the URL path and query. The target scheme and authority are fixed, even for a path beginning `//` or an encoded external URL. Unknown legacy paths stay unknown on the destination rather than silently becoming the landing page.

`POST`, `PUT`, `OPTIONS` and all other methods on these hosts return **405**, never a redirect. This avoids forwarding an email/token-bearing submission to another origin. The redirect runs before asset lookup; there is no open redirect parameter.

The deployed custom routes are **intentionally absent** from the checked-in Wrangler config. Before go-live, confirm control of both legacy hosts and approve their DNS/custom-domain changes explicitly.

## Privacy and security headers

The Worker code logs no email, token, IP, secret or provider body; Workers observability is disabled in the checked-in config. Rate-limit keys contain the edge-supplied IP solely for Cloudflare's transient counters. A successful submission sends the email to Clerk and the challenge/token plus client IP to Cloudflare Turnstile. No browser account or user library is read.

Cloudflare/Clerk may retain their own provider operational data; verify the maintainers' requirements and retention settings before collecting real users. Display the appropriate privacy disclosure before activation. Enrollment does not verify that the entrant controls the address, and is not a subscription payment or browser account creation.

Responses set `nosniff`, no-referrer, frame denial, a restrictive permissions policy and CSP. HTTPS responses also set HSTS for the individual host (no `includeSubDomains` or preload). CSP permits same-origin scripts and fonts plus Cloudflare's Turnstile script/frame/connect host; no Google Fonts or other third-party asset hosts are permitted. Inline styles remain allowed for widget/layout behavior, but inline scripts are not. Keep application JavaScript and fonts in same-origin files; do not weaken CSP to paper over a broken widget.

## Verification boundary

Observed in the implementation environment:

- **20 native tests pass:** enablement/secret/privacy-contact boundaries, minimal public config with removal contact while closed, dummy keys, exact origins, same-origin metadata, redirects, size/content-type enforcement, strict JSON, honeypot, email/token limits, Clerk HTTP fields and duplicate-neutral success, malformed/provider errors, Turnstile hostname/action/error checks.
- **Release Wasm build passes** using `worker-build 0.8.7` and the locked standalone Cargo dependencies.
- **Wrangler 4.135.0 dry-run bundle passes** with `--cwd website/worker`: the custom build runs in the standalone crate, static assets resolve correctly, and `ASSETS` plus the 5/60s rate-limit binding are recognized. The first local invocation using only `--config` failed before startup; the corrected working-directory invocation is documented above. The dry run exited without uploading or deploying and did not start a local listener.
- **Not verified by those checks:** rendered browser behavior, active local Wrangler routes, real Turnstile challenge completion, Clerk writes and persistent duplicate handling, email behavior in the chosen Clerk app, public DNS, TLS, the domain redirects or live deployment. Record those observations separately after performing them; do not infer them from unit tests.

Useful local smoke checks with collection still disabled:

```sh
curl -i http://127.0.0.1:8798/api/config
# 200; enabled:false, turnstileSiteKey:null, privacyContact:null, Cache-Control:no-store.

curl -i http://127.0.0.1:8798/privacy
# 200 HTML from privacy.html, not the landing page.

curl -i http://127.0.0.1:8798/missing-page
# 404 with no HTML page of its own (public/ has no 404.html), never 200 landing HTML.

curl -i http://127.0.0.1:8798/api/waitlist \
  -H 'Origin: http://127.0.0.1:8798' \
  -H 'Content-Type: application/json' \
  --data '{"email":"fixture@example.com","turnstileToken":"fixture","website":""}'
# 503, ok:false, Retry-After:60; never a fake signup.

curl -i 'http://127.0.0.1:8798/path?from=fixture' -H 'Host: legacy.example'
# 308, Location:https://crossbow.zone/path?from=fixture.

curl -i -X POST http://127.0.0.1:8798/api/waitlist -H 'Host: legacy.example'
# 405, Allow:GET, HEAD; no Location header or provider call.
```

For the final browser journey, inspect the normal and compact layout, keyboard focus, validation, disabled configuration, in-flight submission, success, challenge reset/retry and reopened state. Provider read-back must be done in the correct dedicated Clerk instance; a success banner alone is insufficient. Keep real addresses and challenge values out of captured evidence.
