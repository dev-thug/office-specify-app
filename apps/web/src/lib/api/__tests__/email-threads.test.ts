import { afterEach, describe, expect, it, vi } from "vitest";

import { getEmailThread } from "@/lib/api/inbox";

afterEach(() => vi.unstubAllGlobals());

describe("email thread detail API", () => {
  it("normalizes the flat ThreadDetail response for the inbox view", async () => {
    const messages = [
      {
        id: "outbound-before",
        direction: "outbound",
        subject: "Question",
        message_at: "2026-10-10T09:00:00Z",
        classification: "neutral",
        classification_reviewed: false,
        attachments: [],
        participants: [],
      },
      {
        id: "inbound-1",
        direction: "inbound",
        subject: "Question",
        message_at: "2026-10-10T10:00:00Z",
        classification: "neutral",
        classification_reviewed: false,
        attachments: [],
        participants: [],
      },
      {
        id: "outbound-after",
        direction: "outbound",
        subject: "Re: Question",
        message_at: "2026-10-10T11:00:00Z",
        classification: "neutral",
        classification_reviewed: false,
        attachments: [],
        participants: [],
      },
    ];
    const flatDetail = {
      id: "thread-1",
      mail_account_id: "mailbox-1",
      contact_id: "contact-1",
      assigned_principal_id: null,
      status: "open",
      created_at: "2026-10-10T09:00:00Z",
      updated_at: "2026-10-10T11:00:00Z",
      version: 7,
      my_last_read_at: null,
      messages,
    };
    const fetcher = vi.fn(async () => new Response(
      JSON.stringify({ data: flatDetail }),
      { status: 200, headers: { "content-type": "application/json" } }
    ));
    vi.stubGlobal("fetch", fetcher);

    const result = await getEmailThread("thread-1");

    expect(fetcher).toHaveBeenCalledOnce();
    expect(result.data).toMatchObject({
      id: "thread-1",
      mail_account_id: "mailbox-1",
      thread: {
        id: "thread-1",
        mail_account_id: "mailbox-1",
        contact_id: "contact-1",
        assigned_principal_id: null,
        last_inbound_message_id: "inbound-1",
        subject: "Re: Question",
        status: "open",
        version: 7,
      },
      messages,
    });
  });

  it("preserves the server inbound anchor when equal timestamps favor another UUID", async () => {
    const lowerUuid = "00000000-0000-4000-8000-000000000001";
    const higherUuid = "ffffffff-ffff-4fff-8fff-ffffffffffff";
    const messageAt = "2026-10-10T10:00:00Z";
    const flatDetail = {
      id: "thread-2",
      mail_account_id: "mailbox-1",
      status: "open",
      created_at: messageAt,
      updated_at: messageAt,
      version: 2,
      last_inbound_message_id: lowerUuid,
      messages: [higherUuid, lowerUuid].map((id) => ({
        id,
        direction: "inbound",
        subject: "Re: Question",
        message_at: messageAt,
        classification: "neutral",
        classification_reviewed: false,
        attachments: [],
        participants: [],
      })),
    };
    vi.stubGlobal("fetch", vi.fn(async () => new Response(
      JSON.stringify({ data: flatDetail }),
      { status: 200, headers: { "content-type": "application/json" } }
    )));

    const result = await getEmailThread("thread-2");

    expect(result.data.thread.last_inbound_message_id).toBe(lowerUuid);
  });
});
