import { afterEach, describe, expect, it, vi } from "vitest";

import {
  confirmUnsubscribe,
  createSuppression,
  getUnsubscribePreview,
  requestSuppressionRelease,
  reviewCommunicationBasis,
  revokeCommunicationBasis,
} from "@/lib/api/compliance";
import { ApiError } from "@/lib/api/problem";

function mockFetch(
  status = 200,
  body: unknown = { data: {} },
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

function mockProblem(status: number, code: string) {
  return mockFetch(status, {
    type: `urn:outreach:problem:${code.toLowerCase().replaceAll("_", "-")}`,
    title: code,
    status,
    code,
    detail: "problem detail",
    request_id: "req_test",
    retryable: false,
  }, { "content-type": "application/problem+json" });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("compliance mutations", () => {
  it("creates a suppression via POST /api/v1/suppressions with an Idempotency-Key", async () => {
    const calls = mockFetch(201, { data: { id: "s1" } });
    await createSuppression(
      {
        address: "user@example.com",
        purpose_scope: "marketing",
        reason: "manual",
        evidence_ref: "ticket-42",
      },
      { idempotencyKey: "supp-key" }
    );
    expect(calls[0]!.url).toBe("/api/v1/suppressions");
    expect(calls[0]!.init.method).toBe("POST");
    const h = new Headers(calls[0]!.init.headers);
    expect(h.get("Idempotency-Key")).toBe("supp-key");
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      address: "user@example.com",
      purpose_scope: "marketing",
      reason: "manual",
      evidence_ref: "ticket-42",
    });
  });

  it("requests a release (never a direct release) via release-requests", async () => {
    const calls = mockFetch(201, { data: { approval_request_id: "ar1" } });
    const res = await requestSuppressionRelease(
      "s1",
      { reason: "re-consented", evidence_ref: "doc-9" },
      { idempotencyKey: "rel-key" }
    );
    expect(calls[0]!.url).toBe("/api/v1/suppressions/s1/release-requests");
    expect(calls[0]!.init.method).toBe("POST");
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      reason: "re-consented",
      evidence_ref: "doc-9",
    });
    expect(res.data.approval_request_id).toBe("ar1");
  });

  it("sends If-Match with the stored version on basis review and revoke", async () => {
    const calls = mockFetch(200, { data: { id: "b1", status: "valid" } });
    await reviewCommunicationBasis(
      "b1",
      { decision: "valid", valid_until: "2027-01-01T00:00:00Z" },
      { etag: 7, idempotencyKey: "rev-key" }
    );
    expect(calls[0]!.url).toBe("/api/v1/communication-bases/b1/reviews");
    const h = new Headers(calls[0]!.init.headers);
    expect(h.get("If-Match")).toBe('"7"');
    expect(h.get("Idempotency-Key")).toBe("rev-key");
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      decision: "valid",
      valid_until: "2027-01-01T00:00:00Z",
    });

    await revokeCommunicationBasis("b1", { reason: "stale" }, { etag: '"8"' });
    expect(calls[1]!.url).toBe("/api/v1/communication-bases/b1/revoke");
    expect(new Headers(calls[1]!.init.headers).get("If-Match")).toBe('"8"');
  });

  it("maps problem+json failures to ApiError with code and request_id", async () => {
    mockProblem(412, "VERSION_MISMATCH");
    await expect(
      reviewCommunicationBasis("b1", { decision: "valid" }, { etag: 1 })
    ).rejects.toMatchObject({
      name: "ApiError",
      code: "VERSION_MISMATCH",
      status: 412,
      requestId: "req_test",
    });
  });

  it("maps 403 FORBIDDEN without swallowing status", async () => {
    mockProblem(403, "FORBIDDEN");
    const err = await createSuppression({
      address: "a@b.co",
      purpose_scope: "all",
      reason: "manual",
    }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(403);
  });
});

describe("public unsubscribe (B.4.4)", () => {
  it("GET reads confirmation data without side effects", async () => {
    const calls = mockFetch(200, {
      data: {
        organization_name: "Example Corp",
        scope: "workspace_marketing",
        masked_address: "u***@example.com",
      },
    });
    const res = await getUnsubscribePreview("tok-abc");
    expect(calls[0]!.url).toBe("/public/v1/unsubscribe/tok-abc");
    expect(calls[0]!.init.method ?? "GET").toBe("GET");
    // GET에는 body가 없다 — 상태 변경 없음.
    expect(calls[0]!.init.body).toBeUndefined();
    expect(res.data.organization_name).toBe("Example Corp");
  });

  it("POSTs an empty object body and returns the unsubscribed result", async () => {
    const calls = mockFetch(200, {
      data: {
        status: "unsubscribed",
        scope: "workspace_marketing",
        message: "처리되었습니다",
      },
    });
    const res = await confirmUnsubscribe("tok-abc", {
      idempotencyKey: "unsub-key",
    });
    expect(calls[0]!.url).toBe("/public/v1/unsubscribe/tok-abc");
    expect(calls[0]!.init.method).toBe("POST");
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({});
    expect(new Headers(calls[0]!.init.headers).get("Idempotency-Key")).toBe(
      "unsub-key"
    );
    expect(res.data.status).toBe("unsubscribed");
  });

  it("repeated request on an already-processed token is still a 200 result", async () => {
    mockFetch(200, { data: { status: "unsubscribed" } });
    const res = await confirmUnsubscribe("tok-abc");
    expect(res.data.status).toBe("unsubscribed");
  });

  it("unknown token surfaces a generalized 404 problem", async () => {
    mockProblem(404, "NOT_FOUND");
    const err = await confirmUnsubscribe("bad").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(404);
  });

  it("encodes the token in the path", async () => {
    const calls = mockFetch(200, { data: {} });
    await getUnsubscribePreview("a/b?c");
    expect(calls[0]!.url).toBe("/public/v1/unsubscribe/a%2Fb%3Fc");
  });
});
