import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { afterEach, describe, expect, it, vi } from "vitest";

import approvals from "../../../../messages/en/approvals.json";
import common from "../../../../messages/en/common.json";
import { ApprovalDetail } from "../approval-detail";

const nativeApproval = {
  id: "approval-1",
  action_type: "campaign_launch",
  status: "pending",
  requested_by: "requester-1",
  requester_display_name: "Requester",
  version: 4,
  snapshot_hash: "office-snapshot-hash",
  expires_at: "2026-10-11T00:00:00Z",
  self_request: false,
  can_decide: true,
  native_export_required: true,
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
  native_export: {
    id: "native-export-1",
    export_hash: "native-export-hash",
    state: "syncing",
    subject: "Mapped Campaigns subject",
    body_html: "<p>Mapped native HTML</p>",
    field_map: {
      "contact.name": { contact_field: "Last Name", merge_tag: "$[UD:FNAME]$" },
    },
    recipient_count: 9,
    sender_address: "campaigns@example.test",
    provider_list_key: "zoho-list-1",
    topic_id: "topic-1",
  },
};

function mount(request: typeof nativeApproval) {
  const calls: { path: string; method: string; body: unknown; headers: Headers }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://office.test");
    const path = url.pathname;
    const method = init?.method ?? "GET";
    const headers = new Headers(init?.headers);
    const body = method === "POST" ? JSON.parse(String(init?.body ?? "{}")) : null;
    calls.push({ path, method, body, headers });
    if (path === "/api/v1/approvals/approval-1") {
      return new Response(JSON.stringify({ data: request }), {
        status: 200,
        headers: { "content-type": "application/json", etag: '"4"' },
      });
    }
    if (path === "/api/v1/me") {
      return new Response(JSON.stringify({ data: { principal_id: "approver-1", role: "manager" } }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }
    if (path === "/api/v1/approvals/approval-1/decisions") {
      return new Response(JSON.stringify({ data: { approval_status: "approved" } }), {
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
  return calls;
}

afterEach(() => vi.unstubAllGlobals());

describe("native export approval review", () => {
  it("shows the isolated native export and keeps approval disabled until sync is ready", async () => {
    const calls = mount(nativeApproval);

    expect(await screen.findByText("Native Zoho Campaigns export")).toBeVisible();
    const preview = screen.getByTitle("Native Campaigns HTML preview");
    expect(preview).toHaveAttribute("sandbox", "");
    expect(preview).toHaveAttribute("srcdoc", "<p>Mapped native HTML</p>");
    expect(screen.getByText("Last Name", { exact: true })).toBeVisible();
    expect(screen.getByText("$[UD:FNAME]$", { exact: true })).toBeVisible();
    expect(screen.getByText("zoho-list-1", { exact: true })).toBeVisible();
    expect(screen.getByText("topic-1", { exact: true })).toBeVisible();
    expect(screen.getByRole("button", { name: "Approve" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Reject / request changes" })).toBeEnabled();
    expect(calls.some((call) => call.method === "POST" && call.path.endsWith("/decisions"))).toBe(false);
  });

  it("sends both the Office snapshot hash and the reviewed native export hash when approving READY", async () => {
    const calls = mount({
      ...nativeApproval,
      native_export: { ...nativeApproval.native_export, state: "ready" },
    });

    const approve = await screen.findByRole("button", { name: "Approve" });
    expect(approve).toBeEnabled();
    fireEvent.click(approve);
    const confirmation = await screen.findByLabelText('Type "approve" to confirm');
    fireEvent.change(confirmation, { target: { value: "approve" } });
    fireEvent.click(screen.getByRole("button", { name: "Confirm approval" }));

    await waitFor(() => expect(calls.some((call) => call.method === "POST" && call.path.endsWith("/decisions"))).toBe(true));
    const decision = calls.find((call) => call.method === "POST" && call.path.endsWith("/decisions"))!;
    expect(decision.body).toEqual({
      decision: "approved",
      expected_snapshot_hash: "office-snapshot-hash",
      expected_export_hash: "native-export-hash",
    });
    expect(decision.headers.get("If-Match")).toBe('"4"');
    expect(decision.headers.get("Idempotency-Key")).toBeTruthy();
  });

  it("allows rejection before native sync without sending a native export hash", async () => {
    const calls = mount(nativeApproval);
    fireEvent.click(await screen.findByRole("button", { name: "Reject / request changes" }));
    fireEvent.change(screen.getByLabelText("Reason"), { target: { value: "Review the native field mapping" } });
    fireEvent.click(screen.getByRole("button", { name: "Confirm rejection" }));

    await waitFor(() => expect(calls.some((call) => call.method === "POST" && call.path.endsWith("/decisions"))).toBe(true));
    const decision = calls.find((call) => call.method === "POST" && call.path.endsWith("/decisions"))!;
    expect(decision.body).toEqual({
      decision: "rejected",
      expected_snapshot_hash: "office-snapshot-hash",
      reason: "Review the native field mapping",
    });
    expect(decision.body).not.toHaveProperty("expected_export_hash");
  });
});
