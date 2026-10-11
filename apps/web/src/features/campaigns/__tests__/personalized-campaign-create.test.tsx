import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { afterEach, describe, expect, it, vi } from "vitest";

import campaigns from "../../../../messages/en/campaigns.json";
import common from "../../../../messages/en/common.json";
import { PersonalizedCampaignCreate } from "../personalized-campaign-create";

const push = vi.fn();
vi.mock("next/navigation", () => ({ useRouter: () => ({ push, replace: vi.fn() }) }));

function json(data: unknown, init: { status?: number; page?: unknown } = {}) {
  return new Response(JSON.stringify({ data, ...(init.page ? { page: init.page } : {}) }), {
    status: init.status ?? 200,
    headers: { "content-type": "application/json" },
  });
}

function mount(fetcher: typeof fetch) {
  vi.stubGlobal("fetch", fetcher);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={queryClient}>
      <NextIntlClientProvider locale="en" messages={{ campaigns, common }}>
        <PersonalizedCampaignCreate />
      </NextIntlClientProvider>
    </QueryClientProvider>
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  push.mockClear();
});

describe("new sales campaign form", () => {
  it("opens the created campaign from the flat summary response", async () => {
    const calls: { url: string; method?: string }[] = [];
    mount(
      vi.fn<typeof fetch>(async (input, init) => {
        const url = String(input);
        calls.push({ url, method: init?.method });
        if (url.startsWith("/api/v1/sales-campaigns/sender")) {
          return json({
            sender: {
              ready: true,
              profile_name: "Zoho SMTP",
              from_name: "Acme",
              from_address: "sales@acme.test",
            },
          });
        }
        if (url === "/api/v1/sales-campaigns" && init?.method === "POST") {
          return json({ id: "campaign-123", name: "Q4", version: 1 }, { status: 201 });
        }
        return json([], { page: { next_cursor: null, has_more: false } });
      })
    );

    fireEvent.change(await screen.findByLabelText("Campaign name"), {
      target: { value: "Q4 outreach" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /Create campaign/ })).toBeEnabled()
    );
    fireEvent.click(screen.getByRole("button", { name: /Create campaign/ }));

    await waitFor(() => expect(push).toHaveBeenCalledWith("/app/campaigns/campaign-123"));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("explains an empty meeting-type list and links to where to create one", async () => {
    mount(
      vi.fn<typeof fetch>(async (input) => {
        const url = String(input);
        if (url.startsWith("/api/v1/sales-campaigns/sender")) return json({ sender: null });
        return json([], { page: { next_cursor: null, has_more: false } });
      })
    );
    const link = await screen.findByRole("link", { name: "Create a meeting type" });
    expect(link).toHaveAttribute("href", "/app/meeting-types");
  });
});
