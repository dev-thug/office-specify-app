import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { afterEach, describe, expect, it, vi } from "vitest";

import common from "../../../../messages/en/common.json";
import settings from "../../../../messages/en/settings.json";
import operations from "../../../../messages/en/operations.json";
import { SmtpSettings } from "../smtp-settings";
import { MailAccountsScreen } from "../mail-accounts-screen";

vi.mock("@/features/settings/use-me", () => ({ useIsAdmin: () => true }));

type Call = { url: string; init: RequestInit };

const mailAccount = {
  id: "mail-account-1",
  connection_id: "connection-1",
  provider: "zoho_mail",
  provider_account_id: "account-1",
  primary_address: "mailbox@acme.test",
  name: "Marketing mailbox",
  status: "active",
  last_synced_at: null,
  version: 1,
  created_at: "2026-10-01T00:00:00Z",
};

const profile = {
  id: "smtp-1",
  name: "Newsletter SMTP",
  mail_account_id: mailAccount.id,
  host: "smtppro.zoho.com",
  port: 587,
  security: "starttls",
  username: "smtp-login@acme.test",
  from_address: "news@updates.acme.test",
  from_name: "Acme News",
  reply_to: null,
  custom_headers: {},
  status: "pending",
  last_error_code: null,
  channel_id: null,
  sender_identity_id: null,
  version: 1,
  has_password: true,
};

function response(data: unknown, status = 200) {
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
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const invalidate = vi.spyOn(queryClient, "invalidateQueries");

  render(
    <QueryClientProvider client={queryClient}>
      <NextIntlClientProvider locale="en" messages={{ settings, common }}>
        <SmtpSettings mailAccounts={[mailAccount]} />
      </NextIntlClientProvider>
    </QueryClientProvider>
  );

  return { queryClient, invalidate };
}

