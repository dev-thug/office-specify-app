import { NextIntlClientProvider } from "next-intl";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import enCommon from "../../../../messages/en/common.json";
import enSending from "../../../../messages/en/sending.json";
import { DraftSendingWorkflow } from "../draft-sending-workflow";
import type {
  MessageDraftResponse,
  OutboundMessageResponse,
  OutboundPreflightResponse,
} from "@/lib/api/sending";

const messages = { common: enCommon, sending: enSending };

function renderWorkflow(ui: ReactElement) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <NextIntlClientProvider locale="en" messages={messages}>
        {ui}
      </NextIntlClientProvider>
    </QueryClientProvider>
  );
}

const draft = (overrides: Partial<MessageDraftResponse> = {}): MessageDraftResponse => ({
  draft_id: "draft-1",
  mail_account_id: "mailbox-1",
  owner_principal_id: "person-1",
  visibility: "private",
  thread_id: "thread-1",
  reply_to_message_id: "inbound-1",
  expected_last_inbound_id: "inbound-1",
  to_email_address_id: "address-1",
  purpose: "correspondence",
  subject: "Re: Hello",
  body_text: "Thanks for writing.",
  body_html: null,
  status: "draft",
  version: 4,
  created_at: "2026-10-09T00:00:00Z",
  updated_at: "2026-10-09T00:00:00Z",
  outbound_message_id: null,
  ...overrides,
});

const outbound = (
  status: string,
  overrides: Partial<OutboundMessageResponse> = {}
): OutboundMessageResponse => ({
  outbound_message_id: "outbound-1",
  status,
  failure_code: null,
  provider_accepted: status === "accepted" ? true : null,
  delivery_status: "unknown",
  automatic_resubmit_allowed: false,
  next_action: status === "unknown" ? "reconcile_with_provider" : "none",
  purpose: "correspondence",
  from_address: "team@example.test",
  to_address: "person@example.test",
  subject: "Re: Hello",
  body_text: "Thanks for writing.",
  body_html: null,
  not_before: null,
  accepted_at: null,
  version: 6,
  created_at: "2026-10-09T00:00:00Z",
  payload_hash: "cd".repeat(32),
  ...overrides,
});

const passingPreflight: OutboundPreflightResponse = {
  checked_at: "2026-10-09T00:00:00Z",
  is_execution_authorization: false,
  global_blocking_reasons: [],
  checks: [
    { name: "channel_enabled", passed: true, code: null },
    { name: "recipient_basis", passed: true, code: null },
    { name: "approval_state", passed: true, code: null },
  ],
};

const noChannel = {
  id: "channel-other",
  mail_account_id: "mailbox-other",
  sender_identity_id: "sender-other",
  connection_id: "connection-other",
  provider: "delivery_provider",
  purpose: "correspondence",
  name: "Other mailbox",
  policy_status: "allowed",
  enabled: true,
  capabilities: {},
  internal_daily_cap: null,
  version: 1,
  created_at: "2026-10-09T00:00:00Z",
};

const channel = {
  ...noChannel,
  id: "channel-1",
  mail_account_id: "mailbox-1",
  sender_identity_id: "sender-1",
  connection_id: "connection-1",
  name: "Inbox correspondence",
};

const marketingChannel = {
  ...channel,
  id: "channel-marketing",
  purpose: "marketing",
  name: "Inbox marketing",
};

const meetingType = {
  id: "meeting-1",
  host_principal_id: "person-1",
  calendar_connection_id: "calendar-1",
  name: "Sales introduction",
  duration_minutes: 30,
  buffer_before_minutes: 0,
  buffer_after_minutes: 0,
  minimum_notice_minutes: 60,
  booking_horizon_days: 30,
  status: "active",
};

