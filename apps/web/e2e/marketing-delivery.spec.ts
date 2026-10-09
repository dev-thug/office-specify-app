import { expect, test } from "@playwright/test";

async function signInAsAdmin(page: import("@playwright/test").Page) {
  await page.goto("/login");
  await page.locator("#login-email").fill(process.env.E2E_ADMIN_EMAIL!);
  await page.locator("#login-password").fill(process.env.E2E_ADMIN_PASSWORD!);
  await page.locator('button[type="submit"]').click();
  await expect(page).toHaveURL(/\/app\/dashboard/);
}

test("approved frozen Office launch exports to the free Campaigns API with a read-only unknown-outcome check", async ({ page }) => {
  await signInAsAdmin(page);

  const calls: { path: string; method: string; body: unknown; key: string | null }[] = [];
  let exportState = "PREPARING";
  let exportVersion = 1;
  let providerCampaignKey: string | null = null;
  let sendAttempts = 0;
  let syncAttempts = 0;
  const exportResponse = () => ({
    id: "export-1",
    launch_id: "launch-1",
    connection_id: "connection-1",
    state: exportState,
    version: exportVersion,
    source_manifest_hash: "source-hash",
    export_hash: "export-hash",
    content_hash: "content-hash",
    recipient_count: 12,
    active_count: 9,
    pending_count: 2,
    excluded_count: 1,
    sender_address: "newsletter@example.test",
    sender_name: "Office News",
    subject: "A useful update",
    provider_list_key: "list-1",
    topic_id: null,
    provider_campaign_key: providerCampaignKey,
    last_checked_at: null,
    expires_at: "2026-10-11T00:00:00Z",
    created_at: "2026-10-10T00:00:00Z",
    report: {},
  });
  const json = (data: unknown, status = 200) => ({
    status,
    contentType: "application/json",
    body: JSON.stringify({ data }),
  });

  await page.route("**/api/v1/campaigns/campaign-1", (route) => route.fulfill(json({
    id: "campaign-1",
    name: "Office campaign fixture",
    status: "draft",
    mail_account_id: "mail-1",
    version: 1,
    channel_ready: true,
    blocking_reasons: [],
    revision: {
      id: "revision-1",
      campaign_id: "campaign-1",
      revision_no: 1,
      status: "frozen",
      mail_account_id: "mail-1",
      delivery_channel_id: "channel-1",
      timezone: "Asia/Seoul",
      schedule_rules: {},
      stop_on_reply: true,
      stop_on_booking: true,
      steps: [{ step_order: 1, template_version_id: "office-version-1", delay_hours: 0 }],
    },
    launches: [{
      id: "launch-1",
      campaign_id: "campaign-1",
      revision_id: "revision-1",
      delivery_channel_id: "channel-1",
      status: "approved",
      manifest_hash: "source-hash",
      version: 1,
      recipients_count: 12,
      frozen_at: "2026-10-10T00:00:00Z",
      expires_at: "2026-10-11T00:00:00Z",
    }],
    stats: {},
  })));
  await page.route("**/api/v1/delivery-channels?*", (route) => route.fulfill({
    ...json([{
      id: "channel-1",
      mail_account_id: "mail-1",
      sender_identity_id: "sender-1",
      connection_id: "connection-1",
      provider: "zoho_campaigns",
      purpose: "marketing",
      name: "Office Campaigns",
      policy_status: "allowed",
      enabled: true,
      version: 2,
    }]),
    body: JSON.stringify({ data: [{
      id: "channel-1",
      mail_account_id: "mail-1",
      sender_identity_id: "sender-1",
      connection_id: "connection-1",
      provider: "zoho_campaigns",
      purpose: "marketing",
      name: "Office Campaigns",
      policy_status: "allowed",
      enabled: true,
      version: 2,
    }], page: { next_cursor: null, has_more: false } }),
  }));
  await page.route("**/api/v1/integrations/zoho-campaigns", (route) => route.fulfill(json({
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
    connections: [{
      id: "connection-1",
      name: "Office Campaigns",
      status: "active",
      write_scopes_ready: true,
      plan: "free",
      region: "com",
      last_verified_at: "2026-10-10T00:00:00Z",
      lists_count: 1,
      created_at: "2026-10-10T00:00:00Z",
    }],
  })));
  await page.route("**/api/v1/marketing/connections/connection-1/fields", (route) => route.fulfill(json([
    { display_name: "Last Name", field_name: "Last Name", field_id: "field-1" },
  ])));
  await page.route("**/api/v1/marketing/connections/connection-1/topics?*", (route) => route.fulfill(json([
    { id: "topic-1", name: "Product Updates", primary_list: "list-1" },
  ])));
  await page.route("**/api/v1/marketing/exports**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    const method = request.method();
    const body = method === "POST" ? request.postDataJSON() : null;
    calls.push({ path, method, body, key: request.headers()["idempotency-key"] ?? null });

    if (path === "/api/v1/marketing/exports" && method === "POST") {
      return route.fulfill(json(exportResponse(), 201));
    }
    if (path === "/api/v1/marketing/exports/export-1" && method === "GET") {
      return route.fulfill(json(exportResponse()));
    }
    if (path === "/api/v1/marketing/exports/export-1/content") {
      return route.fulfill(json({
        export_id: "export-1",
        subject: "A useful update",
        html: "<p>Office-owned HTML</p><a href=\"https://unsubscribe.test/one\">Unsubscribe</a>",
        text: "Office-owned text\nUnsubscribe: https://unsubscribe.test/one",
        export_hash: "export-hash",
      }));
    }
    if (path === "/api/v1/marketing/exports/export-1/sync" && method === "POST") {
      syncAttempts += 1;
      if (syncAttempts === 1) {
        exportState = "SYNCING";
        exportVersion = 2;
      } else {
        exportState = "READY";
        exportVersion = 3;
        providerCampaignKey = "zoho-campaign-1";
      }
      return route.fulfill(json(exportResponse()));
    }
    if (path === "/api/v1/marketing/exports/export-1/send" && method === "POST") {
      sendAttempts += 1;
      exportState = "UNKNOWN";
      return route.fulfill({
        status: 503,
        contentType: "application/problem+json",
        body: JSON.stringify({
          type: "urn:outreach:problem:provider-submission-outcome-unknown",
          title: "Provider outcome unknown",
          status: 503,
          code: "PROVIDER_SUBMISSION_OUTCOME_UNKNOWN",
          retryable: true,
        }),
      });
    }
    if (path === "/api/v1/marketing/exports/export-1/refresh" && method === "POST") {
      return route.fulfill(json(exportResponse()));
    }
    return route.fulfill({ status: 404, contentType: "application/json", body: "{}" });
  });
  let paidApiCalls = 0;
  await page.route("**/api/v1/zoho-email-api/**", (route) => {
    paidApiCalls += 1;
    return route.abort();
  });

  await page.goto("/app/campaigns/campaign-1");
  const prepare = page.getByRole("button", { name: "Prepare Office export", exact: true });
  await expect(prepare).toBeEnabled();
  await prepare.click();
  await expect(page.getByText("Office News <newsletter@example.test>")).toBeVisible();
  await expect(page.getByText("12", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Continue contact sync", exact: true }).click();
  await expect(page.getByText("SYNCING", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Continue contact sync", exact: true }).click();
  await expect(page.getByText("zoho-campaign-1", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "Preview exported HTML", exact: true }).click();
  const frame = page.getByTitle("Isolated exported email preview");
  await expect(frame).toHaveAttribute("sandbox", "");
  await expect(frame).toHaveAttribute("srcdoc", /Office-owned HTML/);
  await expect(page.getByText("Native unsubscribe is included in each exported message.")).toBeVisible();

  const send = page.getByRole("button", { name: "Send campaign", exact: true });
  await expect(send).toBeDisabled();
  await page.getByLabel("Type SEND to confirm", { exact: true }).fill("SEND");
  await expect(send).toBeEnabled();
  await send.click();
  await expect(page.getByText("Provider outcome is unknown. Refresh the export status before taking any further action.")).toBeVisible();
  await expect(send).toBeDisabled();
  await page.getByRole("button", { name: "Refresh export status", exact: true }).click();
  await expect(page.getByText("UNKNOWN", { exact: true })).toBeVisible();

  expect(sendAttempts).toBe(1);
  expect(syncAttempts).toBe(2);
  const syncCalls = calls.filter((call) => call.path.endsWith("/sync"));
  expect(syncCalls).toHaveLength(2);
  expect(syncCalls[0]?.key).toBeTruthy();
  expect(syncCalls[0]?.key).not.toBe(syncCalls[1]?.key);
  const sendCall = calls.find((call) => call.path.endsWith("/send"));
  expect(sendCall?.body).toEqual({ expected_export_hash: "export-hash", confirmation: "SEND" });
  expect(sendCall?.key).toBeTruthy();
  expect(calls.filter((call) => call.path.endsWith("/send"))).toHaveLength(1);
  expect(calls.some((call) => call.path === "/api/v1/marketing/exports/export-1/refresh" && call.method === "POST")).toBe(true);
  expect(paidApiCalls).toBe(0);
});

test("template details keep the Office authoring path without a Zoho template import", async ({ page }) => {
  await signInAsAdmin(page);

  const version = {
    template_version_id: "office-version-1",
    template_id: "office-template-1",
    version_no: 1,
    subject: "Office subject",
    body_text: "Hello {{contact.name}}",
    body_html: "<p>Hello {{contact.name}}</p>",
    variable_schema: { type: "object", properties: { "contact.name": { type: "string" } }, required: ["contact.name"] },
    content_hash: "office-content-hash",
    created_at: "2026-10-10T00:00:00Z",
  };
  await page.route("**/api/v1/email-templates/office-template-1/versions?*", (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ data: [{ template_version_id: version.template_version_id, version_no: 1, content_hash: version.content_hash, created_at: version.created_at }], page: { next_cursor: null, has_more: false } }),
  }));
  await page.route("**/api/v1/email-templates/office-template-1", (route) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ data: { id: "office-template-1", name: "Office-owned template", archived_at: null, created_at: "2026-10-10T00:00:00Z" } }) }));
  await page.route("**/api/v1/template-versions/office-version-1", (route) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ data: version }) }));
  let paidApiCalls = 0;
  await page.route("**/api/v1/zoho-email-api/**", (route) => {
    paidApiCalls += 1;
    return route.abort();
  });

  await page.goto("/app/templates/office-template-1");
  await expect(page.getByRole("heading", { name: "Office-owned template", exact: true })).toBeVisible();
  await expect(page.getByText("Hello {{contact.name}}", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "New version", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Zoho templates", exact: true })).toHaveCount(0);
  expect(paidApiCalls).toBe(0);
});

