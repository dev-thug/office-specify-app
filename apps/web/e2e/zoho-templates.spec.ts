import { expect, test } from "@playwright/test";

test("Zoho templates are a mocked cloud source for an immutable Office version", async ({ page }) => {
  await page.goto("/login");
  await page.locator("#login-email").fill(process.env.E2E_ADMIN_EMAIL!);
  await page.locator("#login-password").fill(process.env.E2E_ADMIN_PASSWORD!);
  await page.locator('button[type="submit"]').click();
  await expect(page).toHaveURL(/\/app\/dashboard/);
  const signedInMe = await page.evaluate(async () => {
    const response = await fetch("/api/v1/me");
    return response.json();
  });
  const workspaceId = signedInMe.data.workspace_id as string;
  expect(workspaceId).toBeTruthy();

  await page.goto("/app/templates");
  await page.getByRole("button", { name: "New template", exact: true }).click();
  const create = page.getByRole("dialog");
  const name = "Zoho import fixture " + crypto.randomUUID();
  await create.getByLabel("Template name", { exact: true }).fill(name);
  await create.getByRole("button", { name: "Create", exact: true }).click();
  await expect(page).toHaveURL(/\/app\/templates\/[0-9a-f-]{36}$/);

  // The test uses only synthetic API responses; it never contacts Zoho or requires its paid product.
  let externalLoads = 0;
  await page.route("https://example.invalid/**", (route) => {
    externalLoads += 1;
    return route.abort();
  });
  await page.route("**/api/v1/integrations/zoho-campaigns", (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ data: { email_api_configured: true, workspace_id: workspaceId } }),
  }));
  await page.route("**/api/v1/zoho-email-api/templates/remote-fixture", (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    headers: { etag: '"fixture-hash"' },
    body: JSON.stringify({ data: {
      id: "remote-fixture",
      name: "Fixture welcome",
      content: '<style>.welcome { color: teal; }</style><p class="welcome">Hello $[Contact Name]$</p><img src="https://example.invalid/pixel">',
      content_type: "html",
      content_hash: "fixture-hash",
    } }),
  }));
  await page.route("**/api/v1/zoho-email-api/templates**", (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path !== "/api/v1/zoho-email-api/templates") return route.fallback();
    return route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ data: [{ id: "remote-fixture", name: "Fixture welcome" }], page: { next_cursor: null, has_more: false } }),
    });
  });

  await page.getByRole("button", { name: "Zoho templates", exact: true }).click();
  const library = page.getByRole("dialog");
  await library.getByRole("button", { name: "Fixture welcome", exact: true }).click();
  await expect(library.getByTitle("Zoho template preview")).toHaveAttribute("sandbox", "");
  await page.screenshot({ path: "test-artifacts/zoho-template-native-preview.png", fullPage: true, animations: "disabled" });
  await expect(library.getByTitle("Zoho template preview")).toBeVisible();
  expect(externalLoads).toBe(0);
  await library.getByRole("button", { name: "Import as new Office version", exact: true }).click();
  await expect(library.getByText("Unsupported HTML will be removed from the Office copy", { exact: true })).toBeVisible();
  await library.getByLabel("Office mapping for Contact Name", { exact: true }).selectOption("contact.name");
  await library.getByRole("button", { name: "Continue to Office editor", exact: true }).click();

  const editor = page.getByRole("dialog");
  await expect(editor.getByLabel("Subject", { exact: true })).toHaveValue("Fixture welcome");
  await expect(editor.getByLabel("Body (text)", { exact: true })).toHaveValue("Hello {{contact.name}}");
  await editor.getByRole("button", { name: "Save version", exact: true }).click();
  await expect(editor).toHaveCount(0);
  await expect(page.getByText("Hello {{contact.name}}", { exact: true })).toBeVisible();
});