function mockSendingApi(options: {
  draft?: MessageDraftResponse;
  drafts?: Partial<MessageDraftResponse>[];
  outbound?: OutboundMessageResponse;
  preflight?: OutboundPreflightResponse;
  approvedPreflight?: OutboundPreflightResponse;
  freezeFailureCount?: number;
  submitFailureCount?: number;
  mailAccountActions?: string[];
}) {
  const calls: { url: string; init: RequestInit }[] = [];
  let freezeFailures = options.freezeFailureCount ?? 0;
  let submitFailures = options.submitFailureCount ?? 0;
  let currentOutbound = options.outbound;
  let approvalRequested = false;

  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      calls.push({ url: `${url.pathname}${url.search}`, init: init ?? {} });
      if (url.pathname === "/api/v1/me") {
        return jsonResponse(200, { principal_id: "person-1", kind: "human" });
      }
      if (url.pathname === "/api/v1/mail-accounts") {
        return jsonResponse(200, [
          {
            id: "mailbox-1",
            name: "Inbox mailbox",
            primary_address: "team@example.test",
            status: "active",
            allowed_actions: options.mailAccountActions ?? [
              "mail.read",
              "mail.compose",
              "mail.send",
            ],
          },
        ]);
      }
      if (url.pathname === "/api/v1/delivery-channels") {
        return jsonResponse(200, [noChannel, channel, marketingChannel]);
      }
      if (url.pathname === "/api/v1/meeting-types") {
        return jsonResponse(200, [meetingType]);
      }
      if (url.pathname === "/api/v1/approvals") {
        return jsonResponse(200, approvalRequested ? [{
          id: "approval-1",
          action_type: "message_submit",
          status: "approved",
          requested_by: "person-1",
          target_id: "outbound-1",
          outbound_message_id: "outbound-1",
          version: 1,
        }] : []);
      }
      if (url.pathname === "/api/v1/message-drafts" && init?.method !== "POST") {
        return jsonResponse(200, options.drafts ?? []);
      }
      if (url.pathname === "/api/v1/message-drafts/draft-1") {
        return jsonResponse(200, options.draft ?? draft(), { etag: '"4"' });
      }
      if (url.pathname.endsWith("/freeze")) {
        if (freezeFailures > 0) {
          freezeFailures -= 1;
          return Promise.reject(new TypeError("network"));
        }
        const frozen = draft({ status: "frozen", outbound_message_id: "outbound-1" });
        options.draft = frozen;
        currentOutbound ??= outbound("draft");
        return jsonResponse(201, {
          outbound_message_id: "outbound-1",
          status: "draft",
          payload_hash: "cd".repeat(32),
          version: 6,
        });
      }
      if (url.pathname === "/api/v1/outbound-messages/outbound-1/preflight") {
        return jsonResponse(
          200,
          approvalRequested && options.approvedPreflight
            ? options.approvedPreflight
            : options.preflight ?? passingPreflight
        );
      }
      if (url.pathname === "/api/v1/outbound-messages/outbound-1/approval-requests") {
        approvalRequested = true;
        return jsonResponse(201, {
          approval_request_id: "approval-1",
          status: "pending",
          expires_at: "2026-10-16T00:00:00Z",
          version: 1,
        });
      }
      if (url.pathname === "/api/v1/outbound-messages/outbound-1/submit") {
        if (submitFailures > 0) {
          submitFailures -= 1;
          return Promise.reject(new TypeError("network"));
        }
        currentOutbound = outbound("queued");
        return jsonResponse(202, {
          outbound_message_id: "outbound-1",
          status: "queued",
          job_id: "job-1",
          provider_accepted: false,
        });
      }
      if (url.pathname === "/api/v1/outbound-messages/outbound-1") {
        return jsonResponse(200, currentOutbound ?? options.outbound ?? outbound("draft"));
      }
      if (url.pathname === "/api/v1/outbound-messages/outbound-1/cancel") {
        currentOutbound = outbound("canceled");
        return jsonResponse(200, {
          outbound_message_id: "outbound-1",
          status: "canceled",
          version: 7,
        });
      }
      return jsonResponse(404, { title: `Unexpected request: ${url.pathname}` });
    })
  );
  return calls;
}

