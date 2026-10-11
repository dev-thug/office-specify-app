import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { afterEach, describe, expect, it, vi } from "vitest";

import common from "../../../../messages/en/common.json";
import operations from "../../../../messages/en/operations.json";
import settings from "../../../../messages/en/settings.json";
import { MailAccountsScreen } from "../mail-accounts-screen";

vi.mock("@/features/settings/use-me", () => ({ useIsAdmin: () => true }));

type Call = { url: string; init: RequestInit };

const unconfigured = {
  configured: false,
  source: "none",
  client_id: null,
  has_secret: false,
  redirect_uri: "https://office.example/api/v1/integrations/zoho/callback",
  default_redirect_uri: "https://office.example/api/v1/integrations/zoho/callback",
  missing: ["client_id", "client_secret"],
  updated_at: null,
};

function json(data: unknown, status = 200) {
  return new Response(JSON.stringify({ data }), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function mount(fetcher: typeof fetch) {
  vi.stubGlobal("fetch", fetcher);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
  );
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={queryClient}>
      <NextIntlClientProvider locale="en" messages={{ settings, common, operations }}>
        <MailAccountsScreen />
      </NextIntlClientProvider>
    </QueryClientProvider>
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("Zoho OAuth client entry in the connect dialog", () => {
  it("asks for the client values when unconfigured, validates, saves, then starts OAuth", async () => {
    const calls: Call[] = [];
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      const request = init ?? {};
      calls.push({ url, init: request });
      if (url === "/api/v1/mail-accounts/oauth-settings" && request.method === "PUT") {
        return json({
          ...unconfigured,
          configured: true,
          source: "workspace",
          client_id: "1000.ABCDEF",
          has_secret: true,
          missing: [],
        });
      }
      if (url === "/api/v1/mail-accounts/oauth-settings") return json(unconfigured);
      if (url === "/api/v1/mail-accounts/connection-requests") {
        return json({
          connection_id: "c1",
          authorization_url: "https://accounts.zoho.com/oauth/v2/auth?x=1",
          state_expires_at: "2026-10-11T00:10:00Z",
        });
      }
      return json([]);
    });
    mount(fetcher);

    fireEvent.click(await screen.findByRole("button", { name: "Connect mailbox" }));
    const clientId = await screen.findByLabelText("Client ID");
    const secret = screen.getByLabelText("Client secret");
    // The callback URI to register in the Zoho API Console is shown.
    expect(
      await screen.findAllByText(
        "https://office.example/api/v1/integrations/zoho/callback"
      )
    ).not.toHaveLength(0);

    // Empty submit is rejected client-side without any network write.
    fireEvent.click(screen.getByRole("button", { name: "Save and start OAuth" }));
    expect((await screen.findAllByText("This field is required")).length).toBe(2);
    expect(
      calls.some((call) => call.init.method === "PUT" || call.init.method === "POST")
    ).toBe(false);

    fireEvent.change(clientId, { target: { value: "1000.ABCDEF" } });
    fireEvent.change(secret, { target: { value: "a-secret-value" } });
    fireEvent.click(screen.getByRole("button", { name: "Save and start OAuth" }));

    await waitFor(() =>
      expect(
        calls.some((call) => call.url === "/api/v1/mail-accounts/connection-requests")
      ).toBe(true)
    );
    const put = calls.find((call) => call.init.method === "PUT")!;
    expect(JSON.parse(String(put.init.body))).toEqual({
      client_id: "1000.ABCDEF",
      client_secret: "a-secret-value",
    });
    // Saving happens before the OAuth start.
    expect(calls.indexOf(put)).toBeLessThan(
      calls.findIndex((call) => call.url === "/api/v1/mail-accounts/connection-requests")
    );
  });

  it("skips the client fields when the workspace is already configured", async () => {
    const fetcher = vi.fn<typeof fetch>(async (input) => {
      const url = String(input);
      if (url === "/api/v1/mail-accounts/oauth-settings") {
        return json({
          ...unconfigured,
          configured: true,
          source: "workspace",
          client_id: "1000.SAVED",
          has_secret: true,
          missing: [],
        });
      }
      return json([]);
    });
    mount(fetcher);

    fireEvent.click(await screen.findByRole("button", { name: "Connect mailbox" }));
    expect(
      await screen.findByText("Using the saved Zoho OAuth client.")
    ).toBeInTheDocument();
    expect(screen.queryByLabelText("Client secret")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start OAuth" })).toBeInTheDocument();
  });
});
