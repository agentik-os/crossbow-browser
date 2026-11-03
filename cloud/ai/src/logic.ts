// Pure helpers for the AI Worker (no Workers runtime imports), tested with
// `npm test` (node --test).

/** Workers AI text model used by POST /v1/ai/complete (verified in the catalog). */
export const DEFAULT_MODEL = "@cf/meta/llama-4-scout-17b-16e-instruct";
/** Models a client may ask for. */
export const MODELS: readonly string[] = [DEFAULT_MODEL];
/** POST /v1/ai/complete requests per account per UTC day. */
export const DAILY_LIMIT = 50;
export const MAX_OUTPUT_TOKENS = 1024;
export const MAX_MESSAGES = 50;
export const MAX_MESSAGE_CHARS = 32_000;
export const MAX_TOTAL_CHARS = 100_000;
/** Usage rows older than this many days are dropped. */
export const USAGE_RETENTION_DAYS = 7;

export type Role = "system" | "user" | "assistant";
export interface ChatMessage {
  role: Role;
  content: string;
}
export interface CompleteInput {
  messages: ChatMessage[];
  model: string;
}

export class InputError extends Error {}

const ROLES = new Set<string>(["system", "user", "assistant"]);

/** Validate `{messages:[{role, content}], model?}`. Throws InputError. */
export function parseCompleteBody(body: unknown): CompleteInput {
  if (!body || typeof body !== "object") throw new InputError("body must be a JSON object");
  const { messages, model } = body as { messages?: unknown; model?: unknown };
  if (!Array.isArray(messages) || messages.length === 0 || messages.length > MAX_MESSAGES) {
    throw new InputError(`messages must be an array of 1-${MAX_MESSAGES} items`);
  }
  let total = 0;
  const out: ChatMessage[] = [];
  for (const m of messages) {
    if (!m || typeof m !== "object") throw new InputError("each message must be an object");
    const { role, content } = m as { role?: unknown; content?: unknown };
    if (typeof role !== "string" || !ROLES.has(role)) {
      throw new InputError("role must be system, user or assistant");
    }
    if (typeof content !== "string" || content.length === 0 || content.length > MAX_MESSAGE_CHARS) {
      throw new InputError(`content must be a string of 1-${MAX_MESSAGE_CHARS} characters`);
    }
    total += content.length;
    out.push({ role: role as Role, content });
  }
  if (total > MAX_TOTAL_CHARS) throw new InputError("messages are too long");
  if (!out.some((m) => m.role === "user")) throw new InputError("at least one user message is required");
  let chosen = DEFAULT_MODEL;
  if (model !== undefined && model !== null) {
    if (typeof model !== "string" || !MODELS.includes(model)) {
      throw new InputError(`model must be one of: ${MODELS.join(", ")}`);
    }
    chosen = model;
  }
  return { messages: out, model: chosen };
}

/** UTC calendar day, e.g. "2026-10-23". */
export function utcDay(nowMs: number): string {
  return new Date(nowMs).toISOString().slice(0, 10);
}

export function retentionCutoff(nowMs: number): string {
  return utcDay(nowMs - USAGE_RETENTION_DAYS * 86_400_000);
}

export interface QuotaDecision {
  allowed: boolean;
  /** Requests counted today after this decision. */
  used: number;
  limit: number;
  remaining: number;
}

export function decideQuota(usedBefore: number, limit: number): QuotaDecision {
  if (usedBefore >= limit) return { allowed: false, used: usedBefore, limit, remaining: 0 };
  const used = usedBefore + 1;
  return { allowed: true, used, limit, remaining: limit - used };
}

/** The API Worker injects ids of this shape; anything else is not trusted. */
export function trustedAccount(value: string | null): string | null {
  return value && /^acc_[0-9a-f]{32}$/.test(value) ? value : null;
}

export function errorBody(code: string, message: string): string {
  return JSON.stringify({ error: { code, message } });
}

// --- Workers AI stream compatibility -------------------------------------
//
// Since October 2026, Workers AI text models stream OpenAI-style SSE events that
// also carry the legacy `response` field with the same text, e.g.
//   data: {"choices":[{"delta":{"content":"One"}}], "response":"One", ...}
// workers-ai-provider (all releases through 4.0.1) emits a text delta for both
// fields, so every token arrives twice. We drop `response` whenever the event
// already has `choices[0].delta.content`, and leave every other event alone.

/** Rewrite one SSE line; non-data lines and unparsable data pass through. */
export function dedupeSseLine(line: string): string {
  if (!line.startsWith("data:")) return line;
  const data = line.slice(5).trimStart();
  if (data === "" || data === "[DONE]") return line;
  try {
    const event = JSON.parse(data) as { response?: unknown; choices?: Array<{ delta?: { content?: unknown } }> };
    if (event && typeof event === "object" && "response" in event && typeof event.choices?.[0]?.delta?.content === "string") {
      delete event.response;
      return `data: ${JSON.stringify(event)}`;
    }
  } catch {
    // Not JSON: leave it for the provider to report.
  }
  return line;
}

/** Byte stream transform applying `dedupeSseLine` across chunk boundaries. */
export function dedupeSseStream(): TransformStream<Uint8Array, Uint8Array> {
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  let pending = "";
  return new TransformStream({
    transform(chunk, controller) {
      pending += decoder.decode(chunk, { stream: true });
      const lines = pending.split("\n");
      pending = lines.pop() ?? "";
      if (lines.length > 0) controller.enqueue(encoder.encode(lines.map(dedupeSseLine).join("\n") + "\n"));
    },
    flush(controller) {
      pending += decoder.decode();
      if (pending.length > 0) controller.enqueue(encoder.encode(dedupeSseLine(pending)));
    },
  });
}
