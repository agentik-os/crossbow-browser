# Crossbow

Crossbow is a desktop web browser written in Rust, with Arc-inspired workflows and its own identity: a vertical sidebar of Spaces and profiles, favorites, pinned and Today tabs, folders, split view, Peek and a command bar. Its interface is drawn with egui, and pages are rendered by Chromium through CEF. macOS is the first development target; Windows and Linux are targets, not yet verified releases.

**Status: development prototype, version 0.1.0.** This is not a production browser or a claim of complete Arc parity. Much of what follows is built and tested in source only; some journeys were exercised in ad-hoc signed macOS development builds. [Feature status](docs/feature-status.md) records, area by area, what was observed, what is blocked and what is outstanding, and the [Arc Settings parity table](docs/arc-settings-parity.md) compares Settings option by option.

## What works

**Source only** below means built and tested in source (unit, SQLite, simulated-frame or stand-in-page tests), not in the running app. A journey described as exercised natively ran in an ad-hoc signed macOS development build, within the limits given in feature status.

- **Browsing.** Spaces with per-Space themes, profiles, favorites, pinned and Today tabs, folders, archive, split view, Peek, find on page, the command bar with search engines and site search, and the Library are in source. Native development builds exercised parts of tab close, restore and archive, per-profile history, split-pane removal, Library and Find input, and links opened in Little Windows; Peek's native qualification is unfinished. Folder menus are tested with simulated input, and moving a Space to another profile, split persistence, per-site profile rules and search suggestions are source only.
- **Content blocking (source tests and one development-build run).** Per-profile **Block ads and trackers** and **Block cookie banners**, both off by default, with EasyList and EasyPrivacy built in unmodified (version 202611010524, updated only with app releases), lists added from files, element hiding and per-site exceptions. Tested in source with simulated requests and stand-in pages, and once in a development build against a loopback fixture page, where the shield counted each blocked request twice (since fixed in source, not yet rerun); not against real pages, and no independent review yet.
- **Passwords, addresses and cards (preview; source tests and development-build runs).** A local vault in Settings → Passwords, sealed with XChaCha20-Poly1305 under a key in the macOS Keychain, with website login saving (off by default) and filling on request, saved addresses and saved cards. **No external security review; use test passwords only.** Tested in source with an in-memory key store, stand-in pages and simulated frames, and in two development builds against loopback fixture pages with a test key file; not against the real Keychain, the real clipboard or real pages, and with no external security review.
- **Local AI (source only).** Settings → AI Providers detects Claude Code, Codex, Gemini CLI, OpenCode, Ollama, LM Studio and llama.cpp without launching them, and uses as its model only a user-started OpenAI-compatible server on `127.0.0.1`. With it come **Ask AI**, a prompt-only, session-only chat, and browser features that are each off by default: **Translate Pages**, **5-Second Previews**, Ask on Page, Ask AI in the command bar, and Tidy Downloads, Tab Titles and Today. They have been tested only with fake providers and test pages; no real model has run them and they haven't been used in the running app. CLI adapters stay disabled.
- **Account and sync.** Email-code sign-in to a Rust Cloudflare Worker that syncs the library as JSON: **sync is not end-to-end encrypted**. Browser sign-in with Clerk is offered only by the development Worker and is tested only in source against loopback fixtures. **Delete Sync Data…** is source only in the app; its server route is deployed and passed a scripted check on the development Worker, where the 18-step e2e harness also passed, but hasn't been run from the desktop app against a deployed Worker. **Delete Account…** is deployed on the development Worker only, where it was checked with a disposable account; the app's side is source only, and it doesn't delete your Clerk user. An **opt-in foundation** for end-to-end-encrypted library rows is source only, tested with an in-process fake server and an in-memory key store; it has no native, real-Keychain, deployed-Worker, two-device or external-review evidence.
- **Default-browser link handling.** Links from other apps and launch arguments open in Crossbow (`http`, `https`, HTML `file` and `crossbow` only), routed by Settings → Links rules to a Space, or to a Little Window. A native development build received and routed links with synthetic data; the rule editor, bringing the window forward and the real default-browser query weren't checked natively. Crossbow shows whether it is the default browser but never changes it.
- **Downloads.** A per-profile folder and **Ask where to save each file**, and **Pause / Resume / Cancel** when Chromium supplies controls, are source only. The native chooser, file output, interrupted transfers, real transfer bytes and restart are unverified; download history and restart-resume aren't implemented.
- **Personalization.** Space palettes, custom gradients and Grain, Sand, Denim and Tweed textures; an emoji or Lucide icon per Space; Website appearance; transparent chrome; and an app icon picker (Black, Graphite, Pearl or a custom PNG). The icon picker was exercised in native development builds with Dock readback. The native emoji and icon picker journey and Website appearance (source only) haven't run natively, and real desktop compositing of transparent chrome is unverified.

