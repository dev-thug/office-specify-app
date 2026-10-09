import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { NextIntlClientProvider } from "next-intl";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import campaigns from "../../../../messages/en/campaigns.json";
import common from "../../../../messages/en/common.json";
import settings from "../../../../messages/en/settings.json";
import type { MarketingExport } from "@/lib/api/marketing";
import { MarketingExportPanel } from "../export-panel";

const auth = vi.hoisted(() => ({ isAdmin: true }));
vi.mock("@/features/settings/use-me", () => ({
  useIsAdmin: () => auth.isAdmin,
}));
const previousScrollIntoView = Element.prototype.scrollIntoView;
beforeAll(() => {
  Element.prototype.scrollIntoView = vi.fn();
});
afterAll(() => {
  if (previousScrollIntoView) Element.prototype.scrollIntoView = previousScrollIntoView;
  else Reflect.deleteProperty(Element.prototype, "scrollIntoView");
});

const connection = {
  id: "connection-1",
  name: "Office Campaigns",
  status: "active",
  write_scopes_ready: true,
  plan: "free",
  region: "com",
  last_verified_at: "2026-10-09T00:00:00Z",
  lists_count: 1,
  created_at: "2026-10-09T00:00:00Z",
};
const setup = {
  configured: true,
  missing_fields: [],
  callback_uri: "https://office.test/callback",
  accounts_host: "https://accounts.zoho.com",
  campaigns_host: "https://campaigns.zoho.com",
  plan: "free",
  scheduled_send: false,
  webhooks: false,
  mail_oauth_configured: false,
  email_api_configured: false,
  workspace_id: "workspace-1",
  connections: [connection],
};
const prepared = {
  id: "export-1",
  launch_id: "launch-1",
  connection_id: connection.id,
  state: "PREPARED",
  version: 1,
  source_manifest_hash: "source-hash",
  export_hash: "export-hash",
  content_hash: "content-hash",
  recipient_count: 12,
  active_count: 9,
  pending_count: 2,
  excluded_count: 1,
  sender_address: "news@example.test",
  sender_name: "Office News",
  subject: "A useful update",
  provider_list_key: "list-1",
  topic_id: "topic-1",
  provider_campaign_key: null,
  last_checked_at: null,
  expires_at: "2026-10-11T00:00:00Z",
  created_at: "2026-10-10T00:00:00Z",
  report: {},
};

function response(data: unknown, status = 200, contentType = "application/json") {
  return new Response(JSON.stringify({ data }), {
    status,
    headers: { "content-type": contentType },
  });
}

function mount(sourceStatus = "approved") {
  return render(
    <QueryClientProvider
      client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}
    >
      <NextIntlClientProvider
        locale="en"
        messages={{ campaigns, common, settings }}
      >
        <MarketingExportPanel
          launchId="launch-1"
          sourceStatus={sourceStatus}
          sourceManifestHash="source-hash"
          connectionId={connection.id}
        />
      </NextIntlClientProvider>
    </QueryClientProvider>
  );
}

afterEach(() => {
  auth.isAdmin = true;
  sessionStorage.clear();
  localStorage.clear();
  vi.unstubAllGlobals();
});

