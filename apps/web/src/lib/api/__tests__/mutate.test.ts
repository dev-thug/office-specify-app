import { afterEach, describe, expect, it, vi } from "vitest";

import { mutate, readEtag } from "@/lib/api/mutate";

function mockFetch(
  status = 200,
  body: unknown = { data: { ok: true } },
  headers: Record<string, string> = {}
) {
  const calls: { url: string; init: RequestInit }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init: init ?? {} });
      return new Response(body === null ? "" : JSON.stringify(body), {
        status,
        headers: { "content-type": "application/json", ...headers },
      });
    })
  );
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("mutate", () => {
  it("applies If-Match from a stored etag and the caller's idempotency key", async () => {
    const calls = mockFetch();
    const res = await mutate<{ ok: boolean }>(
      "/api/v1/companies/1",
      { method: "PATCH", json: { name: "x" } },
      { etag: 7, idempotencyKey: "op-key-1" }
    );
    const h = new Headers(calls[0]!.init.headers);
    expect(h.get("If-Match")).toBe('"7"');
    expect(h.get("Idempotency-Key")).toBe("op-key-1");
    expect(h.get("Content-Type")).toBe("application/json");
    expect(calls[0]!.init.method).toBe("PATCH");
    expect(res.data.ok).toBe(true);
  });

  it("accepts an already-quoted etag unchanged", async () => {
    const calls = mockFetch();
    await mutate("/api/v1/x", { method: "PUT" }, { etag: '"42"' });
    expect(new Headers(calls[0]!.init.headers).get("If-Match")).toBe('"42"');
  });

  it("generates an idempotency key when the caller omits one", async () => {
    const calls = mockFetch();
    await mutate("/api/v1/x", { method: "POST", json: {} });
    await mutate("/api/v1/x", { method: "POST", json: {} });
    const key1 = new Headers(calls[0]!.init.headers).get("Idempotency-Key");
    const key2 = new Headers(calls[1]!.init.headers).get("Idempotency-Key");
    expect(key1).toMatch(/^[0-9a-f-]{36}$/);
    expect(key2).not.toBe(key1);
  });

  it("omits If-Match when no etag is stored", async () => {
    const calls = mockFetch();
    await mutate("/api/v1/x", { method: "DELETE" });
    expect(new Headers(calls[0]!.init.headers).get("If-Match")).toBeNull();
  });

  it("explicit headers win over generated ones", async () => {
    const calls = mockFetch();
    await mutate(
      "/api/v1/x",
      { method: "POST" },
      { etag: 1, idempotencyKey: "a", headers: { "If-Match": '"9"' } }
    );
    const h = new Headers(calls[0]!.init.headers);
    expect(h.get("If-Match")).toBe('"9"');
    expect(h.get("Idempotency-Key")).toBe("a");
  });
});

describe("readEtag", () => {
  it("reads etag from an ApiResult response and from a Response", async () => {
    mockFetch(200, { data: { id: "1" } }, { etag: '"12"' });
    const res = await mutate("/api/v1/x", { method: "POST" });
    expect(readEtag(res)).toBe('"12"');
    expect(readEtag(res.response)).toBe('"12"');
    expect(readEtag(new Response("", { status: 200 }))).toBeUndefined();
  });
});
