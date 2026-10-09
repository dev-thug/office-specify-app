import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
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
