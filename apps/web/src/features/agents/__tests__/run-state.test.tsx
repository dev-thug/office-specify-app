import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AgentsTable } from "@/features/agents/agents-table";

import {
  canCancelRun,
  isActiveRunStatus,
  isFinalRunStatus,
} from "@/features/agents";

describe("run-state", () => {
  it("treats queued/running/awaiting_approval as active", () => {
    for (const s of ["queued", "running", "awaiting_approval"]) {
      expect(isActiveRunStatus(s)).toBe(true);
      expect(isFinalRunStatus(s)).toBe(false);
    }
  });

  it("treats completed/failed/canceled as final and inactive", () => {
    for (const s of ["completed", "failed", "canceled"]) {
      expect(isFinalRunStatus(s)).toBe(true);
      expect(isActiveRunStatus(s)).toBe(false);
    }
  });

  it("allows cancel only while active and not already requested", () => {
    expect(canCancelRun({ status: "running" })).toBe(true);
    expect(
      canCancelRun({ status: "running", cancel_requested_at: "2026-10-09T00:00:00Z" })
    ).toBe(false);
    expect(canCancelRun({ status: "completed" })).toBe(false);
    expect(canCancelRun({ status: "failed" })).toBe(false);
  });

  it("honours allowed_actions when the server sends them", () => {
    expect(
      canCancelRun({ status: "queued", allowed_actions: ["cancel"] })
    ).toBe(true);
    expect(canCancelRun({ status: "queued", allowed_actions: [] })).toBe(false);
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("agent list labels", () => {
  it("uses the principal name and keeps its UUID out of the visible table", async () => {
    const principalId = "4b6c0922-b98e-44af-83f2-5bb3be064dec";
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(JSON.stringify({
          data: [{
            id: principalId,
            kind: "agent",
            name: "Outreach researcher",
            status: "active",
            scopes: ["crm.read"],
          }],
          page: { has_more: false, next_cursor: null },
        }), { status: 200, headers: { "content-type": "application/json" } })
      )
    );
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const messages = {
      agents: { agentsList: { columns: { name: "Name", status: "Status", scopes: "Scopes" }, noScopes: "No scopes", empty: "No agents", emptyDescription: "No agents" } },
      common: { table: { empty: "Nothing", loading: "Loading", loadMore: "Load more", rowsSelected: "{count}", allFilteredSelected: "{count}", selectAllFiltered: "{count}", clearSelection: "Clear", selectVisibleRows: "Select visible rows", selectRow: "Select row" } },
      status: { active: "Active" },
    };
    render(
      <NextIntlClientProvider locale="en" messages={messages}>
        <QueryClientProvider client={queryClient}>
          <AgentsTable />
        </QueryClientProvider>
      </NextIntlClientProvider>
    );

    expect(await screen.findByText("Outreach researcher")).toBeInTheDocument();
    expect(screen.queryByText(principalId)).not.toBeInTheDocument();
  });
});
