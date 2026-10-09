import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import type { ReactElement, ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { DashboardScreen } from "@/features/dashboard/dashboard-screen";
import enDashboard from "../../../../messages/en/dashboard.json";

const messages = { dashboard: enDashboard };

const dashboardResponse = {
  period: {
    from: "2026-09-09T10:15:00Z",
    to: "2026-10-09T10:15:00Z",
  },
  generated_at: "2026-10-09T10:15:00Z",
  scope: "mine",
  kpis: {
    eligible_contacted_companies: 23,
    reply_company_ratio: { numerator: 4, denominator: 23, value: 4 / 23 },
    meeting_conversion: { numerator: 2, denominator: 23, value: 2 / 23 },
    first_reply: { sample_size: 0, median_seconds: null },
    agent_proposal_adoption: { numerator: 1, denominator: 2, value: 0.5 },
  },
  operational: {
    unknown_outbound_messages: 1,
    unknown_jobs: 2,
    dead_jobs: 3,
    outbox_backlog: 4,
    reauth_required_mailboxes: 5,
    emergency_stop: false,
    send_workers_locked: false,
  },
  queues: [
    { key: "inbox_unhandled", count: 2, oldest_at: "2026-10-09T09:00:00Z" },
    { key: "approvals_pending", count: 1 },
    { key: "meetings_today", count: 3 },
    { key: "outbound_unknown", count: 1 },
    { key: "sync_lag", count: 0 },
    { key: "reauth_required", count: 5 },
    { key: "dead_jobs", count: 3 },
    { key: "outbox_backlog", count: 4 },
  ],
};

function renderDashboard(ui: ReactElement) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const wrapper = ({ children }: { children: ReactNode }) => (
    <NextIntlClientProvider locale="en" messages={messages}>
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    </NextIntlClientProvider>
  );
  return render(ui, { wrapper });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("DashboardScreen", () => {
  it("renders the backend object-shaped KPI DTO and mine-scoped queues", async () => {
    const calls: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        calls.push(url);
        const data =
          url === "/api/v1/dashboard/summary?scope=mine"
            ? dashboardResponse
            : url === "/api/v1/me"
              ? { role: "member" }
              : { enabled: false, version: 1, reason: null, updated_at: null, updated_by: null };
        return new Response(JSON.stringify({ data }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );

    renderDashboard(<DashboardScreen />);

    await waitFor(() =>
      expect(screen.getByText("Reply company ratio")).toBeInTheDocument()
    );
    expect(screen.getByText("Eligible contacted companies")).toBeInTheDocument();
    expect(screen.getAllByText("N/A")).toHaveLength(2);
    expect(screen.getByText("Sample size 0")).toBeInTheDocument();
    expect(screen.getAllByText("Denominator 23")).toHaveLength(2);
    expect(screen.getAllByText(/Observed/)).toHaveLength(5);
    expect(screen.getByRole("link", { name: "Replies to handle 2 — go to area" })).toHaveAttribute(
      "href",
      "/app/inbox"
    );
    expect(screen.getByRole("link", { name: "Mailboxes needing reauthentication 5 — go to area" })).toHaveAttribute(
      "href",
      "/app/settings/mail-accounts"
    );
    expect(screen.getByRole("link", { name: "Dead jobs 3 — go to area" })).toHaveAttribute(
      "href",
      "/app/operations/jobs"
    );
    expect(screen.getByRole("link", { name: "Unknown jobs 2 — go to area" })).toHaveAttribute(
      "href",
      "/app/operations/jobs"
    );
    expect(screen.queryByRole("tab", { name: "Team" })).not.toBeInTheDocument();
    expect(calls).toContain("/api/v1/dashboard/summary?scope=mine");
  });
});
