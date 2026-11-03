# Sidebar viewport fixture

This loopback-only, synthetic page separates actual layout viewport size from device scale and responsive CSS. It records no typed text, credentials, URLs or personal browsing data. The readback includes only whether the draft equals the fixed marker `KEEP-RESIZE`, text lengths, caret indices and content-free viewport/focus metrics. In-memory resize samples are bounded at 300 and report drops.

Build the small Rust server outside the repository's source tree and launch it from its real entry point:

```sh
rustc --edition=2024 app/tests/fixtures/viewport/server.rs -o "$CROSSBOW_QA_TOOLS/viewport-fixture"
rustc --edition=2024 --test app/tests/fixtures/viewport/server.rs -o "$CROSSBOW_QA_TOOLS/viewport-fixture-tests"
"$CROSSBOW_QA_TOOLS/viewport-fixture-tests"
"$CROSSBOW_QA_TOOLS/viewport-fixture"
```

`CROSSBOW_QA_TOOLS` must be an existing private scratch/tool directory. The server binds **127.0.0.1 on an OS-selected port** and prints that exact URL. It never stops/reuses another fixture service, reads files by request path or writes state. It serves only `/`; unknown paths and queries return 404. Stop only the exact owned server after the run.

## Native acceptance procedure

1. Freeze the exact build's source/binary hashes and record version/build, signing state and installation boundary. Use a new disposable profile, and make sure nothing else uses the desktop's keyboard and pointer during the run.
2. Open the printed loopback URL through the real Crossbow entry point. Do not reuse a real browser library. Record the native content rectangle and scale, engine view rectangle/screen scale, backing texture dimensions and this page's `viewportProbe.snapshot()`.
3. Type `KEEP-RESIZE` with native input, put the caret inside it, and scroll using the real page controls. CDP may only **read** the allowlisted fixture. Do not inject focus, field values, selection or CSS.
4. At normal and 640×420 windows, exercise sidebar show/hide, repeated reversal and continuous width drag. Repeat with an open split, Peek, search and Settings; check stale frames, hit testing, popup placement, focus/caret, scroll and preserved drafts. Compare both transition endpoints and intermediate geometry.
5. Match actual **content** width and scale in the existing authorized Arc reference window. The 640/1024px CSS breakpoints are intentional: a narrow layout is not evidence of mobile emulation. Compare `innerWidth`, `devicePixelRatio`, visual viewport scale, user agent/mobile hint and the exact 100 CSS pixel ruler.
6. Confirm restored sidebar width/state after an orderly quit/reopen. A document draft is not expected to survive a reload unless a separate session-recovery feature explicitly provides it; do not confuse that with draft loss during resizing.
7. Measure native input-to-presentation and frame cadence independently. The fixture's `requestAnimationFrame` gap is **page scheduling context only**, not display-frame or latency proof. Keep outliers, dropped-sample counts and actual display refresh/load context.

If capture, exact-target inspection, desktop isolation or signing/installation access is unavailable, keep that row unverified. Server/unit/headless fixture tests validate the instrument only, not native Crossbow behavior or Arc parity.
