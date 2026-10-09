import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import type { ReactElement, ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { LeadProposalReviewList } from "@/features/research";
import type { LeadProposal } from "@/lib/api/research";
import enCommon from "../../../../messages/en/common.json";
import enResearch from "../../../../messages/en/research.json";
import enStatus from "../../../../messages/en/status.json";

const messages = { research: enResearch, common: enCommon, status: enStatus };

function proposal(over: Partial<LeadProposal>): LeadProposal {
  return {
    id: "p1",
    agent_run_id: null,
    proposed_by: "agent-1",
    external_key: null,
    company_name: "Acme Corp",
    domain: "acme.example",
    proposed_data: {},
    status: "pending",
    accepted_company_id: null,
    reviewed_by: null,
    reviewed_at: null,
    version: 2,
    created_at: "2026-10-09T01:00:00Z",
    ...over,
  };
}

function mockListFetch(items: LeadProposal[]) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () =>
      new Response(
        JSON.stringify({
          data: items,
          meta: { request_id: "req_test" },
          page: { next_cursor: null, has_more: false },
        }),
        { status: 200, headers: { "content-type": "application/json" } }
      )
    )
  );
}

function renderList(ui: ReactElement) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const wrap = ({ children }: { children: ReactNode }) => (
    <NextIntlClientProvider locale="en" messages={messages}>
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    </NextIntlClientProvider>
  );
  return render(ui, { wrapper: wrap });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("LeadProposalReviewList", () => {
  it("shows accept/reject only on decidable statuses", async () => {
    mockListFetch([
      proposal({ id: "p1", status: "pending", company_name: "Pending Co" }),
      proposal({
        id: "p2",
        status: "accepted",
        company_name: "Done Co",
        accepted_company_id: "c1",
      }),
    ]);
    renderList(<LeadProposalReviewList />);
    await waitFor(() =>
      expect(screen.getByText("Pending Co")).toBeInTheDocument()
    );
    // pending 카드에는 채택/거절 버튼이, accepted 카드에는 없다.
    expect(screen.getByRole("button", { name: "Accept" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reject" })).toBeInTheDocument();
    const doneCard = screen.getByText("Done Co").closest("[data-slot='card']");
    expect(doneCard?.querySelector("button")).toBeNull();
    // 채택된 후보는 기업 링크를 보여준다.
    expect(
      screen.getByRole("link", { name: "View company" })
    ).toHaveAttribute("href", "/app/companies/c1");
  });

  it("flags emails in proposed_data as unverified and not sendable", async () => {
    mockListFetch([
      proposal({
        id: "p1",
        proposed_data: { contact_email: "lead@acme.example" },
      }),
    ]);
    renderList(<LeadProposalReviewList />);
    await waitFor(() =>
      expect(
        screen.getAllByText("lead@acme.example").length
      ).toBeGreaterThan(0)
    );
    expect(
      screen.getByText("Unverified — not sendable")
    ).toBeInTheDocument();
    expect(screen.getByText("Send basis unverified")).toBeInTheDocument();
  });

  it("renders embedded evidence with source link and review status", async () => {
    mockListFetch([
      proposal({
        id: "p1",
        evidence: [
          {
            id: "e1",
            company_id: null,
            contact_id: null,
            lead_proposal_id: "p1",
            field_path: "industry",
            source_url: "https://acme.example/about",
            observed_at: "2026-10-08T00:00:00Z",
            excerpt: null,
            value: "manufacturing",
            evidence_type: "observed",
            confidence: 0.9,
            review_status: "pending",
            created_by: "agent-1",
            reviewed_by: null,
            created_at: "2026-10-08T00:00:00Z",
          },
        ],
      }),
    ]);
    renderList(<LeadProposalReviewList />);
    await waitFor(() =>
      expect(screen.getByText("industry")).toBeInTheDocument()
    );
    const source = screen.getByRole("link", {
      name: "https://acme.example/about",
    });
    expect(source).toHaveAttribute("href", "https://acme.example/about");
    expect(source).toHaveAttribute("rel", expect.stringContaining("noopener"));
    // 후보 상태(pending)와 증빙 검토 상태(pending)가 모두 표시된다.
    expect(screen.getAllByText("Pending").length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText("Model confidence 90%")).toBeInTheDocument();
  });
});