Also in source, each with its status in [feature status](docs/feature-status.md): Little Windows, Picture in Picture, Capture, Boosts, site permissions, links to other apps, customizable shortcuts, Warn before quitting, Arc and Chrome import, and library backups with Restore Data.

## Build and run on macOS

Requirements: Rust with edition 2024 support, Xcode Command Line Tools, and the CEF distribution matching the pinned `cef` crate. The commands below assume it is installed under `~/.local/share/cef`.

```sh
CEF_PATH="$HOME/.local/share/cef" cargo xtask bundle
```

The development build is produced at:

```text
target/debug/bundle/Crossbow.app
```

To preserve an already-running bundle, create a separate named candidate:

```sh
CEF_PATH="$HOME/.local/share/cef" cargo xtask bundle --candidate test-1
```

This writes `target/debug/candidate-test-1/Crossbow.app` and refuses to overwrite an existing candidate. It does not install the app or change the running bundle. Use a new label for new bytes.

Run an isolated profile rather than the normal browsing library:

```sh
CROSSBOW_DATA_DIR="$HOME/Library/Application Support/Crossbow-QA" \
CROSSBOW_TOKEN_FILE=1 \
CROSSBOW_API_URL="<dev Worker URL>" \
CROSSBOW_DEV_KEY="<dev key>" \
CROSSBOW_IMPORT_FIXTURES="$PWD/app/tests/fixtures/import" \
  target/debug/bundle/Crossbow.app/Contents/MacOS/Crossbow
```

Each new build is ad-hoc signed, so macOS treats it as a new app. Before Chromium loads any page it reads the login-Keychain item "Chromium Safe Storage", and a new build waits on a Keychain prompt: every tab stays blank with a spinner, and nothing is logged, until the prompt is answered. For isolated QA runs, add Chromium's own switch, which Crossbow passes through to the engine:

```sh
open -n -a target/debug/candidate-test-1/Crossbow.app \
  --env CROSSBOW_DATA_DIR="$HOME/Library/Application Support/Crossbow-QA" --args --use-mock-keychain
```

The mock Keychain encrypts cookies with a fixed test key, so use it only with synthetic data. Cookies saved under the real Keychain key can't be read in such a run. Only a stable signed identity removes the prompt for everyday use.

The development API sends no email. Started with `CROSSBOW_DEV_KEY` set to its key, the app shows a test code instead; the key goes only to `CROSSBOW_API_URL`, never to a server typed in Settings. Use a disposable `@example.com` identity for QA; do not merge a real account into test data. File token storage is an explicit isolated-QA option for save, load and sign-out; it keeps an account-bound credential under the selected data directory, with mode `0600` on Unix. It is **never** a fallback after denied or unavailable OS-keychain access. Never commit or print that file. A credential-storage failure prevents library transfer and leaves sign-in incomplete. Permanent macOS permission persistence still needs a stable signed installation and its own native verification. To run your own sync server, see [cloud/README.md](cloud/README.md).

`CROSSBOW_IMPORT_FIXTURES` selects synthetic `Arc/` and `Chrome/Bookmarks` sources. When set, missing fixtures must not fall back to the user's real browser data. Unset it only for an intentional real import. Import reads source files without modifying them; passwords, cookies and extensions are excluded.

A build has a sync server only if `CROSSBOW_API_URL` is set when it is compiled: `cargo xtask bundle` reads it from the environment or from a git-ignored `.crossbow-build.env` file at the repository root (a `CROSSBOW_API_URL=https://…` line), and `--candidate` refuses to build without one. A plain source build says "This build has no sync server." and offers no sign-in; you can still type your own server in Settings → Account (https, or http on this Mac). Setting `CROSSBOW_API_URL` when you launch the app, as above, overrides both for that run. `cargo xtask bundle --release` selects release compilation. The current macOS bundler ad-hoc signs a **development build**; this is not a Developer ID/notarized public release. Do not copy it over an existing working signed installation. The bundle replaces the camera, microphone, Bluetooth and passkey usage strings, which CEF leaves as the executable name, with a sentence each, and stops if any usage string is still just that name. It declares no location use, so location stays unavailable. There is no entitlements file yet. A hardened-runtime Developer ID build will need `com.apple.security.device.camera` and `com.apple.security.device.audio-input`. Chromium's helpers probably need JIT and library-validation entitlements, but those haven't been checked against CEF 154. Windows/Linux packaging and native journeys need their own verification.