describe("Office-owned Zoho Campaigns export", () => {
  it("keeps agents out of export APIs", () => {
    auth.isAdmin = false;
    const fetcher = vi.fn();
    vi.stubGlobal("fetch", fetcher);

    const { container } = mount();

    expect(container).toBeEmptyDOMElement();
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("prepares frozen Office content, syncs one draft and sends only after approval and typed confirmation", async () => {
    const content = {
      export_id: "export-1",
      subject: "A useful update",
      html: "<p>Office content</p><a href=\"https://unsubscribe.test/one\">Unsubscribe</a>",
      text: "Office content\nUnsubscribe: https://unsubscribe.test/one",
      export_hash: "export-hash",
    };
    const calls: { path: string; init: RequestInit }[] = [];
    let currentExport: Omit<typeof prepared, "provider_campaign_key"> & { provider_campaign_key: string | null } = { ...prepared, state: "PREPARING" };
    let syncAttempts = 0;
    const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(input), "https://office.test").pathname;
      calls.push({ path, init: init ?? {} });
      if (path === "/api/v1/integrations/zoho-campaigns") return response(setup);
      if (path === "/api/v1/marketing/connections/connection-1/fields") return response([{ display_name: "Last Name", field_name: "Last Name", field_id: "field-1" }]);
      if (path === "/api/v1/marketing/connections/connection-1/topics") return response([{ id: "topic-verified", name: "Product Updates", primary_list: "list-1" }]);
      if (path === "/api/v1/marketing/exports" && init?.method === "POST") return response(currentExport, 201);
      if (path === "/api/v1/marketing/exports/export-1/sync") {
        syncAttempts += 1;
        currentExport = syncAttempts === 1
          ? { ...prepared, state: "NEEDS_REVIEW", version: 2, provider_campaign_key: null }
          : { ...prepared, state: "READY", version: 3, provider_campaign_key: "zoho-campaign-1" };
        return response(currentExport);
      }
      if (path === "/api/v1/marketing/exports/export-1/content") return response(content);
      if (path === "/api/v1/marketing/exports/export-1/send") {
        currentExport = { ...prepared, state: "SENT", provider_campaign_key: "zoho-campaign-1" };
        return response(currentExport);
      }
      if (path === "/api/v1/marketing/exports/export-1") {
        return response(currentExport);
      }
      throw new Error(`Unexpected API request: ${path}`);
    });
    vi.stubGlobal("fetch", fetcher);

    const view = mount("awaiting_approval");
    const prepareButton = await screen.findByRole("button", { name: "Prepare Office export" });
    await waitFor(() => expect(prepareButton).toBeEnabled());
    const topic = screen.getByRole("combobox", { name: "Campaigns topic (optional)" });
    await waitFor(() => expect(topic).toBeEnabled());
    fireEvent.click(topic);
    fireEvent.click(await screen.findByRole("option", { name: "Product Updates · topic-verified" }));
    fireEvent.click(screen.getByText("Advanced: map Office variables to verified Zoho fields"));
    expect(await screen.findAllByText("Last Name", { exact: true })).toHaveLength(2);
    fireEvent.change(screen.getByLabelText("Field map JSON"), { target: { value: JSON.stringify({ "contact.name": { contact_field: "Last Name", merge_tag: "$[UD:FNAME]$" } }) } });
    fireEvent.click(prepareButton);
    expect(await screen.findByText("Verify the field names and merge tags in this account before using the mapping.")).toBeVisible();
    expect(calls.filter(({ path, init }) => path === "/api/v1/marketing/exports" && init.method === "POST")).toHaveLength(0);
    fireEvent.click(screen.getByLabelText("I verified each contact field and merge tag in this Zoho Campaigns account."));
    fireEvent.click(prepareButton);
    await waitFor(() => expect(calls.some(({ path, init }) => path === "/api/v1/marketing/exports" && init.method === "POST")).toBe(true));
    await screen.findByText("Office News <news@example.test>");
    fireEvent.click(screen.getByRole("button", { name: "Continue contact sync" }));
    await screen.findByText("NEEDS_REVIEW");
    fireEvent.click(screen.getByRole("button", { name: "Continue contact sync" }));
    await screen.findByText("zoho-campaign-1");
    expect(screen.getByRole("button", { name: "Send campaign" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Preview exported HTML" }));
    const frame = await screen.findByTitle("Isolated exported email preview");
    expect(frame).toHaveAttribute("sandbox", "");
    expect(frame).toHaveAttribute("srcdoc", expect.stringContaining("Office content"));
    expect(screen.getByText("Native unsubscribe is included in each exported message.")).toBeVisible();
    expect(screen.getByText("12")).toBeVisible();

    view.rerender(
      <QueryClientProvider
        client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}
      >
        <NextIntlClientProvider locale="en" messages={{ campaigns, common, settings }}>
          <MarketingExportPanel
            launchId="launch-1"
            sourceStatus="approved"
            sourceManifestHash="source-hash"
            connectionId={connection.id}
          />
        </NextIntlClientProvider>
      </QueryClientProvider>
    );
    const send = await screen.findByRole("button", { name: "Send campaign" });
    expect(send).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Type SEND to confirm"), { target: { value: "SEND" } });
    expect(send).toBeEnabled();
    fireEvent.click(send);
    await waitFor(() => expect(calls.some(({ path }) => path.endsWith("/send"))).toBe(true));

    const prepareCall = calls.find(({ path, init }) => path === "/api/v1/marketing/exports" && init.method === "POST")!;
    const prepareBody = JSON.parse(String(prepareCall.init.body));
    expect(prepareBody.launch_id).toBe("launch-1");
    expect(prepareBody.topic_id).toBe("topic-verified");
    expect(prepareBody.field_map).toEqual({ "contact.name": { contact_field: "Last Name", merge_tag: "$[UD:FNAME]$" } });
    expect(prepareCall.path).not.toContain("zoho-email-api");
    const syncCalls = calls.filter(({ path }) => path.endsWith("/sync"));
    const sendCall = calls.find(({ path }) => path.endsWith("/send"))!;
    expect(syncCalls).toHaveLength(2);
    const syncKeys = syncCalls.map(({ init }) => new Headers(init.headers).get("Idempotency-Key"));
    expect(syncKeys.every(Boolean)).toBe(true);
    expect(syncKeys[0]).not.toBe(syncKeys[1]);
    expect(new Headers(sendCall.init.headers).get("Idempotency-Key")).toBeTruthy();
    expect(JSON.parse(String(sendCall.init.body))).toEqual({
      expected_export_hash: "export-hash",
      confirmation: "SEND",
    });
    expect(calls.some(({ path }) => path.startsWith("/api/v1/zoho-email-api/"))).toBe(false);
  });

  it("does not issue a fresh send after an unknown provider outcome; refresh stays read-only", async () => {
    const calls: { path: string; init: RequestInit }[] = [];
    let state = "PREPARED";
    let prepareAttempts = 0;
    const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(input), "https://office.test").pathname;
      calls.push({ path, init: init ?? {} });
      if (path === "/api/v1/integrations/zoho-campaigns") return response(setup);
      if (path === "/api/v1/marketing/connections/connection-1/fields") return response([{ display_name: "Last Name", field_name: "Last Name", field_id: "field-1" }]);
      if (path === "/api/v1/marketing/connections/connection-1/topics") return response([{ id: "topic-verified", name: "Product Updates", primary_list: "list-1" }]);
      if (path === "/api/v1/marketing/exports" && init?.method === "POST") {
        prepareAttempts += 1;
        if (prepareAttempts === 1) {
          return new Response(JSON.stringify({
            type: "urn:outreach:problem:dependency-unavailable",
            title: "Temporary outage",
            status: 503,
            code: "DEPENDENCY_UNAVAILABLE",
            retryable: true,
          }), { status: 503, headers: { "content-type": "application/problem+json" } });
        }
        return response(prepared, 201);
      }
      if (path.endsWith("/sync")) { state = "READY"; return response({ ...prepared, state, provider_campaign_key: "zoho-campaign-1" }); }
      if (path.endsWith("/send")) {
        return new Response(JSON.stringify({
          type: "urn:outreach:problem:provider-submission-outcome-unknown",
          title: "Provider outcome unknown",
          status: 503,
          code: "PROVIDER_SUBMISSION_OUTCOME_UNKNOWN",
          retryable: true,
        }), { status: 503, headers: { "content-type": "application/problem+json" } });
      }
      if (path.endsWith("/content")) return response({ export_id: "export-1", subject: prepared.subject, html: "<p>Office</p>", text: "Office", export_hash: prepared.export_hash });
      if (path.endsWith("/refresh")) return response({ ...prepared, state, provider_campaign_key: "zoho-campaign-1" });
      if (path.endsWith("/export-1")) return response({ ...prepared, state, provider_campaign_key: state === "READY" ? "zoho-campaign-1" : null });
      throw new Error(`Unexpected API request: ${path}`);
    });
    vi.stubGlobal("fetch", fetcher);

    const view = mount();
    const prepareButton = await screen.findByRole("button", { name: "Prepare Office export" });
    await waitFor(() => expect(prepareButton).toBeEnabled());
    fireEvent.click(prepareButton);
    await screen.findByText("The Office export could not be prepared. Retry to check the same request.");
    fireEvent.click(prepareButton);
    await screen.findByRole("button", { name: "Sync draft to Zoho Campaigns" });
    const prepareCalls = calls.filter(({ path, init }) => path === "/api/v1/marketing/exports" && init.method === "POST");
    expect(prepareCalls).toHaveLength(2);
    expect(new Headers(prepareCalls[0]!.init.headers).get("Idempotency-Key")).toBe(new Headers(prepareCalls[1]!.init.headers).get("Idempotency-Key"));
    fireEvent.click(screen.getByRole("button", { name: "Sync draft to Zoho Campaigns" }));
    const send = await screen.findByRole("button", { name: "Send campaign" });
    fireEvent.change(screen.getByLabelText("Type SEND to confirm"), { target: { value: "SEND" } });
    fireEvent.click(send);
    await screen.findByText("Provider outcome is unknown. Refresh the export status before taking any further action.");
    expect(screen.getByRole("button", { name: "Send campaign" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Refresh export status" }));
    await screen.findByText("READY");
    expect(screen.getByRole("button", { name: "Send campaign" })).toBeDisabled();
    view.unmount();
    mount();
    const reloadedSend = await screen.findByRole("button", { name: "Send campaign" });
    expect(reloadedSend).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Type SEND to confirm"), { target: { value: "SEND" } });
    expect(reloadedSend).toBeDisabled();
    await waitFor(() => expect(calls.filter(({ path }) => path.endsWith("/send"))).toHaveLength(1));
    expect(calls.filter(({ path, init }) => path.endsWith("/export-1") && (!init.method || init.method === "GET")).length).toBeGreaterThan(0);
  });

  it("releases the send latch only for READY with the explicit incomplete opt-out scan reason", async () => {
    const calls: { path: string; init: RequestInit }[] = [];
    let exportData: MarketingExport = prepared;
    let sendAttempts = 0;
    const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(input), "https://office.test").pathname;
      calls.push({ path, init: init ?? {} });
      if (path === "/api/v1/integrations/zoho-campaigns") return response(setup);
      if (path === "/api/v1/marketing/connections/connection-1/fields") return response([{ display_name: "Last Name", field_name: "Last Name", field_id: "field-1" }]);
      if (path === "/api/v1/marketing/connections/connection-1/topics") return response([{ id: "topic-verified", name: "Product Updates", primary_list: "list-1" }]);
      if (path === "/api/v1/marketing/exports" && init?.method === "POST") return response(exportData, 201);
      if (path.endsWith("/sync")) {
        exportData = { ...prepared, state: "READY", version: 1, provider_campaign_key: "zoho-campaign-1" };
        return response(exportData);
      }
      if (path.endsWith("/send")) {
        sendAttempts += 1;
        exportData = sendAttempts === 1
          ? { ...prepared, state: "READY", version: 2, provider_campaign_key: "zoho-campaign-1", report: { reason: "prior_provider_optout_scan_incomplete" } }
          : { ...prepared, state: "SENT", version: 3, provider_campaign_key: "zoho-campaign-1" };
        return response(exportData);
      }
      if (path.endsWith("/export-1")) return response(exportData);
      throw new Error(`Unexpected API request: ${path}`);
    });
    vi.stubGlobal("fetch", fetcher);

    mount();
    const prepare = await screen.findByRole("button", { name: "Prepare Office export" });
    await waitFor(() => expect(prepare).toBeEnabled());
    fireEvent.click(prepare);
    await screen.findByRole("button", { name: "Sync draft to Zoho Campaigns" });
    fireEvent.click(screen.getByRole("button", { name: "Sync draft to Zoho Campaigns" }));
    await screen.findByText("READY", { exact: true });

    const send = screen.getByRole("button", { name: "Send campaign" });
    fireEvent.change(screen.getByLabelText("Type SEND to confirm"), { target: { value: "SEND" } });
    fireEvent.click(send);
    expect(await screen.findByText("Zoho opt-out status is still being checked. No email was sent. Review the refreshed export, then type SEND again to continue.")).toBeVisible();
    expect(send).toBeDisabled();
    expect(localStorage.getItem("office.marketing.send-started.export-1.export-hash")).toBeNull();

    fireEvent.change(screen.getByLabelText("Type SEND to confirm"), { target: { value: "SEND" } });
    expect(send).toBeEnabled();
    fireEvent.click(send);
    await screen.findByText("SENT", { exact: true });
    const sendCalls = calls.filter(({ path }) => path.endsWith("/send"));
    expect(sendCalls).toHaveLength(2);
    expect(new Headers(sendCalls[0]!.init.headers).get("Idempotency-Key")).toBeTruthy();
    expect(new Headers(sendCalls[0]!.init.headers).get("Idempotency-Key")).not.toBe(
      new Headers(sendCalls[1]!.init.headers).get("Idempotency-Key")
    );
  });

  it("releases the send latch for a definite NEEDS_REVIEW response before continuing sync", async () => {
    const calls: { path: string; init: RequestInit }[] = [];
    let exportData: MarketingExport = prepared;
    let sendAttempts = 0;
    const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(input), "https://office.test").pathname;
      calls.push({ path, init: init ?? {} });
      if (path === "/api/v1/integrations/zoho-campaigns") return response(setup);
      if (path === "/api/v1/marketing/connections/connection-1/fields") return response([]);
      if (path === "/api/v1/marketing/connections/connection-1/topics") return response([]);
      if (path === "/api/v1/marketing/exports" && init?.method === "POST") return response(exportData, 201);
      if (path.endsWith("/sync")) {
        exportData = { ...prepared, state: "READY", version: 3, provider_campaign_key: "zoho-campaign-1" };
        return response(exportData);
      }
      if (path.endsWith("/send")) {
        sendAttempts += 1;
        exportData = sendAttempts === 1
          ? { ...prepared, state: "NEEDS_REVIEW", version: 2, provider_campaign_key: null, report: { reason: "manual_review_required" } }
          : { ...prepared, state: "SENT", version: 4, provider_campaign_key: "zoho-campaign-1" };
        return response(exportData);
      }
      if (path.endsWith("/export-1")) return response(exportData);
      throw new Error(`Unexpected API request: ${path}`);
    });
    vi.stubGlobal("fetch", fetcher);

    mount();
    const prepare = await screen.findByRole("button", { name: "Prepare Office export" });
    await waitFor(() => expect(prepare).toBeEnabled());
    fireEvent.click(prepare);
    fireEvent.click(await screen.findByRole("button", { name: "Sync draft to Zoho Campaigns" }));
    await screen.findByText("READY", { exact: true });
    fireEvent.change(screen.getByLabelText("Type SEND to confirm"), { target: { value: "SEND" } });
    fireEvent.click(screen.getByRole("button", { name: "Send campaign" }));

    expect(await screen.findByText("This export needs review before it can be sent. No email was submitted. Continue the export and send only after it returns READY.")).toBeVisible();
    expect(screen.getByRole("button", { name: "Continue contact sync" })).toBeEnabled();
    expect(localStorage.getItem("office.marketing.send-started.export-1.export-hash")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Continue contact sync" }));
    await screen.findByText("READY", { exact: true });
    fireEvent.change(screen.getByLabelText("Type SEND to confirm"), { target: { value: "SEND" } });
    fireEvent.click(screen.getByRole("button", { name: "Send campaign" }));
    await screen.findByText("SENT", { exact: true });

    const sendCalls = calls.filter(({ path }) => path.endsWith("/send"));
    expect(sendCalls).toHaveLength(2);
    expect(new Headers(sendCalls[0]!.init.headers).get("Idempotency-Key")).not.toBe(
      new Headers(sendCalls[1]!.init.headers).get("Idempotency-Key")
    );
  });
});
