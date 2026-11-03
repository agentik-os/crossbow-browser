# 5-Second Previews fixture

This loopback-only, synthetic page checks Settings → AI Features → **5-Second Previews** against real CEF fetches. Its links cover an article, a redirect chain, a redirect into the local network, a PDF, an attachment, a missing page, a slow page and an empty page. The server sets a cookie on its index page and, for every request, prints only the path and whether a `Cookie` or `Referer` header came (`cookie=yes/no referer=yes/no`), never their values. It records no typed text or personal data.

Build the small Rust server outside the repository's source tree and launch it from its real entry point:

```sh
rustc --edition=2024 app/tests/fixtures/previews/server.rs -o "$CROSSBOW_QA_TOOLS/previews-fixture"
rustc --edition=2024 --test app/tests/fixtures/previews/server.rs -o "$CROSSBOW_QA_TOOLS/previews-fixture-tests"
"$CROSSBOW_QA_TOOLS/previews-fixture-tests"
"$CROSSBOW_QA_TOOLS/previews-fixture"
```

`CROSSBOW_QA_TOOLS` must be an existing private scratch/tool directory. The server binds **127.0.0.1 on an OS-selected port** and prints that exact URL. Paths the page doesn't link to return 404. Stop only the exact server you started.

The page is itself on loopback, so its own links may be previewed; a link from any other page to this server is refused as a local network address.

## Native acceptance procedure

1. Record the exact build (source hash, version/build, signing state). Use a disposable data folder with one normal and one private profile, and a local model server (Ollama or LM Studio) with a small model chosen in AI Providers.
2. Turn on **5-Second Previews** and accept its consent. Open the printed URL in the normal profile; the server prints `GET / cookie=no referer=no` and sets its cookie. Reload once so the tab's next request prints `cookie=yes`.
3. **Real fetch without cookies.** Hold Shift over **An article** for half a second. The card shows "Reading the page…", then a summary of the lighthouse text with "Summary by <provider> · may be wrong". The server prints `GET /article cookie=no referer=no`. The text from the page's script never appears.
4. **Redirect limit.** **Three redirects** prints `/hops/3` to `/hops/1` and `/article`, each `cookie=no referer=no`, and shows a summary. **Four redirects** prints `/hops/4` to `/hops/1` and stops there with "The link redirected more than 3 times, so it wasn't previewed." Nothing reaches the model.
5. **Local network.** **A redirect to the local network** prints `/away` only and shows "Previews aren't available for local network addresses." **A local network address** prints nothing and shows the same notice.
6. **Headers only.** **A PDF** and **An attachment** show their notices; no download starts, nothing appears in Downloads or the downloads list, and nothing reaches the model. **A missing page** shows HTTP status 404, **A slow page** shows the 4-second notice after 4 seconds, and **A page without text** shows the empty-page notice.
7. **Real model.** Stop the model server and hold Shift over **An article**: the card says the provider isn't answering. Restart it and check the summary again.
8. **Focus and dismissal.** With a summary shown, click into the page's text, press Tab and type: the page keeps focus. Escape closes the card; a second Escape reaches the page. Moving off the link, scrolling or clicking also closes it.
9. **Private profiles and Peek.** In the private profile, holding Shift over a link shows "5-Second Previews is unavailable in private profiles." and the server prints nothing. Open the printed URL in Peek from the normal profile and repeat step 3.
10. Turn the feature off: holding Shift over a link shows nothing and the server prints nothing.
11. Repeat steps 3 and 8 at 640×420.

Record a step that fails or can't be observed as such; a passing server test proves only the fixture, not Crossbow.

## Known residual

**DNS rebinding.** Address rules are checked on the host name and literal addresses before each hop. A public name that resolves to a local network address at fetch time isn't refused, because CEF resolves the name after the check. This fixture can't exercise that.
