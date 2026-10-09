import { afterEach, describe, expect, it, vi } from "vitest";

import {
  agentRunCancelPath,
  agentRunPath,
  cancelAgentRun,
  createAgentRun,
  getAgentRun,
} from "@/lib/api/agents";

function mockFetch(status = 200, body: unknown = { data: {} }) {
  const calls: { url: string; init: RequestInit }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init: init ?? {} });
      return new Response(JSON.stringify(body), {
        status,
        headers: { "content-type": "application/json" },
      });
    })
  );
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("agents api", () => {
  it("creates a run with Idempotency-Key and no If-Match (unversioned create)", async () => {
    const calls = mockFetch(201, { data: { id: "r1" } });
    await createAgentRun(
      { agent_principal_id: "a1", objective: "find leads", budget: { max_proposals: 100 } },
      { idempotencyKey: "run-key-1" }
    );
    const h = new Headers(calls[0]!.init.headers);
    expect(calls[0]!.url).toBe("/api/v1/agent-runs");
    expect(calls[0]!.init.method).toBe("POST");
    expect(h.get("Idempotency-Key")).toBe("run-key-1");
    expect(h.get("If-Match")).toBeNull();
    expect(JSON.parse(calls[0]!.init.body as string)).toEqual({
      agent_principal_id: "a1",
      objective: "find leads",
      budget: { max_proposals: 100 },
    });
  });

  it("cancels a run via /cancel without If-Match (agent_runs has no version)", async () => {
    const calls = mockFetch(200, { data: { id: "r1" } });
    await cancelAgentRun(
      "a-b-c",
      { reason: "stop" },
      { idempotencyKey: "cancel-key" }
    );
    const h = new Headers(calls[0]!.init.headers);
    expect(calls[0]!.url).toBe("/api/v1/agent-runs/a-b-c/cancel");
    expect(h.get("If-Match")).toBeNull();
    expect(h.get("Idempotency-Key")).toBe("cancel-key");
  });

  it("fetches a run detail with GET and encodes the id", async () => {
    const calls = mockFetch(200, { data: { id: "x" } });
    await getAgentRun("id/with spaces");
    expect(calls[0]!.url).toBe("/api/v1/agent-runs/id%2Fwith%20spaces");
    expect((calls[0]!.init.method ?? "GET").toUpperCase()).toBe("GET");
  });

  it("builds paths", () => {
    expect(agentRunPath("r9")).toBe("/api/v1/agent-runs/r9");
    expect(agentRunCancelPath("r9")).toBe("/api/v1/agent-runs/r9/cancel");
  });
});
