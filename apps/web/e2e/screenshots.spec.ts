import { test } from "@playwright/test";

const ARTIFACTS = "test-artifacts";

/** 시각 QA — §7.3 화면 스텁과 디자인 카탈로그를 두 테마로 캡처한다. */
test.describe("visual QA", () => {
  test("login page", async ({ page }) => {
    await page.goto("/login");
    await page.waitForLoadState("networkidle");
    await page.screenshot({ path: `${ARTIFACTS}/login.png`, fullPage: true });
  });

  test("dashboard with session cookie", async ({ context, page }) => {
    await context.addCookies([
      {
        name: "outreach_session",
        value: "dummy-session-token",
        url: "http://127.0.0.1:3000",
      },
    ]);
    await page.goto("/app/dashboard");
    await page.waitForLoadState("networkidle");
    await page.screenshot({ path: `${ARTIFACTS}/app-dashboard.png`, fullPage: true });
  });

  test("design catalog light + dark", async ({ context, page }) => {
    await context.addCookies([
      {
        name: "outreach_session",
        value: "dummy-session-token",
        url: "http://127.0.0.1:3000",
      },
    ]);
    await page.goto("/app/design");
    await page.waitForLoadState("networkidle");
    // 라이트 칼럼 (기본) — 첫 번째 그리드 아이템
    await page
      .locator(".grid > div:not(.dark)")
      .first()
      .screenshot({ path: `${ARTIFACTS}/app-design-light.png` });
    // 다크 칼럼 — .dark 스코프
    await page
      .locator("div.dark")
      .first()
      .screenshot({ path: `${ARTIFACTS}/app-design-dark.png` });
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
