import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import type { ReactElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SuppressionsScreen } from "@/features/compliance";
import enCommon from "../../../../messages/en/common.json";
import enCompliance from "../../../../messages/en/compliance.json";
import enStatus from "../../../../messages/en/status.json";

const messages = {
  compliance: enCompliance,
  common: enCommon,
  status: enStatus,
};

const ROWS = [
  {
    id: "s1",
    email_address_id: "ea1",
    email_address: "active@example.com",
    purpose_scope: "marketing",
    reason: "unsubscribe",
    source_outbound_id: null,
    created_by: null,
    released_at: null,
    released_by: null,
    release_evidence_ref: null,
    version: 1,
    created_at: "2026-10-01T00:00:00Z",
  },
  {
    id: "s2",
    email_address_id: "ea2",
    email_address: "released@example.com",
    purpose_scope: "all",
    reason: "complaint",
    source_outbound_id: null,
    created_by: null,
    released_at: "2026-10-03T00:00:00Z",
    released_by: "p1",
    release_evidence_ref: "ev-9",
    version: 2,
    created_at: "2026-09-20T00:00:00Z",
  },
];

function mockFetch(body: unknown = {
  data: ROWS,
  page: { next_cursor: null, has_more: false },
}) {
  const calls: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL) => {
      calls.push(String(url));
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    })
  );
  return calls;
}

function R(ui: ReactElement) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <NextIntlClientProvider locale="en" messages={messages}>
        {ui}
      </NextIntlClientProvider>
    </QueryClientProvider>
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("SuppressionsScreen", () => {
  it("requests /api/v1/suppressions and renders address rows", async () => {
    const calls = mockFetch();
    R(<SuppressionsScreen />);
    await waitFor(() =>
      expect(screen.getByText("active@example.com")).toBeInTheDocument()
    );
    expect(screen.getByText("released@example.com")).toBeInTheDocument();
    expect(calls[0]).toContain("/api/v1/suppressions?");
    expect(calls[0]).toContain("limit=50");
    // 기본 상태 필터는 "차단 중".
    expect(calls[0]).toContain("released=false");
  });

  it("offers release only on active rows — never on released rows", async () => {
    mockFetch();
    R(<SuppressionsScreen />);
    await waitFor(() =>
      expect(screen.getByText("active@example.com")).toBeInTheDocument()
    );
    const releasedRow = screen
      .getByText("released@example.com")
      .closest("tr")!;
    const activeRow = screen.getByText("active@example.com").closest("tr")!;
    expect(
      activeRow.querySelector("button")?.textContent
    ).toBe("Request release");
    expect(releasedRow.querySelector("button")).toBeNull();
  });

  it("shows both blocked and released states as badges, not bare color", async () => {
    mockFetch();
    R(<SuppressionsScreen />);
    await waitFor(() =>
      expect(screen.getByText("active@example.com")).toBeInTheDocument()
    );
    expect(screen.getAllByText("Blocked").length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText("Released")).toBeInTheDocument();
  });

  it("shows the empty state with guidance", async () => {
    mockFetch({ data: [], page: { next_cursor: null, has_more: false } });
    R(<SuppressionsScreen />);
    await waitFor(() =>
      expect(
        screen.getByText("No suppressed addresses")
      ).toBeInTheDocument()
    );
  });
});