function fillRequiredFields(password = "super-secret-smtp-password") {
  fireEvent.change(screen.getByLabelText("Connection name"), {
    target: { value: "Newsletter SMTP" },
  });
  fireEvent.change(screen.getByLabelText("SMTP host"), {
    target: { value: "smtppro.zoho.com" },
  });
  fireEvent.change(screen.getByLabelText("SMTP login"), {
    target: { value: "smtp-login@acme.test" },
  });
  fireEvent.change(screen.getByLabelText("SMTP password"), {
    target: { value: password },
  });
  fireEvent.change(screen.getByLabelText("From address"), {
    target: { value: "news@updates.acme.test" },
  });
  fireEvent.change(screen.getByLabelText("From name"), {
    target: { value: "Acme News" },
  });
  fireEvent.click(screen.getByRole("combobox", { name: "Mail account" }));
  fireEvent.click(screen.getByRole("option", { name: "Marketing mailbox" }));
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("SMTP settings", () => {
  it("requires sender confirmation and sends SMTP login separately from the From alias", async () => {
    const calls: Call[] = [];
    let savedProfiles: unknown[] = [];
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      const request = init ?? {};
      calls.push({ url, init: request });

      if (url === "/api/v1/smtp-connections" && request.method === "POST") {
        savedProfiles = [profile];
        return response(profile, 201);
      }
      if (url === "/api/v1/smtp-connections") return response(savedProfiles);
      return response([]);
    });

    mount(fetcher);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Add SMTP connection" })).toBeEnabled()
    );
    fireEvent.click(screen.getByRole("button", { name: "Add SMTP connection" }));
    fillRequiredFields();

    fireEvent.click(screen.getByRole("button", { name: "Save SMTP connection" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Confirm that this From address is registered to the selected mail account."
    );
    expect(calls.some((call) => call.init.method === "POST")).toBe(false);

    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "This From address is registered to the selected mail account.",
      })
    );
    fireEvent.click(screen.getByRole("button", { name: "Save SMTP connection" }));

    await waitFor(() => {
      expect(
        calls.some(
          (call) =>
            call.url === "/api/v1/smtp-connections" &&
            call.init.method === "POST"
        )
      ).toBe(true);
    });
    const createCall = calls.find(
      (call) =>
        call.url === "/api/v1/smtp-connections" &&
        call.init.method === "POST"
    );
    const payload = JSON.parse(String(createCall?.init.body));

    expect(payload).toMatchObject({
      name: "Newsletter SMTP",
      mail_account_id: mailAccount.id,
      host: "smtppro.zoho.com",
      port: 587,
      security: "starttls",
      username: "smtp-login@acme.test",
      password: "super-secret-smtp-password",
      from_address: "news@updates.acme.test",
      from_name: "Acme News",
      reply_to: null,
      custom_headers: {},
      sender_confirmed: true,
    });
    expect(payload.username).not.toBe(payload.from_address);
    await waitFor(() =>
      expect(screen.queryByDisplayValue("super-secret-smtp-password")).toBeNull()
    );
    expect(await screen.findByText("Pending verification")).toBeInTheDocument();
  });

  it("uses GET for listing, POST for verify and disable, and refreshes SMTP and delivery-channel queries", async () => {
    const calls: Call[] = [];
    let currentProfile = { ...profile };
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      const request = init ?? {};
      calls.push({ url, init: request });

      if (url === "/api/v1/smtp-connections/smtp-1/verify") {
        currentProfile = { ...currentProfile, status: "active" };
        return response(currentProfile);
      }
      if (url === "/api/v1/smtp-connections/smtp-1/disable") {
        currentProfile = { ...currentProfile, status: "disabled" };
        return response(currentProfile);
      }
      if (url === "/api/v1/smtp-connections") {
        return response([currentProfile]);
      }
      return response([]);
    });

    const { invalidate } = mount(fetcher);

    expect(await screen.findByText("Pending verification")).toBeInTheDocument();
    expect(calls.some((call) => call.url === "/api/v1/smtp-connections")).toBe(true);
    expect(
      calls.find((call) => call.url === "/api/v1/smtp-connections")?.init.method
    ).toBeUndefined();

    fireEvent.click(
      screen.getByRole("button", { name: "Check connection and use" })
    );
    await screen.findByText("Active");
    expect(
      calls.some(
        (call) =>
          call.url === "/api/v1/smtp-connections/smtp-1/verify" &&
          call.init.method === "POST"
      )
    ).toBe(true);
    const verifiedQueryKeys = invalidate.mock.calls.map(
      ([filters]) => filters?.queryKey ?? []
    );
    expect(verifiedQueryKeys).toContainEqual(["settings", "smtp-connections"]);
    expect(verifiedQueryKeys).toContainEqual(["operations", "delivery-channels"]);

    invalidate.mockClear();
    fireEvent.click(screen.getByRole("button", { name: "Disable" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm disable" }));

    await screen.findByText("Disabled");
    expect(
      calls.some(
        (call) =>
          call.url === "/api/v1/smtp-connections/smtp-1/disable" &&
          call.init.method === "POST"
      )
    ).toBe(true);
    const disabledQueryKeys = invalidate.mock.calls.map(
      ([filters]) => filters?.queryKey ?? []
    );
    expect(disabledQueryKeys).toContainEqual(["settings", "smtp-connections"]);
    expect(disabledQueryKeys).toContainEqual(["operations", "delivery-channels"]);
  });

  it("keeps credentials out of status and error text and clears the password when the form closes", async () => {
    const secret = "private-test-password";
    const calls: Call[] = [];
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      const request = init ?? {};
      calls.push({ url, init: request });

      if (url === "/api/v1/smtp-connections" && request.method === "POST") {
        return new Response(JSON.stringify({ detail: secret }), {
          status: 422,
          headers: { "content-type": "application/json" },
        });
      }
      if (url === "/api/v1/smtp-connections") return response([]);
      return response([]);
    });

    mount(fetcher);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Add SMTP connection" })).toBeEnabled()
    );
    fireEvent.click(screen.getByRole("button", { name: "Add SMTP connection" }));
    fillRequiredFields(secret);
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "This From address is registered to the selected mail account.",
      })
    );
    expect(screen.getByLabelText("SMTP password")).toHaveAttribute(
      "type",
      "password"
    );
    fireEvent.click(screen.getByRole("button", { name: "Save SMTP connection" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(
      "The request failed. Review the SMTP settings and try again."
    );
    expect(alert.textContent).not.toContain(secret);
    expect(document.body.textContent).not.toContain(secret);

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Add SMTP connection" })
      ).toBeEnabled()
    );
    fireEvent.click(screen.getByRole("button", { name: "Add SMTP connection" }));

    expect(screen.getByLabelText("SMTP password")).toHaveValue("");
  });
});

describe("mail accounts screen", () => {
  it("keeps SMTP setup visible without a Zoho Campaigns connection card", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({
      data: [],
      page: { next_cursor: null, has_more: false },
    }), {
      status: 200,
      headers: { "content-type": "application/json" },
    })));

    render(
      <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
        <NextIntlClientProvider locale="en" messages={{ settings, operations, common }}>
          <MailAccountsScreen />
        </NextIntlClientProvider>
      </QueryClientProvider>
    );

    expect(await screen.findByRole("heading", { name: "Mail accounts" })).toBeVisible();
    expect(screen.getByText("SMTP delivery")).toBeVisible();
    expect(screen.queryByText(/Zoho Campaigns/i)).not.toBeInTheDocument();
  });
});
