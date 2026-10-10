import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { FeatureHelp, UsageGuide } from "../feature-help";
import { SettingsHub } from "@/features/settings/settings-hub";
import { flattenNav, NAV_SECTIONS, primaryNavItemForPath } from "@/lib/nav";
import ko from "../../../../messages/ko/nav.json";
import en from "../../../../messages/en/nav.json";

beforeEach(() => vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} }));
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe("feature usage guidance", () => {
  it("keeps only seven primary destinations while preserving secondary routes for search and help", () => {
    const primaryKeys = NAV_SECTIONS.flatMap((section) =>
      section.items.map((item) => item.key)
    );
    expect(primaryKeys).toEqual([
      "dashboard",
      "companies",
      "campaigns",
      "inbox",
      "templates",
      "meetings",
      "settings",
    ]);

    const allKeys = flattenNav().map((item) => item.key);
    expect(allKeys).toEqual(expect.arrayContaining([
      "contacts",
      "approvals",
      "meetingTypes",
      "agents",
      "imports",
      "compliance",
      "complianceReviews",
      "operations",
      "operationsIntegrations",
      "settingsMembers",
      "settingsMailAccounts",
      "settingsCalendars",
      "settingsPolicies",
      "settingsSecurity",
    ]));

    const expectedPrimaryByPath = [
      ["/app/contacts/contact-1", "companies"],
      ["/app/approvals/approval-1", "campaigns"],
      ["/app/meeting-types", "meetings"],
      ["/app/agents/runs/run-1", "settings"],
      ["/app/imports/import-1", "settings"],
      ["/app/compliance/reviews", "settings"],
      ["/app/operations/integrations", "settings"],
      ["/app/settings/security", "settings"],
    ] as const;
    for (const [path, expectedKey] of expectedPrimaryByPath) {
      expect(primaryNavItemForPath(path)?.key, path).toBe(expectedKey);
    }
  });
  it("includes explicit MCP aliases on the Members route in the shared command-palette source", () => {
    const memberRoute = flattenNav().find((item) => item.key === "settingsMembers");
    expect(memberRoute?.searchTerms).toEqual(
      expect.arrayContaining([
        "MCP",
        "API key",
        "service credential",
        "MCP 자격 증명",
        "API 키",
      ])
    );
  });

  it("separates agent run discovery from MCP credential management", () => {
    expect(en.agents).toBe("Agents & runs");
    expect(en.settingsMembers).toBe("Members & agent access");
    expect(en.help.features.agents.summary).not.toContain("MCP service credentials");
    expect(en.help.features.settingsMembers.summary).toContain("MCP service credentials");
    expect(ko.agents).toBe("에이전트·실행");
    expect(ko.settingsMembers).toBe("멤버·에이전트 접근");
    expect(ko.help.features.settingsMembers.summary).toContain("MCP 서비스 credential");
  });

  it("lists settings-only routes in the settings hub and keeps workflow shortcuts in their own sections", () => {
    render(
      <NextIntlClientProvider locale="en" messages={{ nav: en }}>
        <SettingsHub />
      </NextIntlClientProvider>
    );
    const links = screen.getAllByRole("link");
    const hrefs = links.map((link) => link.getAttribute("href"));
    expect(hrefs).toEqual(expect.arrayContaining([
      "/app/settings/members",
      "/app/settings/mail-accounts",
      "/app/settings/calendars",
      "/app/settings/policies",
      "/app/settings/security",
      "/app/agents",
      "/app/imports",
      "/app/compliance/suppressions",
      "/app/compliance/reviews",
      "/app/operations/jobs",
      "/app/operations/integrations",
    ]));
    expect(hrefs).not.toContain("/app/contacts");
    expect(hrefs).not.toContain("/app/approvals");
    expect(hrefs).not.toContain("/app/meeting-types");
  });

  it.each([['ko', ko], ['en', en]])("explains every navigation feature in %s", (_, messages) => {
    const entries = (messages as unknown as {help?: {features?: Record<string, {summary: string; when: string; steps: string[]}>}}).help?.features;
    for (const item of flattenNav()) {
      expect(entries?.[item.key], item.key).toBeDefined();
      expect(entries?.[item.key]?.summary.length).toBeGreaterThan(20);
      expect(entries?.[item.key]?.when.length).toBeGreaterThan(10);
      expect(entries?.[item.key]?.steps.length).toBeGreaterThanOrEqual(3);
    }
  });

  it("opens feature instructions without navigation and restores keyboard focus on Escape", async () => {
    render(<NextIntlClientProvider locale="ko" messages={{nav: ko}}><FeatureHelp featureKey="campaigns" /></NextIntlClientProvider>);
    const trigger = screen.getByRole("button", {name: "캠페인 사용 도움말"});
    trigger.focus();
    expect(await screen.findByRole("tooltip")).toHaveTextContent(ko.help.features.campaigns.summary);
    fireEvent.click(trigger);
    const dialog = await screen.findByRole("dialog", {name: "캠페인 사용 도움말"});
    expect(within(dialog).getAllByRole("listitem")).toHaveLength(4);
    fireEvent.keyDown(dialog, {key: "Escape"});
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(trigger).toHaveFocus();
  });

  it("offers a getting-started flow and instructions for every feature", async () => {
    render(<NextIntlClientProvider locale="ko" messages={{nav: ko}}><UsageGuide /></NextIntlClientProvider>);
    fireEvent.click(screen.getByRole("button", {name: "사용 가이드"}));
    const dialog = await screen.findByRole("dialog", {name: "사용 가이드"});
    expect(within(dialog).getByRole("region", {name: "처음 사용하는 순서"})).toBeInTheDocument();
    expect(within(within(dialog).getByRole("region", {name: "처음 사용하는 순서"})).getByRole("link", {name: "캠페인 열기"})).toHaveAttribute("href", "/app/campaigns");
    expect(dialog.querySelectorAll("details")).toHaveLength(flattenNav().length);
  });
});
