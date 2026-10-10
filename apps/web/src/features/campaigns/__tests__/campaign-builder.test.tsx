import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { afterEach, describe, expect, it, vi } from "vitest";

import campaigns from "../../../../messages/en/campaigns.json";
import common from "../../../../messages/en/common.json";
import { CampaignBuilder } from "../campaign-builder";
import { CampaignDetail } from "../campaign-detail";

const legacyCampaign = {
  id: "campaign-1",
  name: "Legacy campaign",
  revisions: [
    {
      id: "revision-1",
      revision_no: 1,
      status: "draft",
      mail_account_id: "mail-1",
      delivery_channel_id: "channel-1",
      timezone: "UTC",
      schedule_rules: { weekdays: [1], window_start: "09:00", window_end: "17:00" },
      stop_on_reply: true,
      stop_on_booking: true,
      steps: [
        { id: "step-1", step_order: 1, template_version_id: "version-1", delay_hours: 0 },
        { id: "step-2", step_order: 2, template_version_id: "version-2", delay_hours: 24 },
      ],
    },
  ],
  launches: [],
};

afterEach(() => vi.unstubAllGlobals());

describe("CampaignBuilder legacy revisions", () => {
  it("preserves old sequence data and blocks it from revision or clone authoring", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const path = String(input);
        const data = path === "/api/v1/campaigns/campaign-1"
          ? legacyCampaign
          : path === "/api/v1/me"
            ? { role: "admin", mail_accounts: [] }
            : [];
        return new Response(JSON.stringify({ data, page: { has_more: false } }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );

    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <NextIntlClientProvider locale="en" messages={{ campaigns, common }}>
        <QueryClientProvider client={client}>
          <CampaignBuilder mode="revision" sourceCampaignId="campaign-1" />
        </QueryClientProvider>
      </NextIntlClientProvider>
    );

    expect(await screen.findByText("This legacy campaign is read-only")).toBeInTheDocument();
    expect(screen.getByText(/2 stored message step\(s\) and delays are preserved/)).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Create a new single-message campaign" })
    ).toHaveAttribute("href", "/app/campaigns/new");
    expect(screen.queryByRole("button", { name: "Create draft" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Add step" })).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Delay (hours)")).not.toBeInTheDocument();
  });
});

describe("CampaignBuilder retired delivery channels", () => {
  it("marks a missing saved channel read-only and blocks continuation", async () => {
    const source = {
      id: "retired-channel-campaign",
      name: "Legacy provider campaign",
      status: "draft",
      mail_account_id: "mail-1",
      revision: {
        id: "revision-1",
        revision_no: 1,
        status: "draft",
        mail_account_id: "mail-1",
        delivery_channel_id: "retired-channel",
        timezone: "Asia/Seoul",
        schedule_rules: { weekdays: [1], window_start: "09:00", window_end: "17:00" },
        stop_on_reply: true,
        stop_on_booking: true,
        steps: [{ id: "step-1", step_order: 1, template_version_id: "version-1", delay_hours: 0 }],
      },
      revisions: [],
      launches: [],
    };
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), "http://office.test").pathname;
      const data = path === "/api/v1/campaigns/retired-channel-campaign"
        ? source
        : path === "/api/v1/me"
          ? { role: "admin", mail_accounts: [{ id: "mail-1", address: "team@example.test" }] }
          : path === "/api/v1/mail-accounts"
            ? [{ id: "mail-1", status: "active", connection_id: "mail-connection-1" }]
            : path === "/api/v1/delivery-channels"
              ? [{
                  id: "smtp-current",
                  mail_account_id: "mail-1",
                  sender_identity_id: "sender-1",
                  provider: "delivery_provider",
                  purpose: "marketing",
                  capabilities: { transport: "smtp" },
                  name: "Office SMTP",
                  policy_status: "allowed",
                  enabled: true,
                  version: 1,
                }]
              : [];
      return new Response(JSON.stringify({ data, page: { next_cursor: null, has_more: false } }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }));

    render(
      <NextIntlClientProvider locale="en" messages={{ campaigns, common }}>
        <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
          <CampaignBuilder mode="revision" sourceCampaignId="retired-channel-campaign" />
        </QueryClientProvider>
      </NextIntlClientProvider>
    );

    expect(await screen.findByText(/saved delivery channel is retired or unavailable/i)).toBeVisible();
    const next = screen.getByRole("button", { name: "Next" });
    expect(next).toBeDisabled();
  });
});

