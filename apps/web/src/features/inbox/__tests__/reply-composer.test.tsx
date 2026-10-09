import { NextIntlClientProvider } from "next-intl";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ReactElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ReplyComposer } from "../reply-composer";
import enInbox from "../../../../messages/en/inbox.json";
import enCommon from "../../../../messages/en/common.json";

const messages = { inbox: enInbox, common: enCommon };

const R = (ui: ReactElement) =>
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      {ui}
    </NextIntlClientProvider>
  );

function mockFetchSequence(
  handlers: Array<() => Response | Promise<Response>>
) {
  const calls: { url: string; init: RequestInit }[] = [];
  let i = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init: init ?? {} });
      const handler = handlers[Math.min(i, handlers.length - 1)]!;
      i += 1;
      return handler();
    })
  );
  return calls;
}

const jsonResponse = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });

const baseProps = {
  threadId: "t-1",
  expectedLastInboundId: "m-last",
  inferredTo: "sender@example.dev",
  blockReason: null,
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("ReplyComposer", () => {
  it("초안 생성 — 문서 필드 + 논리 작업 멱등 키를 보낸다", async () => {
    const calls = mockFetchSequence([
      () => jsonResponse(201, { data: { id: "d-1", to_address: "sender@example.dev" } }),
    ]);
    const onCreated = vi.fn();
    R(<ReplyComposer {...baseProps} onCreated={onCreated} />);

    fireEvent.change(screen.getByLabelText(/Body/), {
      target: { value: "회신 본문" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create draft" }));

    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]!.url).toBe("/api/v1/email-threads/t-1/reply-drafts");
    const headers = new Headers(calls[0]!.init.headers);
    expect(headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      expected_last_inbound_id: "m-last",
      body_text: "회신 본문",
      body_html: null,
      visibility: "private",
    });
    await waitFor(() => expect(onCreated).toHaveBeenCalled());
    // 생성 후 — "accepted ≠ delivered" 안내가 보인다
    expect(await screen.findByText(/Reply draft created/)).toBeInTheDocument();
    expect(screen.getByText(/accepted.*not.*delivered/i)).toBeInTheDocument();
  });

  it("네트워크 실패 후 재시도는 같은 멱등 키를 재사용한다", async () => {
    let fail = true;
    const calls = mockFetchSequence([
      () => {
        if (fail) {
          fail = false;
          return Promise.reject(new TypeError("network"));
        }
        return jsonResponse(201, { data: { id: "d-1" } });
      },
      () => jsonResponse(201, { data: { id: "d-1" } }),
    ]);
    R(<ReplyComposer {...baseProps} />);

    fireEvent.change(screen.getByLabelText(/Body/), {
      target: { value: "다시 보낸다" },
    });
    const submit = screen.getByRole("button", { name: "Create draft" });
    fireEvent.click(submit);
    await waitFor(() => expect(calls).toHaveLength(1));

    fireEvent.click(submit);
    await waitFor(() => expect(calls).toHaveLength(2));
    const key1 = new Headers(calls[0]!.init.headers).get("Idempotency-Key");
    const key2 = new Headers(calls[1]!.init.headers).get("Idempotency-Key");
    expect(key1).toBe(key2);
  });

  it("409 REPLY_CONTEXT_CHANGED 를 부모에 알린다", async () => {
    mockFetchSequence([
      () =>
        jsonResponse(409, {
          type: "urn:outreach:problem:reply-context-changed",
          title: "Reply context changed",
          status: 409,
          code: "REPLY_CONTEXT_CHANGED",
          violations: [],
        }),
    ]);
    const onReplyError = vi.fn();
    R(<ReplyComposer {...baseProps} onReplyError={onReplyError} />);
    fireEvent.change(screen.getByLabelText(/Body/), {
      target: { value: "본문" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create draft" }));
    await waitFor(() =>
      expect(onReplyError).toHaveBeenCalledWith(
        expect.objectContaining({ code: "REPLY_CONTEXT_CHANGED" })
      )
    );
  });

  it.each([
    ["reconnect", /cannot create drafts until the mailbox is reconnected/i],
    ["collision", /review the new messages first/i],
    ["forbidden", /do not have compose permission/i],
  ] as const)("blockReason=%s 면 제출이 비활성화되고 이유를 보인다", (reason, text) => {
    R(<ReplyComposer {...baseProps} blockReason={reason} />);
    expect(screen.getByText(text)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Create draft" })
    ).toBeDisabled();
  });

  it("collision 차단 시 새 내용 보기 버튼으로 해소를 요청한다", () => {
    const onAck = vi.fn();
    R(
      <ReplyComposer
        {...baseProps}
        blockReason="collision"
        onAcknowledgeCollision={onAck}
      />
    );
    fireEvent.click(screen.getByRole("button", { name: "Show new content" }));
    expect(onAck).toHaveBeenCalled();
  });
});
