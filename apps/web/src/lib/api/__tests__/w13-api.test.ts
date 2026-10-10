/**
 * Task 13 API 래퍼 — 경로·본문·동시성 헤더(If-Match/Idempotency-Key) 검증.
 * problem+json 응답이 ApiError 로 매핑되는지도 확인한다.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "@/lib/api/problem";
import { getDashboardSummary } from "@/lib/api/dashboard";
import { getJob, putEmergencyStop, requestJobRetry } from "@/lib/api/operations";
import {
  createLocalCalendarConnection,
  createMemberInvitation,
  patchDeliveryChannel,
  putMailAccountGrant,
  requestPrincipalDisable,
  updatePrincipal,
} from "@/lib/api/settings";

function mockFetch(status = 200, body: unknown = { data: {} }) {
  const calls: { url: string; init: RequestInit }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init: init ?? {} });
      const isProblem = status >= 400;
      return new Response(JSON.stringify(body), {
        status,
        headers: {
          "content-type": isProblem ? "application/problem+json" : "application/json",
        },
      });
    })
  );
  return calls;
}

function headersOf(call: { init: RequestInit }) {
  return new Headers(call.init.headers);
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("경로·헤더", () => {
  it("PUT /operations/emergency-stop 은 If-Match + Idempotency-Key + enabled/reason 본문", async () => {
    const calls = mockFetch(200, { data: { enabled: true } });
    await putEmergencyStop(
      { enabled: true, reason: "공격 대응" },
      { etag: 12, idempotencyKey: "stop-1" }
    );
    expect(calls[0]!.url).toBe("/api/v1/operations/emergency-stop");
    expect(calls[0]!.init.method).toBe("PUT");
    const h = headersOf(calls[0]!);
    expect(h.get("If-Match")).toBe('"12"');
    expect(h.get("Idempotency-Key")).toBe("stop-1");
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      enabled: true,
      reason: "공격 대응",
    });
  });

  it("POST /jobs/{id}/retry-requests 는 멱등 키를 보낸다", async () => {
    const calls = mockFetch(202, { data: { job_id: "j1" } });
    await requestJobRetry("job-9", { reason: "dead 작업 재처리" }, { idempotencyKey: "r-1" });
    expect(calls[0]!.url).toBe("/api/v1/jobs/job-9/retry-requests");
    expect(calls[0]!.init.method).toBe("POST");
    expect(headersOf(calls[0]!).get("Idempotency-Key")).toBe("r-1");
  });

  it("GET /jobs/{id} 는 인코딩된 경로로 호출한다", async () => {
    const calls = mockFetch(200, { data: { id: "a/b" } });
    await getJob("a/b");
    expect(calls[0]!.url).toBe("/api/v1/jobs/a%2Fb");
  });

  it("GET /dashboard/summary?scope=mine sends the backend's caller-owned scope", async () => {
    const calls = mockFetch(200, { data: { generated_at: "x" } });
    await getDashboardSummary({ scope: "mine" });
    expect(calls[0]!.url).toBe("/api/v1/dashboard/summary?scope=mine");
  });

  it("PATCH /principals/{id} 는 role 본문 + If-Match", async () => {
    const calls = mockFetch(200, { data: { id: "p1" } });
    await updatePrincipal("p1", { role: "manager" }, { etag: 3, idempotencyKey: "k" });
    expect(calls[0]!.url).toBe("/api/v1/principals/p1");
    expect(calls[0]!.init.method).toBe("PATCH");
    expect(headersOf(calls[0]!).get("If-Match")).toBe('"3"');
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({ role: "manager" });
  });

  it("POST /member-invitations 는 email+role 본문", async () => {
    const calls = mockFetch(201, { data: { id: "i1" } });
    await createMemberInvitation({ email: "a@b.c", role: "member" });
    expect(calls[0]!.url).toBe("/api/v1/member-invitations");
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      email: "a@b.c",
      role: "member",
    });
  });

  it("PUT grants 기존 행은 If-Match, 신규는 If-None-Match:*", async () => {
    const calls = mockFetch(200, { data: {} });
    const grants = {
      can_read: true,
      can_compose: true,
      can_send: false,
      can_approve: false,
      can_assign: false,
    };
    await putMailAccountGrant("ma1", "p1", grants, { etag: 7 });
    expect(calls[0]!.url).toBe("/api/v1/mail-accounts/ma1/grants/p1");
    expect(calls[0]!.init.method).toBe("PUT");
    expect(headersOf(calls[0]!).get("If-Match")).toBe('"7"');
    expect(headersOf(calls[0]!).get("If-None-Match")).toBeNull();

    await putMailAccountGrant("ma1", "p2", grants, { create: true });
    expect(headersOf(calls[1]!).get("If-None-Match")).toBe("*");
    expect(JSON.parse(String(calls[1]!.init.body))).toEqual(grants);
  });

  it("PATCH /delivery-channels/{id} 는 enabled 만 보내고 If-Match 를 쓴다", async () => {
    const calls = mockFetch(200, { data: { enabled: true } });
    await patchDeliveryChannel("ch1", { enabled: true }, { etag: 2 });
    expect(calls[0]!.init.method).toBe("PATCH");
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({ enabled: true });
    expect(headersOf(calls[0]!).get("If-Match")).toBe('"2"');
  });

  it("POST /principals/{id}/disable-requests 는 정책 본문을 보낸다", async () => {
    const calls = mockFetch(202, { data: { job_id: "j" } });
    await requestPrincipalDisable("p1", {
      pending_work_policy: "reassign",
      reassign_to: "p2",
    });
    expect(calls[0]!.url).toBe("/api/v1/principals/p1/disable-requests");
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      pending_work_policy: "reassign",
      reassign_to: "p2",
    });
  });

  it("POST /calendar-connections/local creates the Office internal calendar without a provider payload", async () => {
    const calls = mockFetch(201, { data: {
      id: "calendar-1",
      provider: "office_calendar",
      host_principal_id: "human-1",
      connection_id: "connection-1",
      provider_calendar_id: "office_calendar",
      status: "active",
      include_in_busy: true,
      version: 1,
      created_at: "2026-10-10T00:00:00Z",
    } });
    await createLocalCalendarConnection({ idempotencyKey: "local-calendar-1" });
    expect(calls[0]!.url).toBe("/api/v1/calendar-connections/local");
    expect(calls[0]!.init.method).toBe("POST");
    expect(headersOf(calls[0]!).get("Idempotency-Key")).toBe("local-calendar-1");
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({});
  });
});

describe("에러 매핑", () => {
  it("problem+json 은 code·retryable·request_id 를 보존한 ApiError", async () => {
    mockFetch(412, {
      type: "about:blank",
      title: "version mismatch",
      status: 412,
      code: "VERSION_MISMATCH",
      retryable: true,
      request_id: "req-9",
    });
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              title: "version mismatch",
              status: 412,
              code: "VERSION_MISMATCH",
              retryable: true,
              request_id: "req-9",
            }),
            {
              status: 412,
              headers: { "content-type": "application/problem+json", "x-request-id": "req-9" },
            }
          )
      )
    );
    try {
      await putEmergencyStop({ enabled: false, reason: "x" }, { etag: 1 });
      expect.unreachable();
    } catch (err) {
      expect(err).toBeInstanceOf(ApiError);
      const apiErr = err as ApiError;
      expect(apiErr.code).toBe("VERSION_MISMATCH");
      expect(apiErr.status).toBe(412);
      expect(apiErr.problem.retryable).toBe(true);
    }
  });
});
