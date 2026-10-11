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
    expect(posts[0]!.body.scopes).toEqual(["crm.read", "crm.write", "research.write"]);
    expect(await screen.findByDisplayValue(/Bearer ok_live_SECRETVALUE/)).toBeInTheDocument();
    expect(screen.getByDisplayValue(/:8081\/mcp$/)).toBeInTheDocument();
  });

  it("selects every scope and can issue a never-expiring credential", async () => {
    const posts: Record<string, unknown>[] = [];
    vi.stubGlobal("ResizeObserver", ResizeObserverStub);
    vi.stubGlobal(
      "fetch",
      vi.fn<typeof fetch>(async (input, init) => {
        const url = String(input);
        if (init?.method === "POST" && url.endsWith("/credentials")) {
          posts.push(JSON.parse(String(init.body)));
          return json({ id: "c", principal_id: "agent-1", name: "x", key_prefix: "p", scopes: [], secret: "tok", expires_at: "2126-10-11T00:00:00Z" }, { status: 201 });
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
    fireEvent.click(await screen.findByRole("button", { name: "Select all" }));
    fireEvent.change(screen.getByLabelText("Expires at"), { target: { value: "0" } });
    fireEvent.click(screen.getByRole("button", { name: "Issue" }));

    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0]).toMatchObject({ never_expires: true });
    expect(posts[0]).not.toHaveProperty("expires_in_days");
    expect((posts[0]!.scopes as string[]).length).toBeGreaterThanOrEqual(20);
  });

  it("lists each agent's credentials from GET /credentials (no per-agent list endpoint)", async () => {
    vi.stubGlobal("ResizeObserver", ResizeObserverStub);
    const urls: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn<typeof fetch>(async (input) => {
        const url = String(input);
        urls.push(url);
        if (url.startsWith("/api/v1/credentials")) {
          return json([
            { id: "c1", principal_id: "agent-1", name: "mine", key_prefix: "osk_a", scopes: ["crm.read"], expires_at: "2027-01-01T00:00:00Z", last_used_at: null, revoked_at: null, created_at: "2026-10-11T00:00:00Z" },
            { id: "c2", principal_id: "agent-2", name: "someone-else", key_prefix: "osk_b", scopes: ["crm.read"], expires_at: "2027-01-01T00:00:00Z", last_used_at: null, revoked_at: null, created_at: "2026-10-11T00:00:00Z" },
          ]);
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

    expect(await screen.findByText("mine")).toBeInTheDocument();
    expect(screen.queryByText("someone-else")).toBeNull();
    expect(urls.some((url) => /\/agents\/[^/]+\/credentials/.test(url))).toBe(false);
  });

  it("creating an agent also issues its first token with the same scopes (scopes chosen once)", async () => {
    vi.stubGlobal("ResizeObserver", ResizeObserverStub);
    const posts: { url: string; body: Record<string, unknown> }[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn<typeof fetch>(async (input, init) => {
        const url = String(input);
        if (init?.method === "POST") {
          posts.push({ url, body: JSON.parse(String(init.body)) });
          if (url.endsWith("/agents")) {
            return json({ id: "agent-9", kind: "agent", name: "Sales bot", status: "active", role: "agent" }, { status: 201 });
          }
          return json(
            { id: "c9", principal_id: "agent-9", name: "Sales bot MCP", key_prefix: "osk", scopes: [], secret: "osk_NEWTOKEN", expires_at: "2027-10-11T00:00:00Z" },
            { status: 201 }
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

    fireEvent.click(await screen.findByRole("button", { name: "Add agent" }));
    fireEvent.change(await screen.findByLabelText("Name"), { target: { value: "Sales bot" } });
    fireEvent.click(screen.getByRole("button", { name: "Sales campaigns" }));
    fireEvent.click(screen.getByRole("button", { name: "Create agent" }));

    await waitFor(() => expect(posts).toHaveLength(2));
    expect(posts[0]!.url.endsWith("/agents")).toBe(true);
    expect(posts[1]!.url.endsWith("/agents/agent-9/credentials")).toBe(true);
    expect(posts[1]!.body.scopes).toEqual(posts[0]!.body.scopes);
    expect(await screen.findByDisplayValue("osk_NEWTOKEN")).toBeInTheDocument();
  });
});
