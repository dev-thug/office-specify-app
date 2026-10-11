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

describe("DraftSendingWorkflow sales campaign guard", () => {
  it("shows a campaign-board link and keeps direct send unavailable for linked drafts", async () => {
    mockSendingApi({
      draft: draft({
        status: "frozen",
        outbound_message_id: "outbound-1",
        sales_campaign_id: "campaign-1",
        sales_target_id: "target-1",
      }),
      outbound: outbound("draft"),
    });
    renderWorkflow(
      <DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />
    );

    expect(
      await screen.findByText("This draft is managed by a sales campaign.")
    ).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Open campaign" })).toHaveAttribute(
      "href",
      "/app/campaigns/campaign-1"
    );
    expect(screen.queryByRole("button", { name: "Send message" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Check send policy" })).toBeNull();
    expect(screen.queryByRole("combobox", { name: "Delivery channel" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Request review" })).toBeNull();
  });

  it("shows no freeze or approval step for campaign drafts (the board queues them)", async () => {
    const calls = mockSendingApi({ draft: draft({ purpose: "marketing" }) });
    renderWorkflow(
      <DraftSendingWorkflow
        draftId="draft-1"
        mailAccountId="mailbox-1"
        salesCampaignId="campaign-1"
        allowSubmit={false}
        lockedDeliveryChannelId="channel-marketing"
        campaignMeetingTypeId="meeting-1"
      />
    );

    await waitFor(() =>
      expect(calls.some((call) => call.url.includes("/message-drafts/draft-1"))).toBe(true)
    );
    expect(screen.queryByRole("button", { name: "Request review" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Request approval" })).toBeNull();
    expect(screen.queryByRole("combobox", { name: "Delivery channel" })).toBeNull();
    expect(calls.some((call) => call.url.endsWith("/freeze"))).toBe(false);
  });

  it("sends generic linked drafts back to the campaign board instead of exposing freeze controls", async () => {
    mockSendingApi({
      draft: draft({
        status: "draft",
        sales_campaign_id: "campaign-1",
        sales_target_id: "target-1",
      }),
    });
    renderWorkflow(<DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />);

    expect(
      await screen.findByText("This draft is managed by a sales campaign.")
    ).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Open campaign" })).toHaveAttribute(
      "href",
      "/app/campaigns/campaign-1"
    );
    expect(
      screen.queryByRole("combobox", { name: "Delivery channel" })
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Request review" })
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Request approval" })
    ).toBeNull();
  });
});

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
  thread_id: null,
  approval_status: null,
  approval_valid: false,
  approval_snapshot_matches: true,
  approval_expires_at: null,
  engagement: {
    tracking_enabled: false,
    open_request_count: 0,
    click_request_count: 0,
  },
  known_replies: [],
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
    await screen.findByRole("option", { name: "Sales introduction · 30 min" });
    fireEvent.change(meetingSelect, { target: { value: "meeting-1" } });
    expect(Array.from((channelSelect as HTMLSelectElement).options).map((o) => o.value)).not.toContain("channel-other");

    fireEvent.click(screen.getByRole("button", { name: "Request review" }));
    await waitFor(() => expect(calls.filter((call) => call.url.endsWith("/freeze"))).toHaveLength(1));
    fireEvent.click(screen.getByRole("button", { name: "Request review" }));
    await waitFor(() => expect(calls.filter((call) => call.url.endsWith("/freeze"))).toHaveLength(2));

    const freezes = calls.filter((call) => call.url.endsWith("/freeze"));
    for (const freeze of freezes) {
      const headers = new Headers(freeze.init.headers);
      expect(headers.get("If-Match")).toBe('"4"');
      expect(JSON.parse(String(freeze.init.body))).toEqual({
        delivery_channel_id: "channel-1",
        meeting_type_id: "meeting-1",
        tracking_requested: false,
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
    fireEvent.click(screen.getByRole("button", { name: "Request review" }));
    await waitFor(() =>
      expect(calls.some((call) => call.url.endsWith("/freeze"))).toBe(true)
    );
    const freeze = calls.find((call) => call.url.endsWith("/freeze"))!;
    expect(JSON.parse(String(freeze.init.body))).toEqual({
      delivery_channel_id: "channel-marketing",
      tracking_requested: false,
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
    expect(screen.getByRole("button", { name: "Send message" })).toBeDisabled();
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

    expect(await screen.findByText(/current policy checks passed/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send message" })).toBeDisabled();
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
    expect(screen.getByRole("button", { name: "Send message" })).toBeDisabled();
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
    fireEvent.click(await screen.findByRole("button", { name: "Send message" }));

    const confirm = await screen.findByRole("button", { name: "Confirm send" });
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
    expect(screen.getByRole("button", { name: "Confirm send" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Confirm send" }));
    await waitFor(() => expect(calls.filter((call) => call.url.endsWith("/submit"))).toHaveLength(1));

    fireEvent.click(await screen.findByRole("button", { name: "Send message" }));
    fireEvent.change(screen.getByLabelText(/type.*SEND/i), {
      target: { value: "SEND" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Confirm send" }));
    await screen.findByText("Message queued for dispatch. Provider acceptance and delivery are still unconfirmed.");
    expect(screen.getByText("Submission status: Queued")).toBeInTheDocument();
    expect(screen.getByText("Provider evidence status: Unknown")).toBeInTheDocument();

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
    expect(screen.queryByRole("button", { name: "Send message" })).not.toBeInTheDocument();
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
    expect(screen.getByRole("button", { name: "Send message" })).toBeEnabled();
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

  it("offers only a correspondence channel when freezing an inbox reply", async () => {
    const calls = mockSendingApi({
      draft: draft({ purpose: "correspondence" }),
    });
    renderWorkflow(
      <DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />
    );

    const channelSelect = await screen.findByLabelText("Delivery channel");
    const optionValues = Array.from(
      (channelSelect as HTMLSelectElement).options
    ).map((option) => option.value);
    expect(optionValues).toContain("channel-1");
    expect(optionValues).not.toContain("channel-marketing");

    fireEvent.change(channelSelect, { target: { value: "channel-1" } });
    fireEvent.click(screen.getByRole("button", { name: "Request review" }));
    await waitFor(() =>
      expect(calls.some((call) => call.url.endsWith("/freeze"))).toBe(true)
    );
    const freeze = calls.find((call) => call.url.endsWith("/freeze"))!;
    expect(JSON.parse(String(freeze.init.body))).toEqual({
      delivery_channel_id: "channel-1",
      tracking_requested: false,
    });
  });

  it("opts into engagement requests only when selected before freezing", async () => {
    const calls = mockSendingApi({});
    renderWorkflow(
      <DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />
    );

    const tracking = await screen.findByRole("checkbox", {
      name: "Request engagement tracking",
    });
    expect(tracking).not.toBeChecked();
    fireEvent.click(tracking);
    fireEvent.change(await screen.findByLabelText("Delivery channel"), {
      target: { value: "channel-1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Request review" }));

    await waitFor(() => {
      const freeze = calls.find((call) => call.url.endsWith("/freeze"));
      expect(freeze).toBeDefined();
      expect(JSON.parse(String(freeze!.init.body))).toEqual({
        delivery_channel_id: "channel-1",
        tracking_requested: true,
      });
    });
  });

  it("shows approval, submission, provider, engagement, and linked reply status independently", async () => {
    const outboundStatus = {
      ...outbound("draft"),
      approval_status: "approved",
      approval_valid: true,
      approval_snapshot_matches: true,
      approval_expires_at: "2026-10-16T00:00:00Z",
      thread_id: "thread-1",
      engagement: {
        tracking_enabled: true,
        open_request_count: 4,
        click_request_count: 2,
      },
      known_replies: [
        {
          inbound_message_id: "inbound-1",
          thread_id: "thread-1",
          from_address: "person@example.test",
          received_at: "2026-10-10T10:00:00Z",
        },
      ],
    };
    const calls = mockSendingApi({
      draft: draft({ status: "frozen", outbound_message_id: "outbound-1" }),
      outbound: outboundStatus,
    });
    renderWorkflow(
      <DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />
    );

    expect(await screen.findByText(/approval is valid for this frozen snapshot/i)).toBeInTheDocument();
    expect(screen.getByText("Submission status: Draft")).toBeInTheDocument();
    expect(screen.getByText("Provider evidence status: Unknown")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Open conversation" })).toHaveAttribute(
      "href",
      "/app/inbox/thread-1"
    );
    expect(screen.getByText("Open requests: 4")).toBeInTheDocument();
    expect(screen.getByText("Link click requests: 2")).toBeInTheDocument();
    expect(screen.getByText(/mail proxies or security scanners/i)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Reply from person@example.test" })).toHaveAttribute(
      "href",
      "/app/inbox/thread-1"
    );

    fireEvent.click(await screen.findByRole("button", { name: "Check send policy" }));
    expect(await screen.findByText(/current policy checks passed/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    expect(await screen.findByRole("button", { name: "Confirm send" })).toBeInTheDocument();
    expect(calls.some((call) => call.url.endsWith("/submit"))).toBe(false);
  });

  it("blocks human send when the approved snapshot is no longer valid", async () => {
    const outboundStatus = {
      ...outbound("draft"),
      approval_status: "approved",
      approval_valid: false,
      approval_snapshot_matches: false,
      approval_expires_at: "2026-10-16T00:00:00Z",
    };
    const calls = mockSendingApi({
      draft: draft({ status: "frozen", outbound_message_id: "outbound-1" }),
      outbound: outboundStatus,
      preflight: passingPreflight,
    });
    renderWorkflow(
      <DraftSendingWorkflow draftId="draft-1" mailAccountId="mailbox-1" />
    );

    fireEvent.click(await screen.findByRole("button", { name: "Check send policy" }));
    await screen.findByText(/does not match the current frozen snapshot/i);
    expect(screen.getByRole("button", { name: "Send message" })).toBeDisabled();
    expect(calls.some((call) => call.url.endsWith("/submit"))).toBe(false);
  });
});
