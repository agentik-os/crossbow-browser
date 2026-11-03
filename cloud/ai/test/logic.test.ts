import { test } from "node:test";
import assert from "node:assert/strict";
import {
  DAILY_LIMIT,
  DEFAULT_MODEL,
  InputError,
  decideQuota,
  errorBody,
  parseCompleteBody,
  retentionCutoff,
  trustedAccount,
  utcDay,
} from "../src/logic.ts";

test("parses a minimal completion request", () => {
  const r = parseCompleteBody({ messages: [{ role: "user", content: "hi" }] });
  assert.equal(r.model, DEFAULT_MODEL);
  assert.deepEqual(r.messages, [{ role: "user", content: "hi" }]);
});

test("rejects bad bodies", () => {
  for (const b of [
    null,
    {},
    { messages: [] },
    { messages: [{ role: "tool", content: "x" }] },
    { messages: [{ role: "user", content: "" }] },
    { messages: [{ role: "system", content: "only system" }] },
    { messages: [{ role: "user", content: "x" }], model: "@cf/unknown/model" },
    { messages: [{ role: "user", content: "x".repeat(32_001) }] },
  ]) {
    assert.throws(() => parseCompleteBody(b), InputError);
  }
});

test("daily quota allows exactly the limit", () => {
  let used = 0;
  for (let i = 0; i < DAILY_LIMIT; i++) {
    const d = decideQuota(used, DAILY_LIMIT);
    assert.equal(d.allowed, true);
    used = d.used;
  }
  assert.equal(used, DAILY_LIMIT);
  const d = decideQuota(used, DAILY_LIMIT);
  assert.equal(d.allowed, false);
  assert.equal(d.remaining, 0);
});

test("days and retention are UTC", () => {
  assert.equal(utcDay(Date.UTC(2026, 9, 23, 23, 59)), "2026-10-23");
  assert.equal(retentionCutoff(Date.UTC(2026, 9, 23)), "2026-10-16");
});

test("only API-injected account ids are trusted", () => {
  assert.equal(trustedAccount("acc_" + "a".repeat(32)), "acc_" + "a".repeat(32));
  assert.equal(trustedAccount(null), null);
  assert.equal(trustedAccount("evil"), null);
  assert.equal(trustedAccount("acc_" + "A".repeat(32)), null);
});

test("error body shape", () => {
  assert.deepEqual(JSON.parse(errorBody("rate_limited", "m")), { error: { code: "rate_limited", message: "m" } });
});

test("SSE events carrying both response and delta.content lose the duplicate", async () => {
  const { dedupeSseLine, dedupeSseStream } = await import("../src/logic.ts");
  const both = 'data: {"choices":[{"delta":{"content":"One"}}],"response":"One"}';
  assert.equal(dedupeSseLine(both), 'data: {"choices":[{"delta":{"content":"One"}}]}');
  const legacy = 'data: {"response":"One"}';
  assert.equal(dedupeSseLine(legacy), legacy);
  const reasoning = 'data: {"choices":[{"delta":{"reasoning_content":"x"}}],"response":""}';
  assert.equal(dedupeSseLine(reasoning), reasoning);
  assert.equal(dedupeSseLine("data: [DONE]"), "data: [DONE]");
  assert.equal(dedupeSseLine(": keep-alive"), ": keep-alive");

  // Split an event across chunk boundaries, including inside a UTF-8 sequence.
  const input = new TextEncoder().encode(`${both}\n\ndata: {"choices":[{"delta":{"content":"é"}}],"response":"é"}\n\ndata: [DONE]\n\n`);
  const parts = [input.slice(0, 10), input.slice(10, 75), input.slice(75)];
  const stream = new ReadableStream({ start(c) { for (const p of parts) c.enqueue(p); c.close(); } }).pipeThrough(dedupeSseStream());
  const text = await new Response(stream).text();
  assert.equal(text, 'data: {"choices":[{"delta":{"content":"One"}}]}\n\ndata: {"choices":[{"delta":{"content":"é"}}]}\n\ndata: [DONE]\n\n');
});