A build has an updater only if `CROSSBOW_UPDATE_URL` (its signed feed) and `CROSSBOW_UPDATE_KEYS` (the Ed25519 public keys that sign the feed) are both set when it is compiled, together with `CROSSBOW_BUILD_NUMBER`, which becomes the bundle's `CFBundleVersion`. `cargo xtask bundle` reads all three the way it reads `CROSSBOW_API_URL` and refuses one of the first two without the other; every bundle carries a small `crossbow-update` helper beside the app's executable. Such a build can check the feed once a day, or from Crossbow → **Check for Updates…** and **Check Now** in Settings → General → Updates. It verifies a download before installing it at quit or on **Restart to Update**, and rolls back if the new version fails to start. An ad-hoc-signed build, like the development builds above, can't install an update itself and offers the download instead. Maintainers make the signing key with `cargo xtask release-key` and the feed with `cargo xtask release-sign`. This is source only, tested against loopback feeds with simulated signatures. Nothing has been signed, notarized, hosted or released, and no update feed exists yet ([details](docs/feature-status.md#current-source-capabilities)).

### Diagnostics

All are off by default and meant for isolated runs with a disposable `CROSSBOW_DATA_DIR`, not normal browsing. [Feature status](docs/feature-status.md#local-responsiveness-diagnostics) gives the details.

- `CROSSBOW_FRAME_TRACE=1` writes at most 10,000 UI-pass timings to `frame-trace-<pid>-<session-id>.json` under `CROSSBOW_DATA_DIR` on orderly exit. It records no URLs, typed text, account identifiers or credentials.
- `CROSSBOW_INPUT_TRACE=1` (debug macOS builds) logs pointer-button, wheel and window-focus batches, without text, keys, clipboard contents or URLs.
- `CROSSBOW_FOCUS_TRACE=1` (debug macOS builds) logs focus and routing diagnostics, without text, key identities, clipboard contents, URLs or profile/account IDs.
- `CROSSBOW_QA_ARCHIVE_AFTER_SECS=5` (debug builds, together with an explicit `CROSSBOW_DATA_DIR`) replaces the Today auto-archive age with 1–3600 seconds; the check still runs every 60 seconds. **Do not use it with your browsing library:** eligible tabs really move to the archive.

## Tests

```sh
cargo test -p crossbow-proto -p crossbow-core -p crossbow-ai
CEF_PATH="$HOME/.local/share/cef" cargo test -p crossbow --bin crossbow
```

The app tests cover setup state, input validation, popup/Back ownership, animation stability and deterministic textures. Import regression tests cover synthetic data. These tests supplement—not replace—live native testing.

The standalone [synthetic download fixture](tools/README.md) adds finite loopback tests and deterministic 8 MiB fixed, paced, truncated and range-resumable/refusing attachments for later native qualification. It uses Rust only, serves no real user files and does not replace an already-running fixture. Its HTTP/byte checks are not evidence that Crossbow's native Pause/Resume/Cancel works.

The cloud Workers have their own tests and an end-to-end harness; see [cloud/README.md](cloud/README.md).

For every changed feature, use the real control, complete the action, inspect the result, reopen for persistence, and test error/cancel/retry states. Check normal and 640×420 windows. Record the exact build tested and its results; blocked paths remain pending.

## Architecture

- **Desktop:** Rust, egui/eframe 0.37 and wgpu 30 (`app/`).
- **Web engine:** Chromium through CEF 154.5.0, rendered off-screen into the native UI (`crates/crossbow-engine`). CEF is C++; this project does not implement a new web engine in Rust and does not use Electron.
- **Local library:** SQLite, profiles, spaces, folders, pinned items, favorites, tabs, archive and history (`crates/crossbow-core`).
- **Account and sync (current development implementation):** a separate Rust Cloudflare Worker, D1 accounts and per-account Durable Objects, with custom email-code sign-in (`cloud/`). The wire types in `crates/crossbow-proto` are shared by the app and the Worker. By default the client sends JSON payloads; it is **not end-to-end encrypted**.
- **Open provider foundation:** Rust installation discovery, explicit local-server diagnostics and prompt-only text conversations, with device-local selection per profile (`crates/crossbow-ai`). The earlier TypeScript AI Worker is experimental, not a required provider route.
- **Build tasks:** `cargo xtask` bundles the app and runs repository tools (`xtask/`). `website/` holds a static landing page whose waitlist is disabled.

## Security and privacy

- By default the library syncs as JSON payloads, which are not end-to-end encrypted. The opt-in encryption of library rows is source only and has had no external security review; do not describe sync as end-to-end encrypted.
- The password vault is a preview that has had no external security review. Use test passwords only.
- The sync session token is kept in the OS keychain. File token storage exists only as an explicit option for isolated QA, never as a fallback.
- Local AI features send text only to the loopback server you saved, and only after you turn a feature on or press Send. That server may itself forward what it receives.
- Library backups stay on this Mac and never sync or upload.

The [security architecture](docs/security-architecture.md) sets the target (Clerk identity, encrypted library sync and a separate encrypted password vault) and its acceptance gates, none of which is passed yet. The [v1 sync protocol](docs/sync-protocol.md) documents the current implementation, not that future guarantee.

## Known limitations

The full list, with what was tested, is in [feature status](docs/feature-status.md#known-limitations).

- Full Arc feature parity is not established. There is one main window, plus Little Windows whose shortcut, clicks and timer are not natively exercised, and Notes/Easels are not built.
- Chrome extensions are not supported, by decision (2026-11-01): CEF's extension support works only in CEF's own windows. Built-in content blocking takes the place of ad-blocking extensions.
- Stock CEF does not provide proprietary codecs or Widevine; some media sites will not work.
- Identity providers may reject embedded browsers. A successful development sign-in code does not verify real email delivery or web-provider sign-in, and Clerk browser sign-in is unverified against a real Clerk instance.
- Sync is not end-to-end encrypted. Tombstones, row metadata and the local SQLite library stay unencrypted even with the opt-in foundation. Clear History can still leave some cleared history readable in the library file; [feature status](docs/feature-status.md#architecture) lists this and the other open problems.
- The password vault has no local reauthentication, screen-lock locking, automatic fill, import/export or sync, and has had no external security review.
- Chromium's own form history keeps what pages submit, including usernames and address parts filled from the vault, unencrypted in the profile folder; a fix is in progress.
- Per-profile safeguards are not complete account/profile isolation: synced ownership changes, native cookie/restart boundaries and encrypted cookie/password persistence are unqualified. Legacy history and archive rows without an owning profile stay hidden, not deleted.
- Responsiveness is an open problem and a blocking requirement: current builds feel poor in hands-on use, Arc-level latency and frame consistency are not established, and sidebar-resize stutter and tablet-like website rendering are under diagnosis, with no viewport repair verified.
- Repaint stability remains a qualification gate after a lock-timeout crash in a native development build; long-running stability is not proven.
- Many native journeys are unqualified, including Peek, IME cancellation and non-Latin candidate selection, complete overlay coverage, split persistence, and permanent storage failure and crash recovery. Timed auto-archive is not exact unsaved-form detection.
- The importer is synchronous; a large library may stall the UI while it is applied.
- Opening System Settings does not prove the default browser changed. When testing, don't change the default browser of a Mac you use for everyday browsing.
- Production distribution, signed upgrades, Windows/Linux operation, and complete account/profile isolation journeys have not passed native QA.

## Project references

- [Feature status](docs/feature-status.md)
- [Arc Settings parity](docs/arc-settings-parity.md)
- [Product scope](PRODUCT.md)
- [Design system and workflow contract](DESIGN.md)
- [Security architecture](docs/security-architecture.md)
- [Sync protocol](docs/sync-protocol.md)
- [Cloud backend](cloud/README.md)

Arc is a workflow reference. Crossbow does not redistribute Arc's branding, proprietary assets or application code.

## License

Crossbow is licensed under the [Apache License 2.0](LICENSE). Bundled fonts and icons keep their own licenses (SIL Open Font License 1.1, and ISC with MIT portions); see [NOTICE](NOTICE).

The built-in content-blocking lists keep their own license; see [crates/crossbow-core/lists/LICENSE.md](crates/crossbow-core/lists/LICENSE.md):

> EasyList and EasyPrivacy by The EasyList authors (https://easylist.to/), used unmodified under CC BY-SA 3.0 (https://creativecommons.org/licenses/by-sa/3.0/).
