import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { afterEach, describe, expect, it, vi } from "vitest";

import common from "../../../../messages/en/common.json";
import settings from "../../../../messages/en/settings.json";
import type { MailAccount } from "@/lib/api/settings";
import { GrantsSheet } from "../grants-sheet";

vi.mock("@/features/settings/use-me", () => ({
  useIsAdmin: () => true,
}));

const account: MailAccount = {
  id: "mail-1",
  connection_id: "connection-1",
  provider: "zoho_mail",
  provider_account_id: "provider-account-1",
  primary_address: "team@example.test",
  name: "Support mailbox",
  status: "active",
  last_synced_at: null,
  version: 1,
  created_at: "2026-10-10T00:00:00Z",
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("agent mailbox grants", () => {
  it("labels can_send as human-approved only and keeps approval disabled", async () => {
    const grant = {
      mail_account_id: account.id,
      principal_id: "agent-1",
      principal_name: "Operations agent",
      can_read: true,
      can_compose: true,
      can_send: false,
      can_approve: false,
      can_assign: false,
      version: 1,
    };
    const principal = {
      id: "agent-1",
      kind: "agent",
      user_id: null,
      name: "Operations agent",
      role: "agent",
      status: "active",
      permission_version: 1,
      version: 1,
      created_at: "2026-10-10T00:00:00Z",
    };
    const fetcher = vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), "http://office.test").pathname;
      const data = path.endsWith("/grants")
        ? [grant]
        : path === "/api/v1/principals"
          ? [principal]
          : [];
      return new Response(JSON.stringify({
        data,
        page: { next_cursor: null, has_more: false },
      }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    });
    vi.stubGlobal("fetch", fetcher);

    render(
      <NextIntlClientProvider locale="en" messages={{ settings, common }}>
        <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
          <GrantsSheet account={account} open onOpenChange={() => undefined} />
        </QueryClientProvider>
      </NextIntlClientProvider>
    );

    const approvedSend = await screen.findByRole("switch", {
      name: "Human-approved send only",
    });
    const approve = screen.getByRole("switch", { name: "Approve" });
    expect(approvedSend).toBeEnabled();
    expect(approve).toBeDisabled();

    fireEvent.click(approvedSend);
    expect(approvedSend).toBeChecked();
    expect(approve).toBeDisabled();
  });
});
