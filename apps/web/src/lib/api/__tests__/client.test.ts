import { afterEach, describe, expect, it, vi } from "vitest";

import {
  apiFetch,
  idempotencyKey,
  ifMatch,
  readCookie,
  CSRF_COOKIE,
  CSRF_HEADER,
  IDEMPOTENCY_HEADER,
  IF_MATCH_HEADER,
} from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";

function mockFetch(status = 200, body: unknown = { data: { ok: true }, meta: { request_id: "r" } }, headers: Record<string, string> = {}) {
  const calls: { url: string; init: RequestInit }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init: init ?? {} });
      return new Response(body === null ? "" : JSON.stringify(body), {
        status,
        headers: {
          "content-type": status >= 400 ? "application/problem+json" : "application/json",
          ...headers,
        },
      });
    })
  );
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
  // jsdom 쿠키 초기화
  document.cookie.split(";").forEach((c) => {
    document.cookie = `${c.trim().split("=")[0]}=;expires=Thu, 01 Jan 1970 00:00:00 GMT;path=/`;
  });
});

describe("apiFetch CSRF injection", () => {
  it("adds X-CSRF-Token from outreach_csrf cookie on non-GET", async () => {
    const calls = mockFetch();
    document.cookie = `${CSRF_COOKIE}=tok-123`;
    await apiFetch("/api/v1/things", { method: "POST", json: { a: 1 } });
    const h = new Headers(calls[0]!.init.headers);
    expect(h.get(CSRF_HEADER)).toBe("tok-123");
    expect(h.get("content-type")).toBe("application/json");
  });

  it("does not add CSRF header on GET", async () => {
    const calls = mockFetch();
    document.cookie = `${CSRF_COOKIE}=tok-123`;
    await apiFetch("/api/v1/things");
    expect(new Headers(calls[0]!.init.headers).get(CSRF_HEADER)).toBeNull();
  });

  it("does not override an explicit CSRF header", async () => {
    const calls = mockFetch();
    document.cookie = `${CSRF_COOKIE}=cookie-token`;
    await apiFetch("/api/v1/things", {
      method: "DELETE",
      headers: { [CSRF_HEADER]: "explicit" },
    });
    expect(new Headers(calls[0]!.init.headers).get(CSRF_HEADER)).toBe("explicit");
  });
});

describe("idempotencyKey / ifMatch", () => {
  it("generates distinct UUID-shaped keys", () => {
    const a = idempotencyKey();
    const b = idempotencyKey();
    expect(a).not.toBe(b);
    expect(a).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("ifMatch wraps the version in ETag quotes", () => {
    expect(ifMatch(7)).toEqual({ [IF_MATCH_HEADER]: '"7"' });
    expect(ifMatch('"9"')).toEqual({ [IF_MATCH_HEADER]: '"9"' });
  });

  it("caller can attach idempotency key header", async () => {
    const calls = mockFetch();
    const key = idempotencyKey();
    await apiFetch("/api/v1/x", {
      method: "POST",
      json: {},
      headers: { [IDEMPOTENCY_HEADER]: key },
    });
    expect(new Headers(calls[0]!.init.headers).get(IDEMPOTENCY_HEADER)).toBe(key);
  });
});

describe("apiFetch envelope + errors", () => {
  it("unwraps {data, meta, page}", async () => {
    mockFetch(200, {
      data: [1, 2],
      meta: { request_id: "req-9" },
      page: { next_cursor: "c2", has_more: true },
    });
    const res = await apiFetch<number[]>("/api/v1/list");
    expect(res.data).toEqual([1, 2]);
    expect(res.meta?.request_id).toBe("req-9");
    expect(res.page?.next_cursor).toBe("c2");
  });

  it("throws ApiError for problem+json error responses", async () => {
    mockFetch(403, {
      title: "Forbidden",
      status: 403,
      code: "FORBIDDEN",
      request_id: "req-e",
    });
    const err = await apiFetch("/api/v1/x", { method: "POST" }).catch((e) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({
      code: "FORBIDDEN",
      status: 403,
      requestId: "req-e",
    });
  });
});

describe("readCookie", () => {
  it("reads and decodes cookie values", () => {
    document.cookie = "outreach_session=abc%20def";
    expect(readCookie("outreach_session")).toBe("abc def");
    expect(readCookie("missing")).toBeUndefined();
  });
});
