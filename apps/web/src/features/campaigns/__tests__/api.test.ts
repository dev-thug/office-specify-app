import { afterEach, describe, expect, it, vi } from "vitest";

import {
  createCampaign,
  createLaunch,
  createRevision,
  pauseLaunch,
  cancelLaunch,
  requestLaunchApproval,
  runPreflight,
} from "@/lib/api/campaigns";
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

function problemBody(code: string, status: number) {
  return {
    type: `urn:outreach:problem:${code.toLowerCase().replaceAll("_", "-")}`,
    title: code,
    status,
    code,
    request_id: "req_test",
    retryable: false,
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("campaigns API", () => {
  it("createCampaign — POST /api/v1/campaigns + Idempotency-Key + B.3.6 본문", async () => {
    const calls = mockFetch(201, {
      data: { campaign_id: "c1", revision_id: "r1", status: "draft" },
    });
    const res = await createCampaign(
      {
        name: "테스트",
        mail_account_id: "m1",
        delivery_channel_id: "ch1",
        timezone: "Asia/Seoul",
        schedule_rules: { weekdays: [1, 2], window_start: "09:00" },
        steps: [{ step_order: 1, template_version_id: "v1", delay_hours: 0 }],
        stop_on_reply: true,
        stop_on_booking: true,
      },
      { idempotencyKey: "key-1" }
    );
    expect(calls[0]!.url).toBe("/api/v1/campaigns");
    expect(calls[0]!.init.method).toBe("POST");
    const h = new Headers(calls[0]!.init.headers);
    expect(h.get("Idempotency-Key")).toBe("key-1");
    const body = JSON.parse(String(calls[0]!.init.body));
    expect(body.delivery_channel_id).toBe("ch1");
    expect(body.steps[0].template_version_id).toBe("v1");
    expect(res.data.campaign_id).toBe("c1");
  });

  it("createRevision — POST /api/v1/campaigns/{id}/revisions", async () => {
    const calls = mockFetch(201, { data: { revision_id: "r2" } });
    await createRevision(
      "camp-1",
      {
        delivery_channel_id: "ch1",
        timezone: "Asia/Seoul",
        schedule_rules: { weekdays: [1] },
        steps: [{ step_order: 1, template_version_id: "v1", delay_hours: 24 }],
        stop_on_reply: true,
        stop_on_booking: false,
      },
      { idempotencyKey: "k" }
    );
    expect(calls[0]!.url).toBe("/api/v1/campaigns/camp-1/revisions");
    const body = JSON.parse(String(calls[0]!.init.body));
    expect(body.stop_on_booking).toBe(false);
    expect(body).not.toHaveProperty("mail_account_id");
  });

  it("runPreflight — POST /preflight, 멱등 키 없음(부작용 없는 검사)", async () => {
    const calls = mockFetch(200, {
      data: { recipients: [], is_execution_authorization: false },
    });
    await runPreflight("c1", { revision_id: "r1", email_address_ids: ["e1"] });
    expect(calls[0]!.url).toBe("/api/v1/campaigns/c1/preflight");
    expect(calls[0]!.init.method).toBe("POST");
    expect(
      new Headers(calls[0]!.init.headers).get("Idempotency-Key")
    ).toBeNull();
    expect(JSON.parse(String(calls[0]!.init.body)).email_address_ids).toEqual([
      "e1",
    ]);
  });

  it("createLaunch — POST /launches + 멱등 키(비동기 202 작업)", async () => {
    const calls = mockFetch(202, {
      data: { launch_id: "l1", status: "building" },
    });
    await createLaunch(
      "c1",
      { revision_id: "r1", email_address_ids: ["e1", "e2"] },
      { idempotencyKey: "freeze-key" }
    );
    expect(calls[0]!.url).toBe("/api/v1/campaigns/c1/launches");
    expect(new Headers(calls[0]!.init.headers).get("Idempotency-Key")).toBe(
      "freeze-key"
    );
  });

  it("requestLaunchApproval — POST /campaign-launches/{id}/approval-requests + expected_manifest_hash", async () => {
    const calls = mockFetch(201, {
      data: { approval_request_id: "a1", status: "pending" },
    });
    await requestLaunchApproval(
      "l1",
      { expected_manifest_hash: "ab".repeat(32), reason: "검토 완료" },
      { idempotencyKey: "k2" }
    );
    expect(calls[0]!.url).toBe(
      "/api/v1/campaign-launches/l1/approval-requests"
    );
    const body = JSON.parse(String(calls[0]!.init.body));
    expect(body.expected_manifest_hash).toBe("ab".repeat(32));
  });

  it("pauseLaunch/cancelLaunch — If-Match 헤더로 버전 검사", async () => {
    const calls = mockFetch();
    await pauseLaunch("l1", { reason: "중지" }, { etag: 5, idempotencyKey: "p" });
    await cancelLaunch("l1", {}, { etag: '"7"', idempotencyKey: "c" });
    expect(calls[0]!.url).toBe("/api/v1/campaign-launches/l1/pause");
    expect(new Headers(calls[0]!.init.headers).get("If-Match")).toBe('"5"');
    expect(calls[1]!.url).toBe("/api/v1/campaign-launches/l1/cancel");
    expect(new Headers(calls[1]!.init.headers).get("If-Match")).toBe('"7"');
  });

  it("422 PROVIDER_POLICY_BLOCKED → ApiError.code 유지", async () => {
    mockFetch(422, problemBody("PROVIDER_POLICY_BLOCKED", 422), {
      "content-type": "application/problem+json",
    });
    const err = await runPreflight("c1", {
      revision_id: "r",
      email_address_ids: [],
    }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).code).toBe("PROVIDER_POLICY_BLOCKED");
    expect((err as ApiError).status).toBe(422);
  });
});
