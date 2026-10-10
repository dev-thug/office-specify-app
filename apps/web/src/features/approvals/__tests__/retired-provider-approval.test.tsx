import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { toast } from "sonner";
import { afterEach, describe, expect, it, vi } from "vitest";

import approvals from "../../../../messages/en/approvals.json";
import campaigns from "../../../../messages/en/campaigns.json";
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

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("approval details", () => {
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
      <NextIntlClientProvider locale="en" messages={{ approvals, campaigns, common }}>
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

  it("records approval without showing a queued execution or sending", async () => {
    let currentApproval: Record<string, unknown> = {
      id: "approval-1",
      action_type: "message_submit",
      status: "pending",
      requested_by: "requester-1",
      requester_display_name: "Requester",
      version: 4,
      snapshot_hash: "office-snapshot-hash",
      expires_at: "2026-10-16T00:00:00Z",
      self_request: false,
      can_decide: true,
      recipients_count: 1,
      target: {
        preview: {
          recipients_count: 1,
          sender_address: "team@example.test",
          subject: "Hello",
          body_text: "Frozen message body",
          purpose: "correspondence",
        },
      },
    };
    const calls: { path: string; method: string }[] = [];
    const messageToast = vi.spyOn(toast, "message");
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), "http://office.test");
      const method = init?.method ?? "GET";
      calls.push({ path: url.pathname, method });
      if (url.pathname === "/api/v1/approvals/approval-1") {
        return new Response(JSON.stringify({ data: currentApproval }), {
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
      if (url.pathname === "/api/v1/approvals/approval-1/decisions" && method === "POST") {
        currentApproval = {
          ...currentApproval,
          status: "approved",
          version: 5,
          decision: {
            decision: "approved",
            decided_at: "2026-10-10T10:00:00Z",
            decided_by: "approver-1",
          },
        };
        return new Response(JSON.stringify({
          data: {
            approval_request_id: "approval-1",
            approval_status: "approved",
            execution_status: "ready",
            job_id: null,
            version: 5,
          },
        }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      return new Response("{}", { status: 404, headers: { "content-type": "application/json" } });
    }));

    render(
      <NextIntlClientProvider locale="en" messages={{ approvals, campaigns, common }}>
        <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
          <ApprovalDetail approvalId="approval-1" />
        </QueryClientProvider>
      </NextIntlClientProvider>
    );

    fireEvent.click(await screen.findByRole("button", { name: "Approve" }));
    fireEvent.change(await screen.findByPlaceholderText("approve"), {
      target: { value: "approve" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Confirm approval" }));

    expect(await screen.findByText(/authorized human must still send the message/i)).toBeVisible();
    expect(messageToast).not.toHaveBeenCalled();
    expect(calls.filter((call) => call.method === "POST")).toEqual([
      { path: "/api/v1/approvals/approval-1/decisions", method: "POST" },
    ]);
    expect(calls.some((call) => call.path.includes("/outbound-messages/"))).toBe(false);
  });

  it("keeps campaign approval separate and links the authorized execution action", async () => {
    const approval = {
      id: "approval-1",
      action_type: "campaign_launch",
      status: "approved",
      requested_by: "requester-1",
      requester_display_name: "Requester",
      version: 5,
      snapshot_hash: "manifest-hash",
      launch_id: "launch-1",
      target_id: "launch-1",
      expires_at: "2026-10-16T00:00:00Z",
      self_request: false,
      can_decide: false,
      recipients_count: 2,
      target: {
        preview: {
          recipients_count: 2,
          sender_address: "team@example.test",
          subject: "Approved launch",
          body_text: "Frozen campaign body",
          purpose: "marketing",
        },
      },
    };
    const calls: { path: string; method: string }[] = [];
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), "http://office.test");
      const method = init?.method ?? "GET";
      calls.push({ path: url.pathname, method });
      if (url.pathname === "/api/v1/approvals/approval-1") {
        return new Response(JSON.stringify({ data: approval }), {
          status: 200,
          headers: { "content-type": "application/json", etag: '"5"' },
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
      if (url.pathname === "/api/v1/campaign-launches/launch-1") {
        return new Response(JSON.stringify({
          data: {
            id: "launch-1",
            campaign_id: "campaign-1",
            revision_id: "revision-1",
            status: "approved",
            execution_status: "ready",
            activation_job_id: null,
            manifest_hash: "ab".repeat(32),
            version: 7,
            allowed_actions: ["execute_launch"],
            messages: {
              items: [{
                outbound_message_id: "outbound-1",
                enrollment_id: "enrollment-1",
                step_id: "step-1",
                step_order: 1,
                to_address_snapshot: "person@example.test",
                subject: "Approved launch",
                body_text: "Frozen campaign body",
                body_html: null,
                status: "draft",
                not_before: null,
                payload_hash: "cd".repeat(32),
              }],
              next_cursor: null,
              has_more: false,
            },
            enrollments: {
              total: 2,
              pending: 2,
              active: 0,
              completed: 0,
              stopped: 0,
              suppressed: 0,
            },
          },
        }), { status: 200, headers: { "content-type": "application/json" } });
      }
      return new Response("{}", { status: 404, headers: { "content-type": "application/json" } });
    }));

    render(
      <NextIntlClientProvider locale="en" messages={{ approvals, campaigns, common }}>
        <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
          <ApprovalDetail approvalId="approval-1" />
        </QueryClientProvider>
      </NextIntlClientProvider>
    );

    expect(await screen.findByText(/authorized human must still start the launch separately/i)).toBeVisible();
    expect(await screen.findByRole("button", { name: "Execute approved launch" })).toBeInTheDocument();
    expect(calls.some((call) => call.path.endsWith("/execute"))).toBe(false);
  });
});