# Password save and fill fixture

Loopback-only, synthetic pages for saving and filling website passwords. Two servers listen on two OS-selected ports of 127.0.0.1, and the second is addressed as localhost, so they are two origins of two sites. The fields have no `name` attributes, so a submit sends no field value to the server. Each request's first line is read; the headers and any body are discarded without being read into memory, and nothing is stored or printed. Use only synthetic values such as `fixture-user` and `fixture-pass-1`. Never type a real password.

Build the server outside the repository's source tree and launch it from its real entry point:

```sh
rustc --edition=2024 app/tests/fixtures/login/server.rs -o "$CROSSBOW_QA_TOOLS/login-fixture"
rustc --edition=2024 --test app/tests/fixtures/login/server.rs -o "$CROSSBOW_QA_TOOLS/login-fixture-tests"
"$CROSSBOW_QA_TOOLS/login-fixture-tests"
"$CROSSBOW_QA_TOOLS/login-fixture"
```

`CROSSBOW_QA_TOOLS` must be an existing private scratch or tool directory. The server prints both URLs, `Login fixture: http://127.0.0.1:<site>/` and `Other origin: http://localhost:<other>/`. It serves `/`, `/form`, `POST /signed-in`, `/fetch`, `POST /api/login`, `/frames` and `/patched`; every other path, query or method returns 404. Stop only that exact server process after the run.

| Page | What it is |
|---|---|
| `/form` | A login form that posts to `/signed-in`, so the page navigates. |
| `/fetch` | A login form that stays on the page and sends an empty `POST /api/login`. |
| `/frames` | The form in a same-origin frame and in a frame from the other site (localhost). |
| `/patched` | A page that replaces, before its other scripts, every built-in the listener captures, and reports calls made outside its own code. |
| `<other>/form` | The same form on localhost: another origin and another site. |

## Native checks

Server, unit and simulated-frame tests check the instrument and the source only, not native Crossbow. Keep any row below unverified if it could not be run.

1. Record the exact app's path, version, build and source commit. Launch it with a new disposable data folder, the mock Keychain and the QA vault key: `open -n -a <build folder>/Crossbow.app --env CROSSBOW_DATA_DIR=<disposable folder> --env CROSSBOW_VAULT_KEY_FILE=1 --args --use-mock-keychain`. Settings → Passwords must show the QA key file banner. Don't reuse a real library or vault.
2. **Off by default.** Settings → Passwords → Saving from websites shows "Offer to save passwords from websites" off, the preview line and "Changes apply to pages loaded afterwards." Open `<site>/form` and sign in: no card. In DevTools, `getEventListeners(window).submit` is undefined: no submit listener was installed. Turn the switch on for this profile.
3. **Navigating form.** Reload `<site>/form`, type `fixture-user` and `fixture-pass-1`, Sign In. Exactly one card "Save password for http://127.0.0.1:<site>?" appears (the scheme shows for http sites) at the top right of the pane, with the username. Check it at 640×420 and at a normal size, with the sidebar shown and hidden and with Find open: no clipped or overlapping text, focus ring on the username, Tab order Username, Save, Never for This Site, Not Now.
4. **Not Now, Save, Never.** Not Now: the card goes, nothing is listed in Settings. Sign in again, Save: Settings lists `http://127.0.0.1:<site>` with its profile and "Never used on a page". Quit and relaunch: the entry is still there. Sign in with `fixture-pass-2`: the card says "Update password for…"; **Update Password** replaces only that entry. Never for This Site on another username: no more offers there; Settings lists the site under Never saved; Remove, relaunch, and an offer comes back.
5. **Locked.** Lock Now, sign in: Save shows "Passwords are locked…" and Unlock, which unlocks and saves. Escape with the username focused closes the card and returns keys to the page.
6. **Fetch login.** On `<site>/fetch`, sign in: one offer, without the page navigating.
7. **Frames.** On `<site>/frames`, sign in inside the same-origin frame: one offer. Sign in inside the cross-origin frame: no offer. With a saved entry, focus the cross-origin frame's field and use the key button: its fields stay empty, and the same-origin frame or nothing is filled.
8. **Fill.** On `<site>/form` the key button shows in the address bar; choose the entry: both fields fill, and the page navigates only when you press Sign In. Settings shows "Last used today" after a relaunch. On `<other>/form` there is no key button and no fill. Nothing fills on page load. Lock, focus the password field: the key shows; its menu offers Unlock Passwords, then the entry.
9. **Profiles.** In a Space of another profile, open `<site>/form`: no key button; sign in: no card, as that profile's switch is still off. Turn it on there and reload: the offer saves into that profile only. Never-save lists differ per profile.
10. **Patched built-ins.** On `<site>/patched`, sign in, save, then fill from the key button. The report must keep "Calls from outside this page: none" and "Crossbow names on window: none". "Forge an Untrusted Submit" must give no offer. "Submit From Script After This Click" may offer `forged-user`, for this origin only; Not Now must leave nothing saved. A DevTools read of `loginProbe.snapshot()` is allowed; don't inject input, values or focus.
11. **Off mid-session.** With `<site>/form` open, turn the switch off. Sign in without reloading: no card. Reload: `getEventListeners(window).submit` is undefined again, and signing in gives no card. The key button still fills the saved entry. Relaunch: the switch is still off, and the other profile's switch is unchanged.
12. **No secret in logs.** After the run, search the app's log output and the data folder (except `vault.sqlite*` and the QA key file) for `fixture-pass-1`, `fixture-pass-2` and `forged-pass`: no match.

Record each command, interaction and observed result. Stop the fixture server and delete the disposable data folder afterwards.
