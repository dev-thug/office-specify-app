import { expect, test } from "@playwright/test";

test("Office template authoring is the source and never loads remote Zoho templates", async ({ page }) => {
  await page.goto("/login");
  await page.locator("#login-email").fill(process.env.E2E_ADMIN_EMAIL!);
  await page.locator("#login-password").fill(process.env.E2E_ADMIN_PASSWORD!);
  await page.locator('button[type="submit"]').click();
  await expect(page).toHaveURL(/\/app\/dashboard/);

  let remoteTemplateCalls = 0;
  await page.route("**/api/v1/zoho-email-api/**", (route) => {
    remoteTemplateCalls += 1;
    return route.abort();
  });

  await page.goto("/app/templates");
  await page.getByRole("button", { name: "New template", exact: true }).click();
  const create = page.getByRole("dialog");
  const name = `Office-owned source ${crypto.randomUUID()}`;
  await create.getByLabel("Template name", { exact: true }).fill(name);
  await create.getByRole("button", { name: "Create", exact: true }).click();
  await expect(page).toHaveURL(/\/app\/templates\/[0-9a-f-]{36}$/);
  await expect(page.getByRole("heading", { name, exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Zoho templates", exact: true })).toHaveCount(0);

  await page.getByRole("button", { name: "New version", exact: true }).click();
  const editor = page.getByRole("dialog");
  await editor.getByLabel("Subject", { exact: true }).fill("Hello {{company.name}}");
  const body = editor.getByRole("textbox", { name: "Email message", exact: true });
  await body.fill("Hello ");
  await body.press("End");
  await editor.getByRole("button", { name: "Insert Contact name", exact: true }).click();
  await body.press("End");
  await body.press("Enter");
  await body.pressSequentially("Welcome to Office");

  await editor.getByRole("tab", { name: "HTML code", exact: true }).click();
  const code = editor.getByRole("textbox", { name: "HTML code", exact: true });
  await expect(code).toHaveValue(/\{\{contact\.name\}\}/);
  const officeHtml = "<p>Hello <strong>{{contact.name}}</strong> at {{company.name}}.</p>";
  await code.fill(officeHtml);
  await editor.getByRole("tab", { name: "Compose", exact: true }).click();
  await expect(body.locator("strong")).toHaveText("{{contact.name}}");
  await expect(editor.getByLabel("Body (text)", { exact: true })).toHaveValue("Hello {{contact.name}} at {{company.name}}.");

  await editor.getByRole("tab", { name: "Recipient preview", exact: true }).click();
  const preview = editor.frameLocator('iframe[title="Recipient email preview"]');
  await expect(preview.getByText("Hello Jina Kim at Example Company.", { exact: true })).toBeVisible();

  const saveRequest = page.waitForRequest((request) =>
    request.method() === "POST" && new URL(request.url()).pathname.endsWith("/versions")
  );
  await editor.getByRole("button", { name: "Save version", exact: true }).click();
  const saved = (await saveRequest).postDataJSON();
  expect(saved.body_html).toBe(officeHtml);
  expect(saved.body_text).toBe("Hello {{contact.name}} at {{company.name}}.");
  expect(Object.keys(saved.variable_schema.properties).sort()).toEqual([
    "company.name",
    "contact.name",
  ]);
  await expect(editor).toHaveCount(0);
  await page.reload();
  await expect(page.getByText("Hello {{contact.name}} at {{company.name}}.", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Zoho templates", exact: true })).toHaveCount(0);
  expect(remoteTemplateCalls).toBe(0);
});
