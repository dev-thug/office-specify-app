import { afterEach, describe, expect, it, vi } from "vitest";

import {
  createMessageDraft,
  freezeMessageDraft,
  listMessageDrafts,
  submitOutbound,
} from "@/lib/api/sending";

function mockFetch(status = 200, body: unknown = { data: {} }, headers: Record<string, string> = {}) {
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

afterEach(() => vi.unstubAllGlobals());

describe("sending API", () => {
  it("creates a correspondence draft with the exact mailbox and reply anchor", async () => {
    const calls = mockFetch(201, { data: { draft_id: "draft-1" } });
    await createMessageDraft(
      {
        mail_account_id: "mailbox-1",
        to_email_address_id: "address-1",
        purpose: "correspondence",
        subject: "Re: Hello",
        body_text: "Thanks for writing.",
        body_html: null,
        thread_id: "thread-1",
        reply_to_message_id: "inbound-1",
        expected_last_inbound_id: "inbound-1",
        visibility: "private",
      },
      { idempotencyKey: "draft-operation" }
    );

    expect(calls[0]!.url).toBe("/api/v1/message-drafts");
    expect(calls[0]!.init.method).toBe("POST");
    expect(new Headers(calls[0]!.init.headers).get("Idempotency-Key")).toBe(
      "draft-operation"
    );
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      mail_account_id: "mailbox-1",
      to_email_address_id: "address-1",
      purpose: "correspondence",
      subject: "Re: Hello",
      body_text: "Thanks for writing.",
      body_html: null,
      thread_id: "thread-1",
      reply_to_message_id: "inbound-1",
      expected_last_inbound_id: "inbound-1",
      visibility: "private",
    });
  });

  it("lists only drafts for the requested thread", async () => {
    const calls = mockFetch();
    await listMessageDrafts({ threadId: "thread/1", limit: 25 });

    const url = new URL(calls[0]!.url, "http://localhost");
    expect(url.pathname).toBe("/api/v1/message-drafts");
    expect(url.searchParams.get("thread_id")).toBe("thread/1");
    expect(url.searchParams.get("limit")).toBe("25");
  });

  it("freezes against the loaded draft version and keeps the same retry key", async () => {
    const calls = mockFetch(201, {
      data: {
        outbound_message_id: "outbound-1",
        status: "draft",
        payload_hash: "ab".repeat(32),
        version: 5,
      },
    });
    const input = {
      delivery_channel_id: "channel-1",
      meeting_type_id: "meeting-1",
    };
    await freezeMessageDraft("draft-1", input, {
      etag: 4,
      idempotencyKey: "freeze-operation",
    });
    await freezeMessageDraft("draft-1", input, {
      etag: 4,
      idempotencyKey: "freeze-operation",
    });

    expect(calls).toHaveLength(2);
    for (const call of calls) {
      expect(call.url).toBe("/api/v1/message-drafts/draft-1/freeze");
      const headers = new Headers(call.init.headers);
      expect(headers.get("If-Match")).toBe('"4"');
      expect(headers.get("Idempotency-Key")).toBe("freeze-operation");
      expect(JSON.parse(String(call.init.body))).toEqual(input);
    }
  });

  it("submits the reviewed payload hash with a stable key and no draft ETag", async () => {
    const calls = mockFetch(202, {
      data: {
        outbound_message_id: "outbound-1",
        status: "queued",
        job_id: "job-1",
        provider_accepted: false,
      },
    });
    const input = { expected_payload_hash: "cd".repeat(32) };
    await submitOutbound("outbound-1", input, {
      idempotencyKey: "submit-operation",
    });
    await submitOutbound("outbound-1", input, {
      idempotencyKey: "submit-operation",
    });

    expect(calls).toHaveLength(2);
    for (const call of calls) {
      expect(call.url).toBe("/api/v1/outbound-messages/outbound-1/submit");
      const headers = new Headers(call.init.headers);
      expect(headers.get("If-Match")).toBeNull();
      expect(headers.get("Idempotency-Key")).toBe("submit-operation");
      expect(JSON.parse(String(call.init.body))).toEqual(input);
    }
  });
});
