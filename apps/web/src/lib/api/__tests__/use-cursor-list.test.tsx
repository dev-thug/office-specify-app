import type { ReactNode } from "react";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useCursorList } from "@/lib/api/use-cursor-list";

interface Row {
  id: string;
}

function mockFetchPages() {
  const urls: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL) => {
      const u = String(url);
      urls.push(u);
      const cursor = new URL(u, "http://x").searchParams.get("cursor");
      const body =
        cursor === null
          ? {
              data: [{ id: "a" }],
              page: { next_cursor: "c2", has_more: true },
            }
          : {
              data: [{ id: "b" }],
              page: { next_cursor: null, has_more: false },
            };
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    })
  );
  return urls;
}

function makeWrapper() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return function Wrapper({ children }: { children: ReactNode }) {
    return (
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    );
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("useCursorList", () => {
  it("fetches page 1 with default limit=50 and accumulates cursor pages", async () => {
    const urls = mockFetchPages();
    const { result } = renderHook(
      () =>
        useCursorList<Row>({ queryKey: ["rows"], path: "/api/v1/things" }),
      { wrapper: makeWrapper() }
    );

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(urls).toHaveLength(1);
    expect(urls[0]).toContain("limit=50");
    expect(urls[0]).not.toContain("cursor=");
    expect(result.current.hasNextPage).toBe(true);

    await result.current.fetchNextPage();
    await waitFor(() =>
      expect(result.current.data?.pages).toHaveLength(2)
    );
    expect(urls[1]).toContain("cursor=c2");
    expect(result.current.hasNextPage).toBe(false);
    expect(
      result.current.data?.pages.flatMap((p) => p.data).map((r) => r.id)
    ).toEqual(["a", "b"]);
  });

  it("passes fixed params and a custom limit", async () => {
    const urls = mockFetchPages();
    const { result } = renderHook(
      () =>
        useCursorList<Row>({
          queryKey: ["rows", "filtered"],
          path: "/api/v1/things",
          params: { stage: "engaged", q: "acme", skip: undefined },
          limit: 5,
        }),
      { wrapper: makeWrapper() }
    );

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const u = new URL(urls[0]!, "http://x");
    expect(u.searchParams.get("stage")).toBe("engaged");
    expect(u.searchParams.get("q")).toBe("acme");
    expect(u.searchParams.get("limit")).toBe("5");
    expect(u.searchParams.has("skip")).toBe(false);
  });
});
