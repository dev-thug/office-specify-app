import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { NextIntlClientProvider } from "next-intl";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ContactComposeDialog } from "../contact-compose-dialog";
import { ContactDetailScreen } from "../contact-detail-screen";
import enCommon from "../../../../messages/en/common.json";
import enContacts from "../../../../messages/en/contacts.json";

vi.mock("@/features/companies/notes-panel", () => ({ NotesPanel: () => null }));
vi.mock("@/features/companies/tasks-panel", () => ({ TasksPanel: () => null }));
vi.mock("../contact-form-dialog", () => ({ ContactFormDialog: () => null }));
vi.mock("@/features/sending/draft-sending-workflow", () => ({
  DraftSendingWorkflow: ({
    draftId,
    mailAccountId,
  }: {
    draftId: string;
    mailAccountId: string;
  }) => (
    <section
      aria-label="Draft sending workflow"
      data-draft-id={draftId}
      data-mail-account-id={mailAccountId}
    />
  ),
}));

const messages = { common: enCommon, contacts: enContacts };
const recipient = {
  contactName: "Jina Kim",
  emailAddressId: "email-recipient-1",
  address: "jina@example.dev",
};

function renderDialog() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <NextIntlClientProvider locale="en" messages={messages}>
        <ContactComposeDialog
          open
          onOpenChange={vi.fn()}
          recipient={recipient}
        />
      </NextIntlClientProvider>
    </QueryClientProvider>
  );
}

function renderScreen() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <NextIntlClientProvider locale="en" messages={messages}>
        <ContactDetailScreen contactId="contact-1" />
      </NextIntlClientProvider>
    </QueryClientProvider>
  );
}

function response(status: number, body: unknown) {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

function installApi(mailAccounts: unknown[], draftHandlers: Array<() => Response | Promise<Response>>) {
  const calls: { url: string; init: RequestInit }[] = [];
  let draftCall = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      const call = { url: String(url), init: init ?? {} };
      calls.push(call);
      if (call.url === "/api/v1/me") {
        return response(200, {
          data: {
            principal_id: "principal-1",
            kind: "human",
            role: "admin",
            workspace_id: "workspace-1",
            display_name: "Jina",
            permission_version: 1,
            mail_accounts: mailAccounts,
          },
        });
      }
      if (call.url === "/api/v1/message-drafts" && call.init.method === "POST") {
        const handler = draftHandlers[Math.min(draftCall, draftHandlers.length - 1)]!;
        draftCall += 1;
        return handler();
      }
      throw new Error(`Unexpected API request: ${call.init.method} ${call.url}`);
    })
  );
  return calls;
}

