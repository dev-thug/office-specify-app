import { NextIntlClientProvider } from "next-intl";
import { fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import enCommon from "../../../../messages/en/common.json";
import enInbox from "../../../../messages/en/inbox.json";
import enSending from "../../../../messages/en/sending.json";
import enStatus from "../../../../messages/en/status.json";
import { InboxScreen } from "@/features/inbox/inbox-screen";
import type { MessageDraftResponse } from "@/lib/api/sending";
import {
  SavedDraftDetailScreen,
  SavedDraftsScreen,
} from "../saved-drafts-screen";

vi.mock("../draft-sending-workflow", () => ({
  DraftSendingWorkflow: ({
    draftId,
    mailAccountId,
  }: {
    draftId: string;
    mailAccountId: string;
  }) => (
    <div
      data-testid="draft-sending-workflow"
      data-draft-id={draftId}
      data-mail-account-id={mailAccountId}
    />
  ),
}));

const messages = {
  common: enCommon,
  inbox: enInbox,
  sending: enSending,
  status: enStatus,
};

function renderApp(ui: ReactElement) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <NextIntlClientProvider locale="en" messages={messages}>
        {ui}
      </NextIntlClientProvider>
    </QueryClientProvider>
  );
}

const draftRows = [
  {
    draft_id: "draft-1",
    mail_account_id: "mailbox-1",
    owner_principal_id: "person-1",
    visibility: "private",
    thread_id: null,
    outbound_message_id: null,
    purpose: "marketing",
    subject: "New product announcement",
    status: "draft",
    version: 1,
    created_at: "2026-10-09T10:00:00Z",
    updated_at: "2026-10-09T11:00:00Z",
    body_text: "This content belongs to the detail endpoint.",
  },
  {
    draft_id: "draft-2",
    mail_account_id: "mailbox-1",
    owner_principal_id: "person-1",
    visibility: "mailbox",
    thread_id: null,
    outbound_message_id: "outbound-2",
    purpose: "transactional",
    subject: "Invoice receipt",
    status: "frozen",
    version: 2,
    created_at: "2026-10-09T09:00:00Z",
    updated_at: "2026-10-09T12:00:00Z",
  },
];

const fullDraft: MessageDraftResponse = {
  draft_id: "draft-1",
  mail_account_id: "mailbox-1",
  owner_principal_id: "person-1",
  visibility: "private",
  thread_id: null,
  outbound_message_id: null,
  to_email_address_id: "address-1",
  purpose: "marketing",
  subject: "New product announcement",
  body_text: "Saved body content.",
  body_html: null,
  variables: {},
  status: "draft",
  version: 1,
  created_at: "2026-10-09T10:00:00Z",
  updated_at: "2026-10-09T11:00:00Z",
};

function installApi() {
  const calls: { url: string; init: RequestInit }[] = [];
  let listCall = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      calls.push({ url: `${url.pathname}${url.search}`, init: init ?? {} });
      if (url.pathname === "/api/v1/mail-accounts") {
        return jsonResponse(200, [
          {
            id: "mailbox-1",
            name: "Main mailbox",
            primary_address: "team@example.test",
            status: "active",
          },
        ]);
      }
      if (url.pathname === "/api/v1/email-threads") {
        return jsonResponse(200, []);
      }
      if (url.pathname === "/api/v1/message-drafts" && !url.searchParams.has("thread_id")) {
        listCall += 1;
        return listCall === 1
          ? jsonResponse(200, draftRows.slice(0, 1), {
              page: { next_cursor: "cursor-2", has_more: true },
            })
          : jsonResponse(200, draftRows.slice(1));
      }
      if (url.pathname === "/api/v1/message-drafts/draft-1") {
        return jsonResponse(200, fullDraft, { etag: '"1"' });
      }
      return jsonResponse(404, { title: `Unexpected request: ${url.pathname}` });
    })
  );
  return calls;
}

function jsonResponse(
  status: number,
  data: unknown,
  extra: { page?: unknown; etag?: string } = {}
) {
  const headers = new Headers({ "content-type": "application/json" });
  if (extra.etag) headers.set("etag", extra.etag);
  return new Response(
    JSON.stringify({ data, ...(extra.page ? { page: extra.page } : {}) }),
    { status, headers }
  );
}

afterEach(() => vi.unstubAllGlobals());

describe("saved message draft history", () => {
  it("links to saved messages from the Inbox header", async () => {
    installApi();
    renderApp(<InboxScreen />);

    const link = await screen.findByRole("link", { name: "Saved messages" });
    expect(link.getAttribute("href")).toBe("/app/inbox/drafts");
  });

  it("uses the cursor list, shows metadata only, and loads the next page", async () => {
    const calls = installApi();
    renderApp(<SavedDraftsScreen />);

    const firstLink = await screen.findByRole("link", {
      name: "New product announcement",
    });
    expect(firstLink.getAttribute("href")).toBe("/app/inbox/drafts/draft-1");
    expect(screen.getByText("Main mailbox")).toBeInTheDocument();
    expect(screen.queryByText("This content belongs to the detail endpoint.")).not.toBeInTheDocument();
    const firstUrl = new URL(calls.find((call) => call.url.startsWith("/api/v1/message-drafts?"))!.url, "http://localhost");
    expect(firstUrl.searchParams.get("limit")).toBe("50");

    fireEvent.click(screen.getByRole("button", { name: "Load more" }));
    await screen.findByRole("link", { name: "Invoice receipt" });

    const listCalls = calls.filter((call) => call.url.startsWith("/api/v1/message-drafts?"));
    expect(listCalls).toHaveLength(2);
    expect(new URL(listCalls[1]!.url, "http://localhost").searchParams.get("cursor")).toBe("cursor-2");
  });

  it("loads the chosen draft first and passes its authorized mailbox to the shared workflow", async () => {
    const calls = installApi();
    renderApp(<SavedDraftDetailScreen draftId="draft-1" />);

    const workflow = await screen.findByTestId("draft-sending-workflow");
    expect(workflow.getAttribute("data-draft-id")).toBe("draft-1");
    expect(workflow.getAttribute("data-mail-account-id")).toBe("mailbox-1");
    expect(calls.some((call) => call.url === "/api/v1/message-drafts/draft-1")).toBe(true);
  });
});
