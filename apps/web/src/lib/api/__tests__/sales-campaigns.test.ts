import { afterEach, describe, expect, it, vi } from "vitest";

import {
  createSalesCampaign,
  createSalesCampaignTargetDraft,
  queueSalesCampaignTarget,
  setDefaultSalesCampaignSender,
} from "../sales-campaigns";

function jsonResponse(status: number, data: unknown) {
  return new Response(JSON.stringify({ data }), {
    status,
    headers: { "content-type": "application/json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("sales campaign API", () => {
  it("creates a campaign from its brief without mailbox or channel selectors", async () => {
    const fetchMock = vi.fn<typeof fetch>(async (input, init) => {
      void input;
      void init;
      return jsonResponse(201, { campaign: { id: "campaign-1" }, targets: [] });
    });
    vi.stubGlobal("fetch", fetchMock);

    await createSalesCampaign(
      { name: "Renewal outreach", objective: "Book planning calls", meeting_type_id: null },
      { idempotencyKey: "create-key" }
    );

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(String(url)).toBe("/api/v1/sales-campaigns");
    expect(init?.method).toBe("POST");
    expect(JSON.parse(String(init?.body))).toEqual({
      name: "Renewal outreach",
      objective: "Book planning calls",
      meeting_type_id: null,
    });
  });

  it("creates independent target drafts with the selected recipient and exact copy", async () => {
    const fetchMock = vi.fn<typeof fetch>(async (input, init) => {
      void input;
      void init;
      return jsonResponse(201, { draft_id: "draft-1" });
    });
    vi.stubGlobal("fetch", fetchMock);

    await createSalesCampaignTargetDraft(
      "campaign-1",
      "target-1",
      {
        subject: "Planning for Northstar",
        body_text: "I saw Northstar opened its Toronto office.",
        template_version_id: "template-version-3",
        variables: { company: "Northstar" },
      },
      { idempotencyKey: "draft-key" }
    );

    expect(String(fetchMock.mock.calls[0]?.[0])).toBe(
      "/api/v1/sales-campaigns/campaign-1/targets/target-1/drafts"
    );
    expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({
      subject: "Planning for Northstar",
      body_text: "I saw Northstar opened its Toronto office.",
      template_version_id: "template-version-3",
      variables: { company: "Northstar" },
    });
  });

  it("binds queue requests to the approved hash and current target version", async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => {
      void _input;
      void _init;
      return jsonResponse(202, {
        submission: { outbound_message_id: "outbound-1", status: "queued" },
        batch_at: "2026-10-10T12:10:00Z",
      });
    });
    vi.stubGlobal("fetch", fetchMock);

    await queueSalesCampaignTarget(
      "campaign-1",
      "target-1",
      {
        outbound_message_id: "outbound-1",
        expected_payload_hash: "ab".repeat(32),
        expected_target_version: 7,
      },
      { idempotencyKey: "queue-key" }
    );

    expect(String(fetchMock.mock.calls[0]?.[0])).toBe(
      "/api/v1/sales-campaigns/campaign-1/targets/target-1/queue"
    );
    expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({
      outbound_message_id: "outbound-1",
      expected_payload_hash: "ab".repeat(32),
      expected_target_version: 7,
    });
  });

  it("sets the workspace default sender through the campaign sender endpoint", async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => {
      void _input;
      void _init;
      return jsonResponse(200, { sender: { smtp_profile_id: "smtp-1", ready: true } });
    });
    vi.stubGlobal("fetch", fetchMock);

    await setDefaultSalesCampaignSender("smtp-1", { idempotencyKey: "sender-key" });

    expect(String(fetchMock.mock.calls[0]?.[0])).toBe(
      "/api/v1/sales-campaigns/sender"
    );
    expect(fetchMock.mock.calls[0]?.[1]?.method).toBe("PUT");
    expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({
      smtp_profile_id: "smtp-1",
    });
  });
});
