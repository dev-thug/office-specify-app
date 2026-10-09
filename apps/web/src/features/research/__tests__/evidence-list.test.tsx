import { render, screen } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import type { ReactElement, ReactNode } from "react";
import { describe, expect, it } from "vitest";

import {
  EvidenceItem,
  isSafeEvidenceUrl,
} from "@/features/research/evidence-list";
import type { ResearchEvidence } from "@/lib/api/research";
import enResearch from "../../../../messages/en/research.json";
import enStatus from "../../../../messages/en/status.json";

const messages = { research: enResearch, status: enStatus };

function renderItem(evidence: Partial<ResearchEvidence>, node?: ReactElement) {
  const wrap = ({ children }: { children: ReactNode }) => (
    <NextIntlClientProvider locale="en" messages={messages}>
      {children}
    </NextIntlClientProvider>
  );
  return render(
    node ?? (
      <EvidenceItem
        evidence={{
          id: "e1",
          company_id: "c1",
          contact_id: null,
          lead_proposal_id: null,
          field_path: "industry",
          source_url: "https://example.com/about",
          observed_at: "2026-10-08T00:00:00Z",
          excerpt: null,
          value: "manufacturing",
          evidence_type: "observed",
          confidence: null,
          review_status: "pending",
          created_by: "agent-1",
          reviewed_by: null,
          created_at: "2026-10-08T00:00:00Z",
          ...evidence,
        }}
      />
    ),
    { wrapper: wrap }
  );
}

describe("isSafeEvidenceUrl", () => {
  it("allows http/https only", () => {
    expect(isSafeEvidenceUrl("https://a.example/x")).toBe(true);
    expect(isSafeEvidenceUrl("http://a.example")).toBe(true);
    expect(isSafeEvidenceUrl("javascript:alert(1)")).toBe(false);
    expect(isSafeEvidenceUrl("data:text/html,<b>x</b>")).toBe(false);
    expect(isSafeEvidenceUrl("file:///etc/passwd")).toBe(false);
    expect(isSafeEvidenceUrl("not a url")).toBe(false);
    expect(isSafeEvidenceUrl("//evil.example/x")).toBe(false);
  });
});

describe("EvidenceItem source_url rendering", () => {
  it("renders a javascript: URL as plain text, never as a link", () => {
    renderItem({ source_url: "javascript:alert(1)" });
    expect(screen.queryByRole("link")).toBeNull();
    // 원문 문자열은 감사용으로 그대로 보여준다.
    expect(screen.getByText("javascript:alert(1)")).toBeInTheDocument();
  });

  it("renders https URLs as external links with noopener", () => {
    renderItem({ source_url: "https://example.com/source" });
    const link = screen.getByRole("link", {
      name: "https://example.com/source",
    });
    expect(link).toHaveAttribute("href", "https://example.com/source");
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", expect.stringContaining("noopener"));
  });

  it("shows the reviewer when reviewed_by is set (§7.4)", () => {
    renderItem({ reviewed_by: "principal-42", review_status: "verified" });
    expect(screen.getByText("Reviewed by:")).toBeInTheDocument();
    expect(screen.getByText("principal-42")).toBeInTheDocument();
  });

  it("omits the reviewer when not yet reviewed", () => {
    renderItem({ reviewed_by: null });
    expect(screen.queryByText("Reviewed by:")).toBeNull();
  });
});