const createdDraft = {
  draft_id: "draft-1",
  mail_account_id: "mailbox-compose",
  owner_principal_id: "principal-1",
  visibility: "private",
  to_email_address_id: "email-recipient-1",
  purpose: "correspondence",
  subject: "Meeting follow-up",
  body_text: "Thanks for meeting today.",
  body_html: null,
  expected_last_inbound_id: null,
  reply_to_message_id: null,
  thread_id: null,
  template_version_id: null,
  variables: null,
  status: "draft",
  version: 1,
  created_at: "2026-10-09T10:00:00Z",
  updated_at: "2026-10-09T10:00:00Z",
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("ContactComposeDialog", () => {
  it("opens the compose dialog from the exact contact email row", async () => {
    vi.stubGlobal(
      "ResizeObserver",
      class {
        observe() {}
        unobserve() {}
        disconnect() {}
      }
    );
    const contact = {
      id: "contact-1",
      company_id: null,
      name: "Jina Kim",
      job_title: null,
      department: null,
      owner_principal_id: null,
      owner_display_name: null,
      status: "active",
      archived_at: null,
      emails: [
        {
          email_address_id: "email-recipient-1",
          address: "jina@example.dev",
          label: "Work",
          is_primary: true,
          verification_status: "verified",
          marketing_eligibility: { eligible: true, reason_codes: [] },
          suppressions: [],
        },
      ],
      version: 1,
      updated_at: "2026-10-09T10:00:00Z",
      created_at: "2026-10-09T10:00:00Z",
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: RequestInfo | URL) => {
        if (String(url) === "/api/v1/contacts/contact-1") {
          return response(200, { data: contact });
        }
        if (String(url) === "/api/v1/me") {
          return response(200, {
            data: {
              principal_id: "principal-1",
              kind: "human",
              role: "member",
              workspace_id: "workspace-1",
              display_name: "Jina",
              permission_version: 1,
              mail_accounts: [],
            },
          });
        }
        throw new Error(`Unexpected API request: ${String(url)}`);
      })
    );
    renderScreen();

    fireEvent.click(await screen.findByRole("button", { name: "New message" }));

    expect(await screen.findByText("jina@example.dev")).toBeInTheDocument();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  it("shows only mailboxes with an explicit mail.compose grant, even for an admin", async () => {
    installApi(
      [
        { id: "mailbox-no-grant", address: "admin@example.dev", allowed_actions: [] },
        { id: "mailbox-compose", address: "writer@example.dev", allowed_actions: ["mail.compose"] },
      ],
      [() => response(201, { data: createdDraft })]
    );
    renderDialog();

    await waitFor(() => expect(screen.getByRole("combobox", { name: "From" })).toBeEnabled());
    fireEvent.click(screen.getByRole("combobox", { name: "From" }));
    const options = screen.getAllByRole("option");
    expect(options.map((option) => option.textContent)).toEqual(["writer@example.dev"]);
    expect(within(document.body).queryByText("admin@example.dev")).not.toBeInTheDocument();
  });

  it("creates an exact-recipient correspondence draft and opens the shared sending workflow without sending", async () => {
    const calls = installApi(
      [
        { id: "mailbox-compose", address: "writer@example.dev", allowed_actions: ["mail.compose"] },
      ],
      [() => response(201, { data: createdDraft })]
    );
    renderDialog();

    await waitFor(() => expect(screen.getByRole("combobox", { name: "From" })).toBeEnabled());
    fireEvent.click(screen.getByRole("combobox", { name: "From" }));
    fireEvent.click(screen.getByRole("option", { name: "writer@example.dev" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Subject" }), {
      target: { value: "Meeting follow-up" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), {
      target: { value: "Thanks for meeting today." },
    });
    expect(screen.getByRole("button", { name: "Create draft" })).toBeDisabled();
    fireEvent.click(screen.getByRole("combobox", { name: "Purpose" }));
    fireEvent.click(screen.getByRole("option", { name: "Correspondence" }));
    expect(screen.getByRole("button", { name: "Create draft" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Create draft" }));

    const workflow = await screen.findByRole("region", { name: "Draft sending workflow" });
    expect(workflow).toHaveAttribute("data-draft-id", "draft-1");
    expect(workflow).toHaveAttribute("data-mail-account-id", "mailbox-compose");
    expect(screen.getByRole("link", { name: "Open saved draft" })).toHaveAttribute(
      "href",
      "/app/inbox/drafts/draft-1"
    );
    const createRequest = calls.find(
      (call) => call.url === "/api/v1/message-drafts" && call.init.method === "POST"
    );
    expect(JSON.parse(String(createRequest?.init.body))).toEqual({
      mail_account_id: "mailbox-compose",
      to_email_address_id: "email-recipient-1",
      purpose: "correspondence",
      subject: "Meeting follow-up",
      body_text: "Thanks for meeting today.",
    });
    expect(calls.filter((call) => call.init.method === "POST").map((call) => call.url)).toEqual([
      "/api/v1/message-drafts",
    ]);
  });

  it("reuses the draft idempotency key after a network failure", async () => {
    let fail = true;
    const calls = installApi(
      [
        { id: "mailbox-compose", address: "writer@example.dev", allowed_actions: ["mail.compose"] },
      ],
      [
        () => {
          if (fail) {
            fail = false;
            return Promise.reject(new TypeError("network"));
          }
          return response(201, { data: createdDraft });
        },
      ]
    );
    renderDialog();

    await waitFor(() => expect(screen.getByRole("combobox", { name: "From" })).toBeEnabled());
    fireEvent.click(screen.getByRole("combobox", { name: "From" }));
    fireEvent.click(screen.getByRole("option", { name: "writer@example.dev" }));
    fireEvent.click(screen.getByRole("combobox", { name: "Purpose" }));
    fireEvent.click(screen.getByRole("option", { name: "Correspondence" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Subject" }), {
      target: { value: "Meeting follow-up" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), {
      target: { value: "Thanks for meeting today." },
    });

    const submit = screen.getByRole("button", { name: "Create draft" });
    fireEvent.click(submit);
    await waitFor(() => expect(calls.filter((call) => call.init.method === "POST")).toHaveLength(1));
    fireEvent.click(submit);
    await waitFor(() => expect(calls.filter((call) => call.init.method === "POST")).toHaveLength(2));

    const draftCalls = calls.filter((call) => call.url === "/api/v1/message-drafts");
    const key1 = new Headers(draftCalls[0]!.init.headers).get("Idempotency-Key");
    const key2 = new Headers(draftCalls[1]!.init.headers).get("Idempotency-Key");
    expect(key1).toMatch(/^[0-9a-f-]{36}$/);
    expect(key2).toBe(key1);
  });
});