describe("CampaignBuilder Office SMTP revisions", () => {
  it("keeps scheduling and stop controls and saves an SMTP revision", async () => {
    const source = {
      id: "campaign-smtp",
      name: "SMTP marketing source",
      status: "draft",
      mail_account_id: "mail-1",
      revision: {
        id: "revision-1",
        revision_no: 1,
        status: "draft",
        mail_account_id: "mail-1",
        delivery_channel_id: "channel-smtp",
        timezone: "Asia/Seoul",
        schedule_rules: {
          weekdays: [1, 2, 3, 4, 5],
          window_start: "09:00",
          window_end: "17:00",
          internal_daily_cap: 10,
          minimum_gap_seconds: 30,
        },
        stop_on_reply: true,
        stop_on_booking: true,
        steps: [{ id: "step-1", step_order: 1, template_version_id: "version-1", delay_hours: 0 }],
      },
      launches: [],
    };
    const version = {
      template_version_id: "version-1",
      template_id: "template-1",
      version_no: 1,
      subject: "Hello {{contact.name}}",
      body_text: "Hello {{contact.name}}",
      body_html: "<p>Hello {{contact.name}}</p>",
      variable_schema: { type: "object", properties: { "contact.name": { type: "string" } }, required: ["contact.name"], additionalProperties: false },
      content_hash: "content-hash",
      created_at: "2026-10-10T00:00:00Z",
    };
    let savedRevision: Record<string, unknown> | undefined;
    const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(input), "http://office.test").pathname;
      const method = init?.method ?? "GET";
      let data: unknown = [];
      let status = 200;
      if (path === "/api/v1/campaigns/campaign-smtp") data = source;
      else if (path === "/api/v1/me") data = { role: "admin", mail_accounts: [{ id: "mail-1", address: "team@example.test" }] };
      else if (path === "/api/v1/mail-accounts") data = [{ id: "mail-1", status: "active", connection_id: "mail-connection-1" }];
      else if (path === "/api/v1/delivery-channels") data = [
        {
          id: "channel-smtp",
          mail_account_id: "mail-1",
          sender_identity_id: "sender-1",
          connection_id: "smtp-connection-1",
          provider: "delivery_provider",
          purpose: "marketing",
          capabilities: { transport: "smtp" },
          name: "Office SMTP",
          policy_status: "allowed",
          enabled: true,
          version: 1,
        },
        {
          id: "channel-zoho",
          mail_account_id: "mail-1",
          sender_identity_id: "sender-1",
          connection_id: "campaigns-connection-1",
          provider: "zoho_campaigns",
          purpose: "marketing",
          name: "Zoho Campaigns",
          policy_status: "allowed",
          enabled: true,
          version: 1,
        },
      ];
      else if (path === "/api/v1/meeting-types") data = [];
      else if (path === "/api/v1/contacts") data = [{
        id: "contact-1",
        name: "Fixture contact",
        emails: [{ email_address_id: "email-1", address: "fixture@example.test", is_primary: true, verification_status: "valid", marketing_eligibility: { eligible: true, reason_codes: [] } }],
      }];
      else if (path === "/api/v1/template-versions/version-1") data = version;
      else if (path === "/api/v1/campaigns/campaign-smtp/revisions" && method === "POST") {
        savedRevision = JSON.parse(String(init?.body)) as Record<string, unknown>;
        data = { revision_id: "revision-2", revision_no: 2, status: "draft" };
        status = 201;
      }
      return new Response(JSON.stringify({ data, page: { next_cursor: null, has_more: false } }), {
        status,
        headers: { "content-type": "application/json" },
      });
    });
    vi.stubGlobal("fetch", fetcher);

    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <NextIntlClientProvider locale="en" messages={{ campaigns, common }}>
        <QueryClientProvider client={client}>
          <CampaignBuilder mode="revision" sourceCampaignId="campaign-smtp" />
        </QueryClientProvider>
      </NextIntlClientProvider>
    );

    expect(await screen.findByText("Office SMTP", { exact: true })).toBeVisible();
    expect(screen.queryByText("Zoho Campaigns", { exact: true })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await screen.findByRole("region", { name: "Audience" });
    fireEvent.click(await screen.findByRole("button", { name: /Fixture contact/ }));
    fireEvent.click(screen.getByRole("checkbox", { name: "fixture@example.test" }));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await screen.findByText("One immediate message", { exact: true });
    expect(screen.getByRole("group", { name: "Stop conditions" })).toBeVisible();
    expect(screen.queryByText(/Zoho Campaigns/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Next" }));

    expect(await screen.findByLabelText("Timezone (IANA)")).toHaveValue("Asia/Seoul");
    expect(screen.getByRole("group", { name: "Send days" })).toBeVisible();
    expect(screen.getByLabelText("Window start")).toHaveValue("09:00");
    expect(screen.getByLabelText("Window end")).toHaveValue("17:00");
    expect(screen.getByLabelText("Internal daily cap")).toHaveValue("10");
    expect(screen.getByLabelText("Min gap (seconds)")).toHaveValue("30");
    expect(screen.getByLabelText("Not before")).toBeInTheDocument();
    expect(screen.getByLabelText("Expires at")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByText(/09:00/)).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Create revision" }));

    await waitFor(() => expect(savedRevision).toBeDefined());
    const saved = savedRevision as Record<string, unknown>;
    expect(saved.schedule_rules).toEqual({
      weekdays: [1, 2, 3, 4, 5],
      window_start: "09:00",
      window_end: "17:00",
      internal_daily_cap: 10,
      minimum_gap_seconds: 30,
    });
    expect(saved.stop_on_reply).toBe(true);
    expect(saved.stop_on_booking).toBe(true);
  });
});