function jsonResponse(status: number, data: unknown, headers: Record<string, string> = {}) {
  return new Response(JSON.stringify({ data }), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

afterEach(() => vi.unstubAllGlobals());

describe("DraftSendingWorkflow", () => {
  it("resumes a thread draft after reload and loads its full server record", async () => {
    const calls = mockSendingApi({
      drafts: [
        {
          draft_id: "draft-1",
          mail_account_id: "mailbox-1",
          thread_id: "thread-1",
          subject: "Re: Hello",
          status: "draft",
          version: 4,
        },
      ],
    });
    renderWorkflow(
      <DraftSendingWorkflow threadId="thread-1" mailAccountId="mailbox-1" />
    );

    const select = await screen.findByLabelText("Saved messages");
    await screen.findByRole("option", { name: /Re: Hello/ });
    fireEvent.change(select, { target: { value: "draft-1" } });

    await screen.findByText("Thanks for writing.");
    expect(calls.some((call) => call.url === "/api/v1/message-drafts?thread_id=thread-1&limit=50")).toBe(true);
    expect(calls.some((call) => call.url === "/api/v1/message-drafts/draft-1")).toBe(true);
  });

  it("does not load or expose an outbound from a different mailbox", async () => {
    const calls = mockSendingApi({
      draft: draft({
        mail_account_id: "mailbox-other",
        outbound_message_id: "outbound-other",
        status: "frozen",
      }),
    });
    renderWorkflow(
      <DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />
    );

    expect(
      await screen.findByText(
        "This draft belongs to a different mailbox and cannot be sent here."
      )
    ).toBeInTheDocument();
    expect(calls.some((call) => call.url.includes("/outbound-messages/"))).toBe(
      false
    );
  });

  it("offers only a channel for the exact mailbox and retries freeze with the same bound request", async () => {
    const calls = mockSendingApi({ freezeFailureCount: 1 });
    renderWorkflow(
      <DraftSendingWorkflow
        draftId="draft-1"
        threadId="thread-1"
        mailAccountId="mailbox-1"
      />
    );

    const channelSelect = await screen.findByLabelText("Delivery channel");
    fireEvent.change(channelSelect, { target: { value: "channel-1" } });
    const meetingSelect = await screen.findByLabelText(
      "Booking link meeting type (optional)"
    );
    await screen.findByRole("option", { name: "Sales introduction" });
    fireEvent.change(meetingSelect, { target: { value: "meeting-1" } });
    expect(Array.from((channelSelect as HTMLSelectElement).options).map((o) => o.value)).not.toContain("channel-other");

    fireEvent.click(screen.getByRole("button", { name: "Freeze exact message" }));
    await waitFor(() => expect(calls.filter((call) => call.url.endsWith("/freeze"))).toHaveLength(1));
    fireEvent.click(screen.getByRole("button", { name: "Freeze exact message" }));
    await waitFor(() => expect(calls.filter((call) => call.url.endsWith("/freeze"))).toHaveLength(2));

    const freezes = calls.filter((call) => call.url.endsWith("/freeze"));
    for (const freeze of freezes) {
      const headers = new Headers(freeze.init.headers);
      expect(headers.get("If-Match")).toBe('"4"');
      expect(JSON.parse(String(freeze.init.body))).toEqual({
        delivery_channel_id: "channel-1",
        meeting_type_id: "meeting-1",
      });
    }
    expect(
      new Headers(freezes[0]!.init.headers).get("Idempotency-Key")
    ).toMatch(/^[0-9a-f-]{36}$/);
    expect(new Headers(freezes[1]!.init.headers).get("Idempotency-Key")).toBe(
      new Headers(freezes[0]!.init.headers).get("Idempotency-Key")
    );
  });

  it("offers only delivery channels matching a manual draft's actual purpose", async () => {
    const calls = mockSendingApi({
      draft: draft({ purpose: "marketing" }),
    });
    renderWorkflow(
      <DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />
    );

    const channelSelect = await screen.findByLabelText("Delivery channel");
    await screen.findByRole("option", { name: "Inbox marketing" });
    const optionValues = Array.from(
      (channelSelect as HTMLSelectElement).options
    ).map((option) => option.value);
    expect(optionValues).toContain("channel-marketing");
    expect(optionValues).not.toContain("channel-1");

    fireEvent.change(channelSelect, { target: { value: "channel-marketing" } });
    fireEvent.click(screen.getByRole("button", { name: "Freeze exact message" }));
    await waitFor(() =>
      expect(calls.some((call) => call.url.endsWith("/freeze"))).toBe(true)
    );
    const freeze = calls.find((call) => call.url.endsWith("/freeze"))!;
    expect(JSON.parse(String(freeze.init.body))).toEqual({
      delivery_channel_id: "channel-marketing",
    });
  });

  it("keeps submit blocked when the non-binding preflight has a failed policy check", async () => {
    const calls = mockSendingApi({
      draft: draft({ status: "frozen", outbound_message_id: "outbound-1" }),
      outbound: outbound("draft"),
      preflight: {
        ...passingPreflight,
        checks: [
          { name: "recipient_basis", passed: false, code: "BASIS_UNVERIFIED" },
        ],
      },
    });
    renderWorkflow(
      <DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />
    );

    fireEvent.click(await screen.findByRole("button", { name: "Check send policy" }));

    expect(await screen.findByText(/BASIS_UNVERIFIED/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Submit message" })).toBeDisabled();
    expect(calls.some((call) => call.url.endsWith("/submit"))).toBe(false);
  });

  it("keeps the Inbox collision/review gate in force after a draft was saved", async () => {
    const calls = mockSendingApi({
      draft: draft({ status: "frozen", outbound_message_id: "outbound-1" }),
      outbound: outbound("draft"),
    });
    renderWorkflow(
      <DraftSendingWorkflow
        draftId="draft-1"
        mailAccountId="mailbox-1"
        sendBlocked
      />
    );

    fireEvent.click(await screen.findByRole("button", { name: "Check send policy" }));

    expect(await screen.findByText(/all current policy checks passed/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Submit message" })).toBeDisabled();
    expect(calls.some((call) => call.url.endsWith("/submit"))).toBe(false);
  });

  it("requires the mailbox's mail.send action even when policy checks pass", async () => {
    const calls = mockSendingApi({
      draft: draft({ status: "frozen", outbound_message_id: "outbound-1" }),
      outbound: outbound("draft"),
      mailAccountActions: ["mail.read", "mail.compose"],
    });
    renderWorkflow(
      <DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />
    );

    fireEvent.click(await screen.findByRole("button", { name: "Check send policy" }));

    expect(await screen.findByText(/has not granted you permission to send messages/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Submit message" })).toBeDisabled();
    expect(calls.some((call) => call.url.endsWith("/submit"))).toBe(false);
  });

  it("requires typed confirmation, retries the same submit key, and reports queueing as not delivered", async () => {
    const calls = mockSendingApi({
      draft: draft({ status: "frozen", outbound_message_id: "outbound-1" }),
      outbound: outbound("draft"),
      submitFailureCount: 1,
    });
    renderWorkflow(
      <DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />
    );
    fireEvent.click(await screen.findByRole("button", { name: "Check send policy" }));
    fireEvent.click(await screen.findByRole("button", { name: "Submit message" }));

    const confirm = await screen.findByRole("button", { name: "Queue this message" });
    expect(confirm).toBeDisabled();
    expect(screen.getAllByText("team@example.test").length).toBeGreaterThan(0);
    expect(screen.getAllByText("person@example.test").length).toBeGreaterThan(0);
    expect(
      within(screen.getByRole("dialog")).getByText("person@example.test")
    ).toBeInTheDocument();
    expect(calls.some((call) => call.url.endsWith("/submit"))).toBe(false);

    fireEvent.change(screen.getByLabelText(/type.*SEND/i), {
      target: { value: "SEND" },
    });
    expect(screen.getByRole("button", { name: "Queue this message" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Queue this message" }));
    await waitFor(() => expect(calls.filter((call) => call.url.endsWith("/submit"))).toHaveLength(1));

    fireEvent.click(await screen.findByRole("button", { name: "Submit message" }));
    fireEvent.change(screen.getByLabelText(/type.*SEND/i), {
      target: { value: "SEND" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Queue this message" }));
    await screen.findByText(/queued/i);

    const submits = calls.filter((call) => call.url.endsWith("/submit"));
    expect(submits).toHaveLength(2);
    expect(
      new Headers(submits[0]!.init.headers).get("Idempotency-Key")
    ).toBe(new Headers(submits[1]!.init.headers).get("Idempotency-Key"));
    expect(JSON.parse(String(submits[1]!.init.body))).toEqual({
      expected_payload_hash: "cd".repeat(32),
    });
    expect(
      await screen.findByText(/provider acceptance and delivery are still unconfirmed/i)
    ).toBeInTheDocument();
  });

  it("shows unknown outcomes as lookup-only and never exposes submit", async () => {
    const calls = mockSendingApi({
      draft: draft({ status: "frozen", outbound_message_id: "outbound-1" }),
      outbound: outbound("unknown"),
    });
    renderWorkflow(
      <DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />
    );

    expect(await screen.findByText(/do not resend/i)).toBeInTheDocument();
    const refresh = screen.getByRole("button", { name: "Look up message status" });
    expect(
      screen.getByRole("link", { name: "Open operations jobs" }).getAttribute("href")
    ).toBe("/app/operations/jobs");
    fireEvent.click(refresh);
    await waitFor(() =>
      expect(calls.filter((call) => call.url === "/api/v1/outbound-messages/outbound-1")).toHaveLength(2)
    );
    expect(screen.queryByRole("button", { name: "Submit message" })).not.toBeInTheDocument();
    expect(calls.some((call) => call.url.endsWith("/submit"))).toBe(false);
  });

  it("requests approval for the frozen hash and refreshes after a separate approval", async () => {
    const approvalRequired: OutboundPreflightResponse = {
      ...passingPreflight,
      checks: [
        { name: "approval_state", passed: false, code: "APPROVAL_REQUIRED" },
        { name: "approval_hash", passed: false, code: "APPROVAL_REQUIRED" },
        { name: "approver_permission", passed: false, code: "APPROVAL_REQUIRED" },
      ],
    };
    const calls = mockSendingApi({
      draft: draft({ status: "frozen", outbound_message_id: "outbound-1" }),
      outbound: outbound("draft"),
      preflight: approvalRequired,
      approvedPreflight: passingPreflight,
    });
    renderWorkflow(
      <DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />
    );
    fireEvent.click(await screen.findByRole("button", { name: "Check send policy" }));

    expect(await screen.findByText(/needs a separate approval/i)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText(/why is this message being requested/i), {
      target: { value: "Second reviewer required by policy" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Request approval" }));

    const approvalLink = await screen.findByRole("link", {
      name: "Open approval request",
    });
    expect(approvalLink.getAttribute("href")).toBe("/app/approvals/approval-1");
    const approvalCall = calls.find((call) =>
      call.url.endsWith("/approval-requests")
    )!;
    expect(JSON.parse(String(approvalCall.init.body))).toEqual({
      reason: "Second reviewer required by policy",
      expected_payload_hash: "cd".repeat(32),
    });
    expect(new Headers(approvalCall.init.headers).get("Idempotency-Key")).toMatch(
      /^[0-9a-f-]{36}$/
    );
    expect(screen.queryByRole("button", { name: /approve/i })).not.toBeInTheDocument();

    fireEvent.click(
      await screen.findByRole("button", {
        name: "Refresh approval and message status",
      })
    );
    await waitFor(() =>
      expect(
        calls.filter(
          (call) => call.url === "/api/v1/outbound-messages/outbound-1/preflight"
        )
      ).toHaveLength(2)
    );
    expect(screen.getByRole("button", { name: "Submit message" })).toBeEnabled();
  });

  it("cancels a queued message only after exact confirmation and with If-Match", async () => {
    const calls = mockSendingApi({
      draft: draft({ status: "frozen", outbound_message_id: "outbound-1" }),
      outbound: outbound("queued"),
    });
    renderWorkflow(
      <DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />
    );

    fireEvent.click(await screen.findByRole("button", { name: "Cancel before dispatch" }));
    const confirm = await screen.findByRole("button", { name: "Cancel message" });
    expect(confirm).toBeDisabled();
    fireEvent.change(screen.getByLabelText(/type.*CANCEL/i), {
      target: { value: "CANCEL" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Cancel message" }));

    const cancelCall = await waitFor(() => {
      const result = calls.find((call) => call.url.endsWith("/cancel"));
      expect(result).toBeDefined();
      return result!;
    });
    const headers = new Headers(cancelCall.init.headers);
    expect(headers.get("If-Match")).toBe('"6"');
    expect(headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
  });
});
