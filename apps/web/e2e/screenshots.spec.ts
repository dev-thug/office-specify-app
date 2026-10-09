import { expect, test, type Page } from "@playwright/test";

async function authenticate(page: Page) {
  const email = process.env.E2E_ADMIN_EMAIL;
  const password = process.env.E2E_ADMIN_PASSWORD;
  if (!email || !password) throw new Error("E2E_ADMIN_EMAIL and E2E_ADMIN_PASSWORD are required for authenticated screenshots");
  await page.goto("/login");
  await page.locator("#login-email").fill(email);
  await page.locator("#login-password").fill(password);
  await page.locator('button[type="submit"]').click();
  await expect(page).toHaveURL(/\/app\/dashboard/);
}

const ARTIFACTS = "test-artifacts";

/** 시각 QA — §7.3 실제 화면과 디자인 카탈로그를 두 테마로 캡처한다. */
test.describe("visual QA", () => {
  test("login page", async ({ page }) => {
    await page.goto("/login");
    await page.waitForLoadState("networkidle");
    await page.screenshot({ path: `${ARTIFACTS}/login.png`, fullPage: true });
  });

  test("dashboard with verified session", async ({ page }) => {
    await authenticate(page);
    await page.goto("/app/dashboard");
    await page.waitForLoadState("networkidle");
    await expect(page.getByRole("heading", { name: "Dashboard", exact: true })).toBeVisible();
    await page.screenshot({ path: `${ARTIFACTS}/app-dashboard.png`, fullPage: true });
  });

  test("dashboard light + dark", async ({ page }) => {
    await page.emulateMedia({ colorScheme: "light" });
    await authenticate(page);
    await page.goto("/app/dashboard");
    await page.waitForLoadState("networkidle");
    await expect(page.getByRole("heading", { name: "Dashboard", exact: true })).toBeVisible();
    await expect(page.locator("html")).not.toHaveClass(/(?:^|\s)dark(?:\s|$)/);
    await page.screenshot({ path: `${ARTIFACTS}/app-dashboard-light.png`, fullPage: true });
    await page.emulateMedia({ colorScheme: "dark" });
    await expect(page.locator("html")).toHaveClass(/(?:^|\s)dark(?:\s|$)/);
    await page.screenshot({ path: `${ARTIFACTS}/app-dashboard-dark.png`, fullPage: true });
  });

  test("public unsubscribe", async ({ page }) => {
    await page.goto("/public/unsubscribe/test-token");
    await page.waitForLoadState("networkidle");
    await page.screenshot({
      path: `${ARTIFACTS}/public-unsubscribe.png`,
      fullPage: true,
    });
  });
});