test("native approval is locked until READY and binds the displayed native export hash", async ({ page }) => {
  await signInAsAdmin(page);

  let nativeState = "syncing";
  let decisionBody: unknown;
  const request = () => ({
    id: "approval-native-1",
    action_type: "campaign_launch",
    status: "pending",
    requested_by: "requester-1",
    requester_display_name: "Office requester",
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
      state: nativeState,
      subject: "Mapped Campaigns subject",
      body_html: "<p>Mapped Campaigns HTML</p>",
      field_map: { "contact.name": { contact_field: "Last Name", merge_tag: "$[UD:FNAME]$" } },
      recipient_count: 9,
      sender_address: "campaigns@example.test",
      provider_list_key: "zoho-list-1",
      topic_id: "topic-1",
    },
  });
  await page.route("**/api/v1/me", (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ data: { principal_id: "approver-1", role: "manager" } }),
  }));
  await page.route("**/api/v1/approvals/approval-native-1", (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    headers: { etag: '"4"' },
    body: JSON.stringify({ data: request() }),
  }));
  await page.route("**/api/v1/approvals/approval-native-1/decisions", async (route) => {
    decisionBody = route.request().postDataJSON();
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ data: { approval_status: "approved" } }) });
  });

  await page.goto("/app/approvals/approval-native-1");
  await expect(page.getByTitle("Native Campaigns HTML preview")).toHaveAttribute("sandbox", "");
  await expect(page.getByText("$[UD:FNAME]$", { exact: true })).toBeVisible();
  const approve = page.getByRole("button", { name: "Approve", exact: true });
  await expect(approve).toBeDisabled();

  nativeState = "ready";
  await page.reload();
  await expect(page.getByRole("button", { name: "Approve", exact: true })).toBeEnabled();
  await page.getByRole("button", { name: "Approve", exact: true }).click();
  await page.getByLabel('Type "approve" to confirm', { exact: true }).fill("approve");
  await page.getByRole("button", { name: "Confirm approval", exact: true }).click();
  await expect.poll(() => decisionBody).toEqual({
    decision: "approved",
    expected_snapshot_hash: "office-snapshot-hash",
    expected_export_hash: "native-export-hash",
  });
});
