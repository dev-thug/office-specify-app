import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { NextIntlClientProvider } from "next-intl";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import common from "../../../../messages/en/common.json";
import templates from "../../../../messages/en/templates.json";
import { ZohoTemplateLibrary } from "../zoho-template-library";

const userState = vi.hoisted(() => ({
  current: { kind: "human", role: "admin", principal_id: "admin-1", workspace_id: "workspace-1" },
}));
vi.mock("@/features/settings/use-me", () => ({ useMe: () => ({ data: userState.current }) }));

const setup = {
  data: {
    email_api_configured: true,
    workspace_id: "workspace-1",
  },
};
const listed = [{ id: "remote-1", name: "Welcome" }];
const detail = {
  id: "remote-1",
  name: "Welcome",
  content: "<p>Hello</p>",
  content_type: "html" as const,
  content_hash: "hash-1",
};

function json(data: unknown, options: { status?: number; headers?: Record<string, string> } = {}) {
  return new Response(JSON.stringify(data), {
    status: options.status ?? 200,
    headers: { "content-type": "application/json", ...options.headers },
  });
}

function harness(fetcher: typeof fetch) {
  vi.stubGlobal("fetch", fetcher);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  const imported = vi.fn();
  const view = render(
    <QueryClientProvider client={client}>
      <NextIntlClientProvider locale="en" messages={{ common, templates }}>
        <ZohoTemplateLibrary
          open
          onOpenChange={vi.fn()}
          onImportSeed={imported}
        />
      </NextIntlClientProvider>
    </QueryClientProvider>
  );
  return { ...view, imported, client };
}

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
});
afterEach(() => {
  userState.current = { kind: "human", role: "admin", principal_id: "admin-1", workspace_id: "workspace-1" };
  vi.unstubAllGlobals();
});

