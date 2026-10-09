import { afterEach, describe, expect, it, vi } from "vitest";

import {
  createClassificationReview,
  createReplyDraft,
  getEmailThread,
  patchThreadStatus,
  putAssignment,
  putReadState,
  requestAttachmentDownload,
} from "@/lib/api/inbox";
import { ApiError } from "@/lib/api/problem";

function mockFetch(
  status = 200,
  body: unknown = { data: { id: "x" } },
  headers: Record<string, string> = {}
) {
  const calls: { url: string; init: RequestInit }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init: init ?? {} });
      const contentType =
        status >= 400
          ? "application/problem+json"
          : "application/json";
      return new Response(JSON.stringify(body), {
        status,
        headers: { "content-type": contentType, ...headers },
      });
    })
  );
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("inbox API — 요청 형태", () => {
  it("GET /email-threads/{id} 가 상세를 조회한다", async () => {
    const calls = mockFetch();
    await getEmailThread("thread-1");
    expect(calls[0]!.url).toBe("/api/v1/email-threads/thread-1");
    expect((calls[0]!.init.method ?? "GET").toUpperCase()).toBe("GET");
  });

  it("putAssignment 는 PUT + If-Match + 멱등 키 + assigned_principal_id 를 보낸다", async () => {
    const calls = mockFetch();
    await putAssignment(
      "t1",
      { assigned_principal_id: "p-9" },
      { etag: 3, idempotencyKey: "assign-key" }
    );
    expect(calls[0]!.url).toBe("/api/v1/email-threads/t1/assignment");
    expect(calls[0]!.init.method).toBe("PUT");
    const h = new Headers(calls[0]!.init.headers);
    expect(h.get("If-Match")).toBe('"3"');
    expect(h.get("Idempotency-Key")).toBe("assign-key");
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      assigned_principal_id: "p-9",
    });
  });

  it("배정 해제는 null 을 명시적으로 보낸다", async () => {
    const calls = mockFetch();
    await putAssignment("t1", { assigned_principal_id: null }, { etag: '"4"' });
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      assigned_principal_id: null,
    });
    expect(new Headers(calls[0]!.init.headers).get("If-Match")).toBe('"4"');
  });

  it("putReadState 는 단조 갱신이라 If-Match 없이 본인 읽음 위치만 보낸다", async () => {
    const calls = mockFetch();
    await putReadState("t1", { last_read_message_id: "m-2" });
    expect(calls[0]!.url).toBe("/api/v1/email-threads/t1/read-state");
    expect(calls[0]!.init.method).toBe("PUT");
    const h = new Headers(calls[0]!.init.headers);
    expect(h.get("If-Match")).toBeNull();
    expect(h.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      last_read_message_id: "m-2",
    });
  });

  it("patchThreadStatus 는 PATCH + If-Match + status 만 보낸다", async () => {
    const calls = mockFetch();
    await patchThreadStatus(
      "t1",
      { status: "resolved" },
      { etag: 7, idempotencyKey: "status-key" }
    );
    expect(calls[0]!.url).toBe("/api/v1/email-threads/t1");
    expect(calls[0]!.init.method).toBe("PATCH");
    const h = new Headers(calls[0]!.init.headers);
    expect(h.get("If-Match")).toBe('"7"');
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      status: "resolved",
    });
  });

  it("createReplyDraft 는 B.3.10 문서 필드만 보낸다", async () => {
    const calls = mockFetch(201, { data: { id: "d-1" } });
    await createReplyDraft(
      "t1",
      {
        expected_last_inbound_id: "m-last",
        body_text: "답변입니다",
        body_html: null,
        visibility: "private",
      },
      { idempotencyKey: "draft-key" }
    );
    expect(calls[0]!.url).toBe("/api/v1/email-threads/t1/reply-drafts");
    expect(calls[0]!.init.method).toBe("POST");
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      expected_last_inbound_id: "m-last",
      body_text: "답변입니다",
      body_html: null,
      visibility: "private",
    });
    expect(new Headers(calls[0]!.init.headers).get("Idempotency-Key")).toBe(
      "draft-key"
    );
  });

  it("createClassificationReview 는 분류와 이유를 보낸다", async () => {
    const calls = mockFetch();
    await createClassificationReview("m-1", {
      classification: "unsubscribe_request",
      reason: "명시적 요청",
    });
    expect(calls[0]!.url).toBe(
      "/api/v1/email-messages/m-1/classification-reviews"
    );
    expect(calls[0]!.init.method).toBe("POST");
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      classification: "unsubscribe_request",
      reason: "명시적 요청",
    });
  });

  it("requestAttachmentDownload 는 POST 후 download_url 을 반환한다", async () => {
    mockFetch(200, {
      data: { download_url: "https://files.example/x?sig=1", expires_at: "2026-10-12T02:00:00Z" },
    });
    const res = await requestAttachmentDownload("att-1", {
      idempotencyKey: "dl-key",
    });
    expect(res.data.download_url).toContain("https://");
  });
});

describe("inbox API — 오류 매핑", () => {
  it("409 REPLY_CONTEXT_CHANGED problem+json 을 ApiError code 로 노출한다", async () => {
    mockFetch(409, {
      type: "urn:outreach:problem:reply-context-changed",
      title: "Reply context changed",
      status: 409,
      code: "REPLY_CONTEXT_CHANGED",
      detail: "새 답장이 도착했습니다",
      request_id: "req_x",
      retryable: false,
      violations: [],
    });
    await expect(
      createReplyDraft("t1", {
        expected_last_inbound_id: "m-1",
        body_text: "x",
      })
    ).rejects.toMatchObject({
      name: "ApiError",
      code: "REPLY_CONTEXT_CHANGED",
      status: 409,
      requestId: "req_x",
    });
  });

  it("403 MAILBOX_ACCESS_DENIED 도 ApiError 로 전파된다", async () => {
    mockFetch(403, {
      type: "urn:outreach:problem:mailbox-access-denied",
      title: "Mailbox access denied",
      status: 403,
      code: "MAILBOX_ACCESS_DENIED",
      violations: [],
    });
    await expect(getEmailThread("t1")).rejects.toBeInstanceOf(ApiError);
  });
});
