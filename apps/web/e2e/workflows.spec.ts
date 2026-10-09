import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";

test("authenticated CRM persistence and connected application screens", async ({ page, context }) => {
  const email = process.env.E2E_ADMIN_EMAIL;
  const password = process.env.E2E_ADMIN_PASSWORD;
  expect(email, "E2E_ADMIN_EMAIL is required").toBeTruthy();
  expect(password, "E2E_ADMIN_PASSWORD is required").toBeTruthy();
  await page.goto("/login");
  await page.locator("#login-email").fill(email!);
  await page.locator("#login-password").fill(password!);
  await page.locator('button[type="submit"]').click();
  await expect(page).toHaveURL(/\/app\/dashboard/);
  const csrf = (await context.cookies()).find(c => c.name === "outreach_csrf")?.value;
  expect(csrf).toBeTruthy();
  const name = "Browser QA " + crypto.randomUUID();
  const key = crypto.randomUUID();
  const create = () => context.request.post("/api/v1/companies", {
    headers: { "X-CSRF-Token": csrf!, "Idempotency-Key": key }, data: { name },
  });
  const first = await create();
  expect(first.ok(), await first.text()).toBeTruthy();
  const company = (await first.json()).data;
  const replay = await create();
  expect(replay.ok(), await replay.text()).toBeTruthy();
  expect((await replay.json()).data.id).toBe(company.id);
  const noCsrf = await context.request.post("/api/v1/companies", { data: { name: "must not persist" } });
  expect(noCsrf.status()).toBe(403);
  await page.goto("/app/companies/" + company.id);
  await expect(page.getByRole("heading", { name, exact: true })).toBeVisible();
  await page.reload();
  await expect(page.getByRole("heading", { name, exact: true })).toBeVisible();
  for (const path of ["companies", "contacts", "inbox", "inbox/drafts", "templates", "campaigns", "approvals", "meetings", "meeting-types", "agents", "imports", "operations/jobs", "operations/integrations", "settings/members", "settings/mail-accounts", "settings/calendars", "settings/policies", "compliance/suppressions", "compliance/reviews", "settings/security"]) {
    await page.goto("/app/" + path);
    await page.waitForLoadState("networkidle");
    await expect(page.locator("html#__next_error__"), path).toHaveCount(0);
    await expect(page.locator("h1").first(), path).toBeVisible();
    await expect(page.locator('[data-slot="error-state"]'), path).toHaveCount(0);
    await expect(page.locator("body"), path).not.toContainText("Application error");
  }
  await page.goto("/app/dashboard");
  await page.waitForLoadState("networkidle");
  await expect(page.getByRole("heading", { name: "Dashboard", exact: true })).toBeVisible();
  await page.screenshot({ path: "test-artifacts/workflow-dashboard.png", fullPage: true });
});

test("contact message freezes, confirms the recipient, and resumes queued state", async ({ page }) => {
  const fixture = JSON.parse(readFileSync(process.env.E2E_FIXTURE_PATH ?? "/tmp/outreach-browser-fixture.json", "utf8"));
  await page.goto("/login");
  await page.locator("#login-email").fill(process.env.E2E_ADMIN_EMAIL!);
  await page.locator("#login-password").fill(process.env.E2E_ADMIN_PASSWORD!);
  await page.locator('button[type="submit"]').click();
  await expect(page).toHaveURL(/\/app\/dashboard/);
  await page.goto("/app/contacts/" + fixture.contact_id);
  await page.getByRole("button", { name: "New message", exact: true }).click();
  const compose = page.getByRole("dialog", { name: "New message draft", exact: true });
  await compose.getByRole("combobox", { name: "From", exact: true }).click();
  await page.getByRole("option", { name: fixture.mailbox_address, exact: true }).click();
  await compose.getByRole("combobox", { name: "Purpose", exact: true }).click();
  await page.getByRole("option", { name: "Correspondence", exact: true }).click();
  const subject = "Synthetic send QA " + crypto.randomUUID();
  const body = "Synthetic browser QA only. No real mail provider is configured.";
  await compose.getByLabel("Subject", { exact: true }).fill(subject);
  await compose.getByLabel("Message", { exact: true }).fill(body);
  const createResponse = page.waitForResponse(response => response.request().method() === "POST" && new URL(response.url()).pathname === "/api/v1/message-drafts");
  await compose.getByRole("button", { name: "Create draft", exact: true }).click();
  const created = await createResponse;
  expect(created.status(), await created.text()).toBe(201);
  const draftId = (await created.json()).data.draft_id;
  await compose.getByRole("combobox", { name: "Delivery channel", exact: true }).selectOption({ label: fixture.channel_name });
  const freezeResponse = page.waitForResponse(response => response.request().method() === "POST" && new URL(response.url()).pathname.endsWith("/freeze"));
  await compose.getByRole("button", { name: "Freeze exact message", exact: true }).click();
  const frozen = await freezeResponse;
  expect(frozen.status(), await frozen.text()).toBe(201);
  await compose.getByRole("button", { name: "Check send policy", exact: true }).click();
  await expect(compose.getByRole("button", { name: "Submit message", exact: true })).toBeEnabled();
  await compose.getByRole("button", { name: "Submit message", exact: true }).click();
  const confirm = page.getByRole("dialog", { name: "Queue this exact message?", exact: true });
  await expect(confirm.getByText(fixture.email, { exact: false })).toBeVisible();
  await expect(confirm.getByText(subject, { exact: true })).toBeVisible();
  await expect(confirm.getByText(body, { exact: true })).toBeVisible();
  await expect(confirm.getByRole("button", { name: "Queue this message", exact: true })).toBeDisabled();
  await confirm.getByRole("textbox").fill("SEND");
  const submitResponse = page.waitForResponse(response => response.request().method() === "POST" && new URL(response.url()).pathname.endsWith("/submit"));
  await confirm.getByRole("button", { name: "Queue this message", exact: true }).click();
  const submitted = await submitResponse;
  expect(submitted.status(), await submitted.text()).toBe(202);
  await page.goto("/app/inbox/drafts/" + draftId);
  await expect(page.getByText(/Message queued\. Provider acceptance/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Submit message", exact: true })).toHaveCount(0);
});

test("anonymous browser cannot open internal screens", async ({ page }) => {
  await page.goto("/app/companies");
  await expect(page).toHaveURL(/\/login/);
});
