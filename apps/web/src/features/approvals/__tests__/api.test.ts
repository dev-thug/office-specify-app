import { afterEach, describe, expect, it, vi } from "vitest";

import {
  getApproval,
  postApprovalDecision,
  revokeApproval,
} from "@/lib/api/approvals";
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
      return new Response(JSON.stringify(body), {
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

describe("approvals API", () => {
  it("getApproval — GET /api/v1/approvals/{id}", async () => {
    const calls = mockFetch(200, {
      data: { id: "a1", status: "pending" },
      meta: { request_id: "r" },
    });
    const res = await getApproval("a1");
    expect(calls[0]!.url).toBe("/api/v1/approvals/a1");
    expect(res.data.status).toBe("pending");
  });

  it("postApprovalDecision — If-Match + Idempotency-Key + expected_snapshot_hash (B.3.9)", async () => {
    const calls = mockFetch(200, {
      data: { approval_status: "approved", execution_status: "queued" },
    });
    const res = await postApprovalDecision(
      "a1",
      {
        decision: "approved",
        expected_snapshot_hash: "cd".repeat(32),
        expected_export_hash: "ef".repeat(32),
        reason: "확인",
      },
      { etag: 3, idempotencyKey: "dec-1" }
    );
    expect(calls[0]!.url).toBe("/api/v1/approvals/a1/decisions");
    expect(calls[0]!.init.method).toBe("POST");
    const h = new Headers(calls[0]!.init.headers);
    expect(h.get("If-Match")).toBe('"3"');
    expect(h.get("Idempotency-Key")).toBe("dec-1");
    const body = JSON.parse(String(calls[0]!.init.body));
    expect(body.decision).toBe("approved");
    expect(body.expected_snapshot_hash).toBe("cd".repeat(32));
    expect(body.expected_export_hash).toBe("ef".repeat(32));
    // execution_status는 비동기 실행 — 발송 완료가 아니다.
    expect(res.data.execution_status).toBe("queued");
  });

  it("revokeApproval — POST /{id}/revoke + If-Match", async () => {
    const calls = mockFetch();
    await revokeApproval("a1", { reason: "철회" }, { etag: 2 });
    expect(calls[0]!.url).toBe("/api/v1/approvals/a1/revoke");
    expect(new Headers(calls[0]!.init.headers).get("If-Match")).toBe('"2"');
  });

  it("409 SNAPSHOT_MISMATCH → ApiError.code", async () => {
    mockFetch(
      409,
      {
        type: "urn:outreach:problem:snapshot-mismatch",
        title: "snapshot changed",
        status: 409,
        code: "SNAPSHOT_MISMATCH",
        retryable: false,
      },
      { "content-type": "application/problem+json" }
    );
    const err = await postApprovalDecision(
      "a1",
      { decision: "approved", expected_snapshot_hash: "x" },
      { etag: 1 }
    ).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).code).toBe("SNAPSHOT_MISMATCH");
  });

  it("403 SELF_APPROVAL_FORBIDDEN → ApiError.code", async () => {
    mockFetch(
      403,
      {
        type: "urn:outreach:problem:self-approval-forbidden",
        title: "self approval",
        status: 403,
        code: "SELF_APPROVAL_FORBIDDEN",
      },
      { "content-type": "application/problem+json" }
    );
    const err = await postApprovalDecision(
      "a1",
      { decision: "approved", expected_snapshot_hash: "x" },
      { etag: 1 }
    ).catch((e: unknown) => e);
    expect((err as ApiError).code).toBe("SELF_APPROVAL_FORBIDDEN");
  });
});
