import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { afterEach, describe, expect, it, vi } from "vitest";

import approvals from "../../../../messages/en/approvals.json";
import common from "../../../../messages/en/common.json";
import { ApprovalDetail } from "../approval-detail";

const retiredProviderApproval = {
  id: "approval-1",
  action_type: "campaign_launch",
  status: "approved",
  requested_by: "requester-1",
  requester_display_name: "Requester",
  version: 4,
  snapshot_hash: "office-snapshot-hash",
  expires_at: "2026-10-11T00:00:00Z",
  self_request: false,
  can_decide: false,
  retired_provider: true,
  recipients_count: 12,
  target: {
    preview: {
      recipients_count: 12,
      sender_address: "office@example.test",
      subject: "Office source subject",
      body_text: "Office source text",
      body_html: "<p>Office source HTML</p>",
      purpose: "marketing",
    },
  },
};

afterEach(() => vi.unstubAllGlobals());

describe("retired provider approvals", () => {
  it("keeps the frozen detail visible while hiding all actions", async () => {
    const calls: { path: string; method: string }[] = [];
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), "http://office.test");
      const method = init?.method ?? "GET";
      calls.push({ path: url.pathname, method });
      if (url.pathname === "/api/v1/approvals/approval-1") {
        return new Response(JSON.stringify({ data: retiredProviderApproval }), {
          status: 200,
          headers: { "content-type": "application/json", etag: '"4"' },
        });
      }
      if (url.pathname === "/api/v1/me") {
        return new Response(JSON.stringify({ data: { principal_id: "approver-1", role: "manager" } }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      if (url.pathname === "/api/v1/principals") {
        return new Response(JSON.stringify({ data: [], page: { has_more: false } }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      return new Response("{}", { status: 404, headers: { "content-type": "application/json" } });
    }));

    render(
      <NextIntlClientProvider locale="en" messages={{ approvals, common }}>
        <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
          <ApprovalDetail approvalId="approval-1" />
        </QueryClientProvider>
      </NextIntlClientProvider>
    );

    expect(await screen.findByText("Legacy provider approval is read-only")).toBeVisible();
    expect(screen.getByText(/This request belongs to a retired delivery integration/)).toBeVisible();
    expect(screen.getByText("Office source text")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Approve" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Reject / request changes" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Revoke" })).not.toBeInTheDocument();
    expect(calls.some((call) => call.method === "POST")).toBe(false);
  });
});