describe("Zoho Email API template library", () => {
  it("explains the separate prepaid product and makes no template call when it is not configured", async () => {
    const fetcher = vi.fn<typeof fetch>(async (input) => {
      expect(String(input)).toContain("/api/v1/integrations/zoho-campaigns");
      return json({
        data: {
          ...setup.data,
          email_api_configured: false,
          configured: true,
        },
      });
    });

    harness(fetcher);

    expect(await screen.findByText(/separate prepaid Zoho Email API/i)).toBeInTheDocument();
    expect(screen.getByText(/ZOHO_EMAIL_API_ENABLED/)).toBeInTheDocument();
    expect(fetcher.mock.calls.every(([input]) => !String(input).includes("/zoho-email-api/templates"))).toBe(true);
  });

  it("makes no setup or template call for a non-admin", () => {
    userState.current.role = "member";
    const fetcher = vi.fn<typeof fetch>();

    harness(fetcher);

    expect(fetcher).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("keeps template query caches partitioned by principal and the verified workspace", async () => {
    const fetcher = vi.fn<typeof fetch>(async (input) => {
      const url = String(input);
      if (url.includes("/api/v1/integrations/zoho-campaigns")) return json({ data: setup.data });
      if (url.includes("/api/v1/zoho-email-api/templates/remote-1")) return json({ data: detail });
      if (url.includes("/api/v1/zoho-email-api/templates")) return json({ data: listed, page: { next_cursor: null, has_more: false } });
      throw new Error(`Unexpected request ${url}`);
    });
    const { client } = harness(fetcher);

    await screen.findByTitle("Zoho template preview");

    expect(client.getQueryCache().findAll({ queryKey: ["zoho-email-api-templates", "admin-1", "workspace-1", "workspace-1", "list"] })).toHaveLength(1);
    expect(client.getQueryCache().findAll({ queryKey: ["zoho-email-api-templates", "admin-1", "workspace-1", "workspace-1", "detail", "remote-1"] })).toHaveLength(1);
  });

  it("does not load the library when the setup workspace differs from the signed-in workspace", async () => {
    const fetcher = vi.fn<typeof fetch>(async (input) => {
      if (String(input).includes("/api/v1/integrations/zoho-campaigns")) return json({ data: { ...setup.data, workspace_id: "other-workspace" } });
      throw new Error(`Unexpected request ${String(input)}`);
    });

    harness(fetcher);

    expect(await screen.findByText(/workspace does not match/i)).toBeInTheDocument();
    expect(fetcher.mock.calls.every(([input]) => !String(input).includes("/zoho-email-api/templates"))).toBe(true);
  });

  it("loads remote summaries and details through Office, then previews remote HTML in a sandbox", async () => {
    const nativeSource = '<style>.layout { color: teal; } .hero { background-image: url(https://images.example.test/hero.png); }</style><meta http-equiv="refresh" content="0;url=https://evil.example.test"><table class="layout"><tr><td style="padding:12px">Hi</td></tr></table><img src="https://track.example.test/pixel"><script>run()</script>';
    const fetcher = vi.fn<typeof fetch>(async (input) => {
      const url = String(input);
      if (url.includes("/api/v1/integrations/zoho-campaigns")) return json({ data: setup.data });
      if (url.includes("/api/v1/zoho-email-api/templates/remote-1")) {
        return json({ data: { ...detail, content: nativeSource } }, { headers: { etag: '"hash-1"' } });
      }
      if (url.includes("/api/v1/zoho-email-api/templates")) return json({ data: listed, page: { next_cursor: null, has_more: false } });
      throw new Error(`Unexpected request ${url}`);
    });

    harness(fetcher);

    expect(await screen.findByRole("button", { name: "Welcome" })).toBeInTheDocument();
    expect(await screen.findByTitle("Zoho template preview")).toHaveAttribute("sandbox", "");
    const preview = screen.getByTitle("Zoho template preview");
    const srcDoc = preview.getAttribute("srcdoc") ?? "";
    expect(srcDoc).toContain("default-src 'none'");
    expect(srcDoc).toContain(".layout { color: teal; }");
    expect(srcDoc).toContain("<table");
    expect(srcDoc).toContain("padding:12px");
    expect(srcDoc).not.toContain("<script");
    expect(srcDoc).not.toContain("url=https://evil.example.test");
    expect(srcDoc).not.toContain("https://images.example.test");
    expect(srcDoc).not.toContain("https://track.example.test");
    expect(srcDoc).not.toContain("https://evil.example.test");
    expect(screen.getByText(/External images and other network resources are blocked/i)).toBeInTheDocument();
    expect(document.body.querySelector("details pre")?.textContent).toBe(nativeSource);
    expect(fetcher.mock.calls.some(([input]) => String(input).includes("/api/v1/zoho-email-api/templates/remote-1"))).toBe(true);
  });

  it("keeps native Zoho source intact when only renaming and requires consent before Compose conversion", async () => {
    const nativeSource = '<!doctype html><html><head><style>.layout { color: teal; }</style></head><body><table class="layout"><tr><td>Native layout</td></tr></table></body></html>';
    let updateBody: unknown;
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      if (url.includes("/api/v1/integrations/zoho-campaigns")) return json({ data: setup.data });
      if (init?.method === "PUT") {
        updateBody = JSON.parse(String(init.body));
        return json({ data: { operation_id: "op-native", template_id: "remote-1", deleted: false } }, { status: 202 });
      }
      if (url.includes("/api/v1/zoho-email-api/templates/remote-1")) return json({ data: { ...detail, content: nativeSource } }, { headers: { etag: '"hash-1"' } });
      if (url.includes("/api/v1/zoho-email-api/templates")) return json({ data: listed, page: { next_cursor: null, has_more: false } });
      throw new Error(`Unexpected request ${url}`);
    });

    harness(fetcher);
    fireEvent.click(await screen.findByRole("button", { name: "Edit remote template" }));

    expect(await screen.findByText("This native template is kept in source mode to preserve its layout")).toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: "Compose" })).not.toBeInTheDocument();
    expect(screen.getByLabelText("Zoho template content")).toHaveValue(nativeSource);
    const nativePreview = screen.getByTitle("Native source preview");
    expect(nativePreview).toHaveAttribute("sandbox", "");
    expect(nativePreview.getAttribute("srcdoc")).toContain("<table");
    expect(nativePreview.getAttribute("srcdoc")).toContain(".layout { color: teal; }");

    fireEvent.change(screen.getByLabelText("Zoho template name"), { target: { value: "Renamed only" } });
    fireEvent.click(screen.getByRole("button", { name: "Save in Zoho" }));
    await waitFor(() => expect(updateBody).toEqual({ name: "Renamed only", content: nativeSource, content_type: "html" }));

    fireEvent.click(await screen.findByRole("button", { name: "Edit remote template" }));
    fireEvent.click(await screen.findByRole("button", { name: "Convert source for visual editing" }));
    expect(screen.getByRole("tab", { name: "Compose" })).toBeInTheDocument();
  });

  it("requires explicit merge-field decisions and warns before importing a compatible copy", async () => {
    const native = {
      ...detail,
      content: '<p>Hello $[Contact Name]$ and $[Legacy Field]$</p><img src="$[Image URL]$">',
    };
    const fetcher = vi.fn<typeof fetch>(async (input) => {
      const url = String(input);
      if (url.includes("/api/v1/integrations/zoho-campaigns")) return json({ data: setup.data });
      if (url.includes("/api/v1/zoho-email-api/templates/remote-1")) return json({ data: native });
      if (url.includes("/api/v1/zoho-email-api/templates")) return json({ data: listed, page: { next_cursor: null, has_more: false } });
      throw new Error(`Unexpected request ${url}`);
    });
    const { imported } = harness(fetcher);

    fireEvent.click(await screen.findByRole("button", { name: "Import as new Office version" }));
    expect(await screen.findByText(/unsupported HTML will be removed/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Continue to Office editor" })).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Office mapping for Contact Name"), { target: { value: "contact.name" } });
    expect(screen.getByRole("button", { name: "Continue to Office editor" })).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Office mapping for Legacy Field"), { target: { value: "remove" } });
    expect(screen.getByRole("button", { name: "Continue to Office editor" })).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Office mapping for Image URL"), { target: { value: "remove" } });
    fireEvent.click(screen.getByRole("button", { name: "Continue to Office editor" }));

    await waitFor(() => expect(imported).toHaveBeenCalledWith(expect.objectContaining({
      subject: "Welcome",
      bodyHtml: "<p>Hello {{contact.name}} and </p>",
      bodyText: "Hello {{contact.name}} and",
    })));
  });

  it("creates a new remote copy, updates only with If-Match, and confirms irreversible deletion", async () => {
    const calls: Array<{ url: string; method: string; headers: Headers; body?: unknown }> = [];
    let created = 0;
    let postAttempts = 0;
    let updateAttempts = 0;
    let deleteAttempts = 0;
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      const headers = new Headers(init?.headers);
      calls.push({ url, method, headers, body: init?.body ? JSON.parse(String(init.body)) : undefined });
      if (url.includes("/api/v1/integrations/zoho-campaigns")) return json({ data: setup.data });
      if (method === "POST") {
        postAttempts += 1;
        if (postAttempts === 1) throw new TypeError("connection reset after submit");
        created += 1;
        return json({ data: { operation_id: `op-${created}`, template_id: `remote-${created + 1}`, deleted: false } }, { status: 202 });
      }
      if (method === "PUT") {
        updateAttempts += 1;
        if (updateAttempts === 1) throw new TypeError("connection reset after submit");
        return json({ data: { operation_id: "op-2", template_id: "remote-1", deleted: false } }, { status: 202 });
      }
      if (method === "DELETE") {
        deleteAttempts += 1;
        if (deleteAttempts === 1) throw new TypeError("connection reset after submit");
        return json({ data: { operation_id: "op-3", template_id: "remote-1", deleted: true } }, { status: 202 });
      }
      if (url.includes("/api/v1/zoho-email-api/templates/remote-2")) return json({ data: { ...detail, id: "remote-2", name: "New copy" } });
      if (url.includes("/api/v1/zoho-email-api/templates/remote-3")) return json({ data: { ...detail, id: "remote-3", name: "Copy of Welcome" } });
      if (url.includes("/api/v1/zoho-email-api/templates/remote-1")) return json({ data: detail }, { headers: { etag: '"hash-1"' } });
      if (url.includes("/api/v1/zoho-email-api/templates")) return json({ data: listed, page: { next_cursor: null, has_more: false } });
      throw new Error(`Unexpected request ${method} ${url}`);
    });

    harness(fetcher);
    fireEvent.click(await screen.findByRole("button", { name: "Create remote template" }));
    fireEvent.change(screen.getByLabelText("Zoho template name"), { target: { value: "New copy" } });
    fireEvent.click(screen.getByRole("tab", { name: "HTML code" }));
    fireEvent.change(screen.getByRole("textbox", { name: "HTML code" }), { target: { value: "<p>Copied</p>" } });
    fireEvent.click(screen.getByRole("button", { name: "Create in Zoho" }));
    expect(await screen.findByText("Could not save this Zoho template.")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Create in Zoho" }));
    await waitFor(() => expect(calls.filter((call) => call.method === "POST")).toHaveLength(2));
    const createAttempts = calls.filter((call) => call.method === "POST");
    const create = createAttempts[1]!;
    expect(create.url).toContain("/api/v1/zoho-email-api/templates");
    expect(create.headers.get("Idempotency-Key")).toBeTruthy();
    expect(create.body).toEqual({ name: "New copy", content: "<p>Copied</p>", content_type: "html" });
    expect(createAttempts[0]!.headers.get("Idempotency-Key")).toBe(create.headers.get("Idempotency-Key"));

    fireEvent.click(await screen.findByRole("button", { name: "Welcome" }));
    fireEvent.click(await screen.findByRole("button", { name: "Copy as new" }));
    expect(screen.getByLabelText("Zoho template name")).toHaveValue("Copy of Welcome");
    fireEvent.click(screen.getByRole("button", { name: "Create in Zoho" }));
    await waitFor(() => expect(calls.filter((call) => call.method === "POST")).toHaveLength(3));
    const copyRequest = calls.filter((call) => call.method === "POST")[2]!;
    expect(copyRequest.body).toEqual({
      name: "Copy of Welcome",
      content: "<p>Hello</p>",
      content_type: "html",
    });
    expect(copyRequest.headers.get("Idempotency-Key")).not.toBe(create.headers.get("Idempotency-Key"));

    fireEvent.click(await screen.findByRole("button", { name: "Welcome" }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit remote template" }));
    fireEvent.change(screen.getByLabelText("Zoho template name"), { target: { value: "Welcome revised" } });
    fireEvent.click(screen.getByRole("button", { name: "Save in Zoho" }));
    expect(await screen.findByText("Could not save this Zoho template.")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Save in Zoho" }));
    await waitFor(() => expect(calls.filter((call) => call.method === "PUT")).toHaveLength(2));
    const updateAttemptsList = calls.filter((call) => call.method === "PUT");
    const update = updateAttemptsList[1]!;
    expect(update.headers.get("If-Match")).toBe('"hash-1"');
    expect(update.headers.get("Idempotency-Key")).toBeTruthy();
    expect(updateAttemptsList[0]!.headers.get("Idempotency-Key")).toBe(update.headers.get("Idempotency-Key"));

    fireEvent.click(await screen.findByRole("button", { name: "Delete from Zoho" }));
    const confirm = await screen.findByRole("alertdialog");
    expect(within(confirm).getByText(/cannot be undone in Zoho/i)).toBeInTheDocument();
    expect(calls.some((call) => call.method === "DELETE")).toBe(false);
    fireEvent.click(within(confirm).getByRole("button", { name: "Confirm delete" }));
    expect(await screen.findByText("Could not delete this Zoho template.")).toBeInTheDocument();
    fireEvent.click(await screen.findByRole("button", { name: "Delete from Zoho" }));
    const retryConfirm = await screen.findByRole("alertdialog");
    fireEvent.click(within(retryConfirm).getByRole("button", { name: "Confirm delete" }));
    await waitFor(() => expect(calls.filter((call) => call.method === "DELETE")).toHaveLength(2));
    const deleteCalls = calls.filter((call) => call.method === "DELETE");
    expect(deleteCalls[1]!.headers.get("If-Match")).toBe('"hash-1"');
    expect(deleteCalls[0]!.headers.get("Idempotency-Key")).toBe(deleteCalls[1]!.headers.get("Idempotency-Key"));
  });

  it("explains an unknown provider result and never resubmits automatically", async () => {
    let posts = 0;
    const postKeys: string[] = [];
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      if (url.includes("/api/v1/integrations/zoho-campaigns")) return json({ data: setup.data });
      if (init?.method === "POST") {
        posts += 1;
        postKeys.push(new Headers(init.headers).get("Idempotency-Key") ?? "");
        return new Response(JSON.stringify({ code: "PROVIDER_SUBMISSION_OUTCOME_UNKNOWN", detail: "uncertain" }), {
          status: 409,
          headers: { "content-type": "application/problem+json" },
        });
      }
      if (url.includes("/api/v1/zoho-email-api/templates")) return json({ data: [], page: { next_cursor: null, has_more: false } });
      throw new Error(`Unexpected request ${url}`);
    });

    harness(fetcher);
    fireEvent.click(await screen.findByRole("button", { name: "Create remote template" }));
    fireEvent.change(screen.getByLabelText("Zoho template name"), { target: { value: "Ambiguous" } });
    fireEvent.click(screen.getByRole("tab", { name: "HTML code" }));
    fireEvent.change(screen.getByRole("textbox", { name: "HTML code" }), { target: { value: "<p>Ambiguous</p>" } });
    fireEvent.click(screen.getByRole("button", { name: "Create in Zoho" }));

    expect(await screen.findByText(/outcome is unknown/i)).toBeInTheDocument();
    expect(posts).toBe(1);
    expect(screen.getByRole("button", { name: "Create remote template" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Refresh library before another attempt" }));
    expect(await screen.findByRole("button", { name: "I confirmed Zoho did not apply this change; start a new attempt" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Create remote template" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "I confirmed Zoho did not apply this change; start a new attempt" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Create remote template" })).toBeEnabled());
    expect(posts).toBe(1);

    fireEvent.click(screen.getByRole("button", { name: "Create remote template" }));
    fireEvent.change(screen.getByLabelText("Zoho template name"), { target: { value: "Ambiguous" } });
    fireEvent.click(screen.getByRole("tab", { name: "HTML code" }));
    fireEvent.change(screen.getByRole("textbox", { name: "HTML code" }), { target: { value: "<p>Ambiguous</p>" } });
    fireEvent.click(screen.getByRole("button", { name: "Create in Zoho" }));
    await waitFor(() => expect(posts).toBe(2));
    expect(postKeys[0]).not.toBe(postKeys[1]);
  });
});
