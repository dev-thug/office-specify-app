import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { afterEach, describe, expect, it, vi } from "vitest";

import common from "../../../../messages/en/common.json";
import settings from "../../../../messages/en/settings.json";
import { AgentsSection } from "../agents-section";

vi.mock("@/features/settings/use-me", () => ({ useIsAdmin: () => true }));

function json(data: unknown, init: { status?: number; page?: unknown } = {}) {
  return new Response(JSON.stringify({ data, ...(init.page ? { page: init.page } : {}) }), {
    status: init.status ?? 200,
    headers: { "content-type": "application/json" },
  });
}

afterEach(() => vi.unstubAllGlobals());

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

describe("MCP credential issuance", () => {
  it("sends expires_in_days and a name, and shows the MCP URL and client config once", async () => {
    const posts: { url: string; body: Record<string, unknown> }[] = [];
    vi.stubGlobal("ResizeObserver", ResizeObserverStub);
    vi.stubGlobal(
      "fetch",
      vi.fn<typeof fetch>(async (input, init) => {
        const url = String(input);
        if (init?.method === "POST" && url.endsWith("/credentials")) {
          posts.push({ url, body: JSON.parse(String(init.body)) });
          return json(
            {
              id: "cred-1",
              principal_id: "agent-1",
              name: "Research agent MCP",
              key_prefix: "ok_live",
              scopes: ["crm.read"],
              secret: "ok_live_SECRETVALUE",
              expires_at: "2027-10-11T00:00:00Z",
            },
            { status: 201 }
          );
        }
        if (url.includes("/principals")) {
          return json(
            [{ id: "agent-1", kind: "agent", name: "Research agent", status: "active", role: "agent" }],
            { page: { next_cursor: null, has_more: false } }
          );
        }
        return json([], { page: { next_cursor: null, has_more: false } });
      })
    );
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={queryClient}>
        <NextIntlClientProvider locale="en" messages={{ settings, common }}>
          <AgentsSection />
        </NextIntlClientProvider>
      </QueryClientProvider>
    );

    fireEvent.click(await screen.findByRole("button", { name: "Issue credential" }));
    fireEvent.click(await screen.findByRole("button", { name: "Company research" }));
    fireEvent.click(screen.getByRole("button", { name: "Issue" }));

    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0]!.body).toMatchObject({
      name: "Research agent MCP",
      expires_in_days: 365,
    });
    expect(posts[0]!.body).not.toHaveProperty("expires_at");
    expect(posts[0]!.body.scopes).toEqual(["crm.read", "research.propose", "research.write"]);
    expect(await screen.findByDisplayValue(/Bearer ok_live_SECRETVALUE/)).toBeInTheDocument();
    expect(screen.getByDisplayValue(/:8081\/mcp$/)).toBeInTheDocument();
  });
});
