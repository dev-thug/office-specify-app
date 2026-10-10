import { expect, test } from "@playwright/test";

async function signInAsAdmin(page: import("@playwright/test").Page) {
  await page.goto("/login");
  await page.locator("#login-email").fill(process.env.E2E_ADMIN_EMAIL!);
  await page.locator("#login-password").fill(process.env.E2E_ADMIN_PASSWORD!);
  await page.locator('button[type="submit"]').click();
  await expect(page).toHaveURL(/\/app\/dashboard/);
}

test("Office template details use local authoring", async ({ page }) => {
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
  let remoteTemplateCalls = 0;
  await page.route("**/api/v1/zoho-email-api/**", (route) => {
    remoteTemplateCalls += 1;
    return route.abort();
  });

  await page.goto("/app/templates/office-template-1");
  await expect(page.getByRole("heading", { name: "Office-owned template", exact: true })).toBeVisible();
  await expect(page.getByText("Hello {{contact.name}}", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "New version", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Zoho templates", exact: true })).toHaveCount(0);
  expect(remoteTemplateCalls).toBe(0);
});
