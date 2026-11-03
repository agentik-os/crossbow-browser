// Crossbow AI Worker.
//
// Not public: `workers_dev = false` and no routes. The only way in is the API
// Worker's `AI_WORKER` service binding, which authenticates the session, strips
// client copies of X-Crossbow-* and injects the trusted X-Crossbow-Account and
// X-Crossbow-Device headers. Plumbing only: no app UI uses these routes yet.

import { Agent, getAgentByName } from "agents";
import { streamText } from "ai";
import { createWorkersAI } from "workers-ai-provider";
import {
  DAILY_LIMIT,
  InputError,
  MAX_OUTPUT_TOKENS,
  MODELS,
  type QuotaDecision,
  decideQuota,
  dedupeSseStream,
  errorBody,
  parseCompleteBody,
  retentionCutoff,
  trustedAccount,
  utcDay,
} from "./logic";

/**
 * One Durable Object per account (named by the account id). Today it only keeps
 * the daily request count; it is the future home of conversation state and tools.
 */
export class CrossbowAgent extends Agent<Env> {
  /** Count one completion against today's quota. Called over DO RPC. */
  async consumeDailyRequest(nowMs: number, limit: number): Promise<QuotaDecision> {
    this.sql`CREATE TABLE IF NOT EXISTS ai_usage (day TEXT PRIMARY KEY, used INTEGER NOT NULL)`;
    const day = utcDay(nowMs);
    const rows = this.sql<{ used: number }>`SELECT used FROM ai_usage WHERE day = ${day}`;
    const decision = decideQuota(rows[0]?.used ?? 0, limit);
    if (decision.allowed) {
      this.sql`INSERT INTO ai_usage (day, used) VALUES (${day}, ${decision.used})
               ON CONFLICT(day) DO UPDATE SET used = excluded.used`;
      this.sql`DELETE FROM ai_usage WHERE day < ${retentionCutoff(nowMs)}`;
    }
    return decision;
  }

  /** Nothing reaches the agent over HTTP yet. */
  async onRequest(): Promise<Response> {
    return error(404, "not_found", "not found");
  }
}

function error(status: number, code: string, message: string): Response {
  return new Response(errorBody(code, message), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store" },
  });
}

/**
 * The AI binding with streamed results passed through `dedupeSseStream` (see
 * logic.ts). Only `run` is used by workers-ai-provider for Workers AI chat models.
 */
function dedupingBinding(ai: Ai): Ai {
  const run = async (...args: Parameters<Ai["run"]>) => {
    const out: unknown = await (ai.run as (...a: unknown[]) => Promise<unknown>)(...args);
    return out instanceof ReadableStream ? out.pipeThrough(dedupeSseStream()) : out;
  };
  const gateway = (id: string) => ai.gateway(id);
  return { run, gateway } as unknown as Ai;
}

function gatewayId(env: Env): string | null {
  const id = (env.AI_GATEWAY_ID ?? "").trim();
  return id.length > 0 ? id : null;
}

async function complete(req: Request, env: Env, account: string): Promise<Response> {
  let input;
  try {
    input = parseCompleteBody(await req.json().catch(() => null));
  } catch (e) {
    if (e instanceof InputError) return error(400, "bad_request", e.message);
    throw e;
  }

  const agent = await getAgentByName(env.CrossbowAgent, account);
  const quota = await agent.consumeDailyRequest(Date.now(), DAILY_LIMIT);
  if (!quota.allowed) {
    return error(429, "rate_limited", `daily AI limit reached (${DAILY_LIMIT} requests per day)`);
  }

  const gateway = gatewayId(env);
  const workersai = createWorkersAI(
    gateway ? { binding: dedupingBinding(env.AI), gateway: { id: gateway } } : { binding: dedupingBinding(env.AI) },
  );
  const result = streamText({
    model: workersai(input.model),
    messages: input.messages,
    maxOutputTokens: MAX_OUTPUT_TOKENS,
    onError: ({ error: e }) => console.error("ai: stream failed", e instanceof Error ? e.message : String(e)),
  });
  return result.toTextStreamResponse({
    headers: {
      "cache-control": "no-store",
      "x-crossbow-ai-model": input.model,
      "x-crossbow-ai-remaining": String(quota.remaining),
    },
  });
}

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const account = trustedAccount(req.headers.get("X-Crossbow-Account"));
    if (!account) return error(401, "unauthorized", "missing or invalid session");
    const { pathname } = new URL(req.url);
    try {
      if (req.method === "GET" && pathname === "/v1/ai/health") {
        // `account` echoes the injected header so callers can check the proxy.
        return Response.json(
          { ok: true, models: MODELS, gateway: gatewayId(env), account },
          { headers: { "cache-control": "no-store" } },
        );
      }
      if (req.method === "POST" && pathname === "/v1/ai/complete") {
        return await complete(req, env, account);
      }
      return error(404, "not_found", "not found");
    } catch (e) {
      console.error("ai: request failed", e instanceof Error ? e.message : String(e));
      return error(500, "internal", "internal error");
    }
  },
} satisfies ExportedHandler<Env>;
