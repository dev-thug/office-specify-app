import { expect, test } from "@playwright/test";

test("template create, version save, reload, selection and variable preview use real API IDs", async ({ page }) => {
  await page.goto("/login");
  await page.locator("#login-email").fill(process.env.E2E_ADMIN_EMAIL!);
  await page.locator("#login-password").fill(process.env.E2E_ADMIN_PASSWORD!);
  await page.locator('button[type="submit"]').click();
  await expect(page).toHaveURL(/\/app\/dashboard/);
  await page.goto("/app/templates");
  await page.getByRole("button", { name: "New template", exact: true }).click();
  const create = page.getByRole("dialog");
  const name = "Template regression " + crypto.randomUUID();
  await create.getByLabel("Template name", { exact: true }).fill(name);
  await create.getByRole("button", { name: "Create", exact: true }).click();
  await expect(page).toHaveURL(/\/app\/templates\/[0-9a-f-]{36}$/);
  await expect(page.getByRole("heading", { name, exact: true })).toBeVisible();
  for (const no of [1, 2]) {
    await page.getByRole("button", { name: "New version", exact: true }).click();
    const editor = page.getByRole("dialog");
    await editor.getByLabel("Subject", { exact: true }).fill("Version " + no);
    await editor.getByLabel("Body (text)", { exact: true }).fill(no === 1 ? "Hello {{contact.name}}" : "Saved body 2");
    if (no === 1) {
      await editor.getByRole("button", { name: "Add variable", exact: true }).click();
      await editor.getByLabel("Variable name", { exact: true }).fill("contact.name");
      await editor.getByRole("checkbox", { name: "Required", exact: true }).check();
    }
    await editor.getByRole("button", { name: "Save version", exact: true }).click();
    await expect(editor).toHaveCount(0);
    await expect(page.getByText(no === 1 ? "Hello {{contact.name}}" : "Saved body 2", { exact: true })).toBeVisible();
  }
  await page.reload();
  await expect(page.getByText("Saved body 2", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: /^v1/ }).click();
  await expect(page.getByText("Hello {{contact.name}}", { exact: true })).toBeVisible();
  await page.getByLabel("Sample variables (JSON)", { exact: true }).fill(JSON.stringify({"contact.name":"Office QA"}));
  await page.getByRole("button", { name: "Render", exact: true }).click();
  await expect(page.getByText("Hello Office QA", { exact: true })).toBeVisible();
  await page.getByRole("link", { name: "Template list", exact: true }).click();
  const row = page.getByRole("row").filter({ hasText: name });
  await expect(row).toContainText("v2");
  await row.getByRole("link", { name, exact: true }).click();
  await expect(page.getByRole("heading", { name, exact: true })).toBeVisible();
  await expect(page.getByText("Saved body 2", { exact: true })).toBeVisible();
  await page.screenshot({path:"test-artifacts/template-fixed.png", fullPage:true});
});
