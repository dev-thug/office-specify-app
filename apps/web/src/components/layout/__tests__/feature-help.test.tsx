import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { FeatureHelp, UsageGuide } from "../feature-help";
import { flattenNav } from "@/lib/nav";
import ko from "../../../../messages/ko/nav.json";
import en from "../../../../messages/en/nav.json";

beforeEach(() => vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} }));
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe("feature usage guidance", () => {
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
