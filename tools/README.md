# Synthetic download fixture

`download_fixture.rs` is a standalone Rust HTTP fixture for **local download qualification**, not an application change or a production server. It has no dependencies, external requests, credentials, access to real user data, upload endpoint or arbitrary file-serving route. The HTML contains only same-origin fixture links.

The original Peek fixture's `/`, `/parent-next`, `/ready`, `/flaky`, `/slow` and `/favicon.ico` routes remain available. The parent/ready HTML and existing control-marker meanings are preserved. Building it does **not** replace, restart or update an already-running fixture. Do not bind another server to a running fixture's port.

## Build and finite tests

Run from the repository root. Set `FIXTURE_WORK` to a disposable scratch directory **outside the repository**. The fixture itself requires an existing controls directory and does not create or modify control markers.

```sh
: "${FIXTURE_WORK:?Set FIXTURE_WORK to your disposable scratch directory}"
mkdir -p "$FIXTURE_WORK/controls"
rustc --edition 2024 -D warnings -C opt-level=1 \
  tools/download_fixture.rs -o "$FIXTURE_WORK/download-fixture"
rustc --edition 2024 --test -D warnings -C opt-level=1 \
  tools/download_fixture.rs -o "$FIXTURE_WORK/download-fixture-tests"
CROSSBOW_FIXTURE_TEST_ROOT="$FIXTURE_WORK" \
  "$FIXTURE_WORK/download-fixture-tests" --test-threads=1 --nocapture
```

Tests create unique disposable child directories, bind **only `127.0.0.1:0`** (OS-assigned ephemeral ports), join every fixture worker and remove their own child directories. They do not contact port 18791 or leave a persistent server running. `CROSSBOW_FIXTURE_TEST_ROOT` must already exist; this avoids silently choosing a working directory or a real user's data directory.

The suite covers deterministic bytes; fixed/slow downloads; GET/HEAD; single-byte, suffix and open-ended ranges; 206/416; stable ETag and If-Range fallback; deliberate truncation; resumed bytes reconstructed, persisted and read back; range refusal and full retry; fragmented/malformed/oversized headers; absolute header and response deadlines; stalled-reader write timeout; client cancellation/disconnect and worker reuse; bounded concurrency; missing controls; and the old Peek routes/markers. Most test clocks are shortened, while a separate loopback test exercises production chunk pacing and asserts the production bounds. The legacy 30-second fallback is exercised with a shortened test clock, not a 30-second native wait.

## Optional later manual serving

Check existing listeners before starting or replacing a manual fixture. Neither command below is part of the finite test procedure.

```sh
# Original protocol: first positional argument is an existing synthetic controls directory.
# Default address is 127.0.0.1:18791. Do not run while that port's existing fixture is active.
"$FIXTURE_WORK/download-fixture" "$FIXTURE_WORK/controls"

# Alternative: request an OS-assigned loopback port; the startup line prints the address.
"$FIXTURE_WORK/download-fixture" "$FIXTURE_WORK/controls" --port 0
```

There is no host/bind-address option: both modes bind IPv4 loopback only. The server remains in the foreground until stopped; connection lifetimes and worker concurrency are bounded. No credentials or real browser library are needed. Open `/downloads` only from an isolated native development build with a disposable data folder.

## Attachments

All complete attachment bodies are exactly **8,388,608 bytes (8 MiB)**, with the same deterministic contents. Their fixed, distinct attachment filenames support collision/destination checks without serving any existing file. Paced responses send 32 KiB chunks with 100 ms between chunks: approximately 25.5 seconds for a complete payload, excluding scheduling/backpressure.

| Route | Behavior | Range support |
| --- | --- | --- |
| `/downloads/fixed.bin` | Complete, unpaced baseline | `bytes` |
| `/downloads/slow.bin` | Complete, paced; suitable for attempting native Pause/Resume/Cancel | `bytes` |
| `/downloads/truncated.bin` | Always advertises 8 MiB but closes after 262,144 bytes; intentionally invalid body length | `none`; ignores Range |
| `/downloads/resumable.bin` | Paced; normally complete. With `interrupt-resumable`, full 200 responses close after 262,144 bytes, while accepted ranges still complete | `bytes` |
| `/downloads/range-refusing.bin` | Paced; always ignores Range and sends from byte zero with 200. With `interrupt-refusing`, every response closes after 262,144 bytes until the marker is removed | `none` |

`/downloads` is an explicit entry page linking all five attachments. Unknown paths, including paths that happen to name control files, return 404. Query strings do not select files, sizes or behavior and are never logged.

### Control markers

These are **file-existence checks in the supplied synthetic controls directory**, not HTTP administration endpoints. Marker contents are not read, and the server does not create, delete or consume markers.

