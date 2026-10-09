import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { afterEach, describe, expect, it, vi } from "vitest";

import campaigns from "../../../../messages/en/campaigns.json";
import common from "../../../../messages/en/common.json";
import { CampaignBuilder } from "../campaign-builder";

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

describe("CampaignBuilder Zoho Campaigns revisions", () => {
  it("hides scheduling and stop controls and saves an immediate-only revision", async () => {
    const source = {
      id: "campaign-zoho",
      name: "Free Campaigns source",
      status: "draft",
      mail_account_id: "mail-1",
      revision: {
        id: "revision-1",
        revision_no: 1,
        status: "draft",
        mail_account_id: "mail-1",
        delivery_channel_id: "channel-zoho",
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
      if (path === "/api/v1/campaigns/campaign-zoho") data = source;
      else if (path === "/api/v1/me") data = { role: "admin", mail_accounts: [{ id: "mail-1", address: "team@example.test" }] };
      else if (path === "/api/v1/mail-accounts") data = [{ id: "mail-1", status: "active", connection_id: "mail-connection-1" }];
      else if (path === "/api/v1/delivery-channels") data = [{
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
      }];
      else if (path === "/api/v1/meeting-types") data = [];
      else if (path === "/api/v1/contacts") data = [{
        id: "contact-1",
        name: "Fixture contact",
        emails: [{ email_address_id: "email-1", address: "fixture@example.test", is_primary: true, verification_status: "valid", marketing_eligibility: { eligible: true, reason_codes: [] } }],
      }];
      else if (path === "/api/v1/template-versions/version-1") data = version;
      else if (path === "/api/v1/campaigns/campaign-zoho/revisions" && method === "POST") {
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
          <CampaignBuilder mode="revision" sourceCampaignId="campaign-zoho" />
        </QueryClientProvider>
      </NextIntlClientProvider>
    );

    await screen.findByText("Zoho Campaigns", { exact: true });
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await screen.findByRole("region", { name: "Audience" });
    fireEvent.click(await screen.findByRole("button", { name: /Fixture contact/ }));
    fireEvent.click(screen.getByRole("checkbox", { name: "fixture@example.test" }));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await screen.findByText("One immediate message", { exact: true });
    expect(screen.queryByRole("group", { name: "Stop conditions" })).not.toBeInTheDocument();
    expect(screen.getByText(/Zoho Campaigns sends one bulk campaign/)).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Next" }));

    await screen.findByText(/Free Zoho Campaigns uses one immediate bulk send after approval/);
    expect(screen.queryByLabelText("Timezone (IANA)")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Send days")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Window start")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Window end")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Internal daily cap")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Minimum gap (seconds)")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Executable start")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Expires at")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await screen.findByText(/Free Zoho Campaigns uses one immediate bulk send after approval/);
    expect(screen.queryByText("09:00")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Create revision" }));

    await waitFor(() => expect(savedRevision).toBeDefined());
    const saved = savedRevision as Record<string, unknown>;
    expect(saved.schedule_rules).toEqual({});
    expect(saved.stop_on_reply).toBe(false);
    expect(saved.stop_on_booking).toBe(false);
  });
});
