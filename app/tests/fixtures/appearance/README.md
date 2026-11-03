# Website appearance fixture

This loopback-only, synthetic page shows whether a page follows Settings → General → **Website appearance**. It reports what `prefers-color-scheme` says, records each change while the page stays open (at most 50, with a dropped count) and whether the page was reloaded. It changes nothing itself and records no typed text, URLs or personal data. Its light and dark colours come from its own `@media (prefers-color-scheme: dark)` rule, so a correct result needs no injected CSS.

Build the small Rust server outside the repository's source tree and launch it from its real entry point:

```sh
rustc --edition=2024 app/tests/fixtures/appearance/server.rs -o "$CROSSBOW_QA_TOOLS/appearance-fixture"
rustc --edition=2024 --test app/tests/fixtures/appearance/server.rs -o "$CROSSBOW_QA_TOOLS/appearance-fixture-tests"
"$CROSSBOW_QA_TOOLS/appearance-fixture-tests"
"$CROSSBOW_QA_TOOLS/appearance-fixture"
```

`CROSSBOW_QA_TOOLS` must be an existing private scratch/tool directory. The server binds **127.0.0.1 on an OS-selected port** and prints that exact URL. It serves only `/`; other paths and queries return 404. Stop only the exact server you started.

## Native acceptance procedure

1. Record the exact build (source hash, version/build, signing state). Use a disposable data folder with two profiles, each used by its own Space. Don't pass `--force-dark-mode`, `--force-light-mode` or any other appearance switch.
2. Open the printed URL in a tab of each Space. With both set to **Automatic**, the label and `appearanceProbe.snapshot().prefersDark` match the macOS appearance.
3. In the first Space, choose **Dark**. Without reloading, that tab shows `dark`, `changeCount` grows and `navigationType` stays `navigate`. The second Space's tab doesn't change. Choose **Light**, then **Automatic**, and check each the same way.
4. Switch macOS between Light and Dark: tabs on **Automatic** follow; tabs on Light or Dark don't.
5. Check a field and the menu's popup in each mode, and Peek and a Little Window of the same profile.
6. Quit and reopen: each profile's choice is still selected, and the page opens in that appearance with `changeCount` at 0.
7. Repeat steps 2 and 3 at 640×420.

Record a step that fails or can't be observed as such; a passing server test proves only the fixture, not Crossbow.
