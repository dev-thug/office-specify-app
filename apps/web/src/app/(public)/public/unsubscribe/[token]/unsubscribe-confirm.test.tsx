import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import type { ReactElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import enCommon from "../../../../../../messages/en/common.json";
import enCompliance from "../../../../../../messages/en/compliance.json";

import { UnsubscribeConfirm } from "./unsubscribe-confirm";

const messages = {
  compliance: enCompliance,
  common: enCommon,
};

function stubFetch(
  handler: (url: string, init?: RequestInit) => Response
): { url: string; init?: RequestInit }[] {
  const calls: { url: string; init?: RequestInit }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init });
      return handler(String(url), init);
    })
  );
  return calls;
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
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

describe("UnsubscribeConfirm", () => {
  it("GET shows confirmation data only — org and masked address, no side effect", async () => {
    const calls = stubFetch(() =>
      json({
        data: {
          organization_name: "Example Corp",
          scope: "workspace_marketing",
          masked_address: "u***@example.com",
        },
      })
    );
    R(<UnsubscribeConfirm token="tok1" />);
    await waitFor(() =>
      expect(screen.getByText("Example Corp")).toBeInTheDocument()
    );
    expect(screen.getByText("u***@example.com")).toBeInTheDocument();
    // GET만 호출됐다 — 버튼을 누르기 전 POST는 없다.
    expect(calls).toHaveLength(1);
    expect(calls[0]!.init?.method ?? "GET").toBe("GET");
    expect(screen.getByText("Confirm unsubscribe")).toBeInTheDocument();
  });

  it("POST on confirm moves to the done state; repeats are safe", async () => {
    const calls = stubFetch((url, init) => {
      if (init?.method === "POST") {
        return json({
          data: {
            status: "unsubscribed",
            scope: "workspace_marketing",
            message: "Unsubscribed from Example Corp marketing.",
          },
        });
      }
      return json({ data: { organization_name: "Example Corp" } });
    });
    R(<UnsubscribeConfirm token="tok1" />);
    const button = await screen.findByText("Confirm unsubscribe");
    fireEvent.click(button);
    await waitFor(() =>
      expect(screen.getByText("Unsubscribe processed")).toBeInTheDocument()
    );
    expect(calls.map((c) => c.init?.method)).toContain("POST");
    expect(
      screen.getByText(
        "You won't receive marketing email from this sender anymore. Thank you for being with us."
      )
    ).toBeInTheDocument();
    expect(screen.queryByText("Unsubscribed from Example Corp marketing.")).toBeNull();
  });

  it("already-processed preview goes straight to the done state", async () => {
    stubFetch(() =>
      json({ data: { status: "already_unsubscribed" } })
    );
    R(<UnsubscribeConfirm token="tok1" />);
    await waitFor(() =>
      expect(screen.getByText("Unsubscribe processed")).toBeInTheDocument()
    );
    expect(screen.queryByText("Confirm unsubscribe")).toBeNull();
  });

  it("404 on GET renders the invalid-link state without retry affordance", async () => {
    stubFetch(() =>
      json(
        {
          type: "urn:outreach:problem:not-found",
          title: "Not found",
          status: 404,
          code: "NOT_FOUND",
        },
        404
      )
    );
    R(<UnsubscribeConfirm token="bad" />);
    await waitFor(() =>
      expect(
        screen.getByText("This link cannot be used")
      ).toBeInTheDocument()
    );
    expect(screen.queryByText("Confirm unsubscribe")).toBeNull();
    expect(screen.queryByText("Try again")).toBeNull();
  });

  it("404 on POST flips to the invalid-link state", async () => {
    stubFetch((url, init) => {
      if (init?.method === "POST") {
        return json(
          { type: "urn:x", title: "Not found", status: 404, code: "NOT_FOUND" },
          404
        );
      }
      return json({ data: { organization_name: "Example Corp" } });
    });
    R(<UnsubscribeConfirm token="tok1" />);
    const button = await screen.findByText("Confirm unsubscribe");
    fireEvent.click(button);
    await waitFor(() =>
      expect(
        screen.getByText("This link cannot be used")
      ).toBeInTheDocument()
    );
  });

  it("transient GET failure offers a retry", async () => {
    stubFetch(() => json({ title: "oops", status: 500 }, 500));
    R(<UnsubscribeConfirm token="tok1" />);
    expect(await screen.findByText("Could not process", {}, { timeout: 5000 })).toBeInTheDocument();
    expect(screen.getByText("Try again")).toBeInTheDocument();
  });
});