| Marker | Meaning |
| --- | --- |
| `recover` | Existing behavior: `/flaky` returns the ready HTML. Without it, `/flaky` sends the exact deliberately malformed `CROSSBOW_SYNTHETIC_INVALID_HTTP` response. |
| `release-slow` | Existing behavior: release pending `/slow` responses. Without it, they wait at most 30 seconds, checking at 100 ms intervals, then return ready HTML. |
| `interrupt-resumable` | Snapshot at response-body start: truncate full 200 responses on the resumable attachment. Accepted 206 responses bypass interruption even while the marker remains. |
| `interrupt-refusing` | Snapshot at response-body start: truncate the range-refusing attachment. Remove before a deliberate full retry; ranges still remain unsupported. |

Adding/removing an interruption marker does not change a response already streaming. The markers are independent; `release-slow` affects only the old Peek `/slow`, not `/downloads/slow.bin`.

## HTTP contract and bounds

- HTTP/1.0 and HTTP/1.1 requests; GET/HEAD only, otherwise 405 with `Allow: GET, HEAD`. HTTP/1.1 requires one nonempty Host header. Each connection handles one request and closes; no keep-alive, TLS, HTTP/2 or request bodies.
- A bounded loop reads the **complete header terminator**, including fragmented headers: 16 KiB total, 64 header fields, 4 KiB per field and a 2 KiB request-target limit. Malformed syntax, duplicate Host/Range/If-Range/Content-Length, folded fields, transfer encoding and nonzero request-body length are rejected. Headers time out after **two seconds total**, not two seconds per fragment; responses use 400/408/431 as appropriate.
- Single `bytes=start-end`, `bytes=start-` and `bytes=-suffix` ranges are supported on range-capable GET endpoints. End positions are clipped to the payload; positive oversized suffixes select the whole payload. Satisfiable requests return 206 with correct Content-Length and Content-Range. Out-of-bounds starts and zero-length suffixes return 416 with `Content-Range: bytes */8388608` and an empty body. This intentionally narrow fixture rejects malformed, multipart and unsupported-unit ranges with 400 instead of implementing multipart responses.
- The stable strong ETag is `"crossbow-synthetic-v1-8388608"`. Missing If-Range or an exact match allows the range. Stale, weak and date validators cause a full 200; no date validator is advertised. Range-refusing routes always return 200 regardless of Range/If-Range. Any configured deliberate truncation still applies to a full 200.
- HEAD ignores Range and returns the full representation's headers, including Content-Length, ETag, Content-Disposition and Accept-Ranges, with **no body and no deliberate truncation**. The intentionally malformed legacy `/flaky` remains the explicit protocol-error exception when unrecovered.
- Every ordinary response uses `Connection: close` and `Cache-Control: no-store`. Attachments use `application/octet-stream`, safe fixed attachment filenames and `Accept-Ranges: bytes` or `none`. The documented deliberate interruptions are the only intended Content-Length/body mismatches; disconnects/timeouts can also interrupt transport.
- At most **eight worker threads/connections** are served concurrently; excess accepted connections close immediately without a response. There is no unbounded application queue or thread spawning. Reads/writes have two-second socket timeouts; each response additionally has a 90-second deadline. Long pauses/backpressure can therefore end a transfer. These are controlled fixture limits, not application download policies.
- At most 10,000 connection log records contain fixed synthetic route/outcome labels, status and byte offset/length/written counts. No requested path/query, header value, peer address, file path or credential is copied into request logs. Startup prints only the loopback listener address. `written` counts bytes accepted by the socket, **not bytes persisted by the browser**. A disconnect may be reported as `disconnected` or generic `io-error`, depending on the platform's socket-error classification.

## Materialize and hash expected bytes

This mode binds **no socket**, requires the same existing controls directory, writes one newly created file, flushes it, and refuses to overwrite an existing path. On Unix the file is created with mode 0600. The server never serves that output path.

```sh
"$FIXTURE_WORK/download-fixture" "$FIXTURE_WORK/controls" \
  --emit-expected "$FIXTURE_WORK/expected.bin"
shasum -a 256 "$FIXTURE_WORK/expected.bin"
# Compare separately against the actual completed file saved by the native application.
```

Payload version 1 uses, for each zero-based byte position `i`:

```text
byte[i] = (31*i XOR (i >> 8) XOR (i >> 16) XOR 0xa5) AND 0xff
```

Expected complete-file SHA-256, observed with `shasum -a 256`:

```text
b7ddd3118b90316d232cf22b24a56fabd8fe789f00ce7ae865dc24063091a3f8
```

A recovered transfer must reconstruct exactly these bytes without appending a full 200 response onto an existing prefix. The HTTP tests verify that an interrupted resumable response plus an accepted Range response reconstructs the expected file. They do **not** establish what Chromium will request, how native Resume handles an interruption, or whether a range-refusing transfer should restart or fail.

## Evidence boundary

These tests are fixture-level HTTP and filesystem evidence only. They do not verify the actual Crossbow download chooser, destination/profile boundaries, native Pause/Resume/Cancel callbacks, pointer/keyboard/focus/compact layout, file collision behavior, native reopen, restart-resume or installed application delivery. The tool is not a replacement for exercising downloads natively in the exact build under test.