describe("CampaignDetail retired delivery channels", () => {
  it("shows the stored launch as history and hides every launch action", async () => {
    const campaign = {
      id: "retired-campaign",
      name: "Legacy campaign",
      status: "active",
      mail_account_id: "mail-1",
      revision: {
        id: "revision-1",
        revision_no: 1,
        status: "frozen",
        mail_account_id: "mail-1",
        delivery_channel_id: "retired-channel",
        timezone: "Asia/Seoul",
        schedule_rules: {},
        stop_on_reply: true,
        stop_on_booking: true,
        steps: [],
      },
      revisions: [],
      launches: [{
        id: "launch-1",
        campaign_id: "retired-campaign",
        revision_id: "revision-1",
        delivery_channel_id: "retired-channel",
        status: "frozen",
        manifest_hash: "manifest-hash",
        frozen_at: "2026-10-10T00:00:00Z",
        expires_at: "2026-10-11T00:00:00Z",
        version: 1,
        recipients_count: 2,
      }],
    };
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), "http://office.test").pathname;
      const data = path === "/api/v1/campaigns/retired-campaign"
        ? campaign
        : path === "/api/v1/delivery-channels"
          ? [{
              id: "smtp-current",
              mail_account_id: "mail-1",
              provider: "delivery_provider",
              purpose: "marketing",
              name: "Office SMTP",
              policy_status: "allowed",
              enabled: true,
              capabilities: { transport: "smtp" },
              version: 1,
            }]
          : [];
      return new Response(JSON.stringify({ data, page: { next_cursor: null, has_more: false } }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }));

    render(
      <NextIntlClientProvider locale="en" messages={{ campaigns, common }}>
        <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
          <CampaignDetail campaignId="retired-campaign" />
        </QueryClientProvider>
      </NextIntlClientProvider>
    );

    expect(await screen.findByRole("heading", { name: "Legacy campaign" })).toBeVisible();
    expect(screen.getByText(/This launch uses a retired delivery channel/)).toBeVisible();
    expect(screen.queryByRole("button", { name: "Request approval" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Pause" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Cancel run" })).not.toBeInTheDocument();
  });
});
