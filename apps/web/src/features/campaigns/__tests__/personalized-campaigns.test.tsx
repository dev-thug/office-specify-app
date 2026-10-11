import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { NextIntlClientProvider } from "next-intl";
import type { ReactElement } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import enCommon from "../../../../messages/en/common.json";
import enCompanies from "../../../../messages/en/companies.json";
import koCampaigns from "../../../../messages/ko/campaigns.json";
import enCampaigns from "../../../../messages/en/campaigns.json";
import enResearch from "../../../../messages/en/research.json";
import enTemplates from "../../../../messages/en/templates.json";
import enSending from "../../../../messages/en/sending.json";
import enMeetings from "../../../../messages/en/meetings.json";
import enStatus from "../../../../messages/en/status.json";
import enNav from "../../../../messages/en/nav.json";
import { SidebarNavContent } from "@/components/layout/app-sidebar";
import { CommandPalette } from "@/components/layout/command-palette";
import { TopBar } from "@/components/layout/top-bar";
import { CompaniesScreen } from "@/features/companies/companies-screen";
import { MeetingsPage } from "@/features/meetings/meetings-page";
import CampaignsPage from "@/app/(internal)/app/campaigns/page";
import { CampaignBuilder } from "@/features/campaigns/campaign-builder";
import { PersonalizedCampaignCreate } from "@/features/campaigns/personalized-campaign-create";
import { CampaignTargetDetailDialog } from "@/features/campaigns/campaign-target-detail-dialog";
import { ResearchEvidencePanel } from "@/features/research/research-evidence-panel";
import { ActivityFeed } from "@/features/companies/activity-feed";
import { activityGroup } from "@/features/companies/utils";
import CampaignDetailPage from "@/app/(internal)/app/campaigns/[id]/page";
import NewCampaignPage from "@/app/(internal)/app/campaigns/new/page";
import type {
  SalesCampaignResponse,
  SalesCampaignTarget,
} from "@/lib/api/sales-campaigns";
import { formatDateTime } from "@/features/campaigns/format";

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn() }),
  usePathname: () => "/app/contacts/contact-1",
}));

function mount(ui: ReactElement) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <NextIntlClientProvider
        locale="en"
        messages={{ common: enCommon, nav: enNav, campaigns: enCampaigns, companies: enCompanies, research: enResearch, templates: enTemplates, sending: enSending }}
      >
        {ui}
      </NextIntlClientProvider>
    </QueryClientProvider>
  );
}

beforeEach(() => {
  HTMLElement.prototype.scrollIntoView = vi.fn();
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
});

function campaignTarget(overrides: Partial<SalesCampaignTarget>): SalesCampaignTarget {
  return {
    id: "target-1",
    company_id: "company-1",
    contact_id: "contact-1",
    email_address_id: "email-1",
    company_name: "Northstar",
    contact_name: "Mina Park",
    email_address: "mina@northstar.example",
    stage: "research",
    workflow_stage: "research",
    version: 1,
    current_draft: null,
    outbound: null,
    created_at: "2026-10-10T00:00:00Z",
    updated_at: "2026-10-10T00:00:00Z",
    ...overrides,
  };
}

/** Moving a card to Draft/Review requires an existing draft (server rule). */
const EXISTING_DRAFT: NonNullable<SalesCampaignTarget["current_draft"]> = {
  draft_id: "existing-draft",
  subject: "Existing draft",
  status: "draft",
  version: 1,
  outbound_message_id: null,
};

function salesCampaign(targets: SalesCampaignTarget[]): SalesCampaignResponse {
  return {
    campaign: {
      id: "campaign-1",
      name: "Q4 account planning",
      objective: "Schedule planning calls",
      mail_account_id: "mailbox-1",
      delivery_channel_id: "channel-1",
      sender_identity_id: "sender-1",
      smtp_profile_id: "smtp-1",
      meeting_type_id: "meeting-1",
      from_address: "sales@example.test",
      from_name: "Sales Team",
      reply_to: "reply@example.test",
      sender_status: "active",
      version: 1,
      created_at: "2026-10-10T00:00:00Z",
      updated_at: "2026-10-10T00:00:00Z",
    },
    targets,
  };
}

interface BoardPointer {
  handle: HTMLElement;
  pointerId: number;
  x: number;
  y: number;
}

let restoreElementFromPoint: (() => void) | null = null;

function mockElementFromPoint(resolve: (x: number, y: number) => Element | null) {
  restoreElementFromPoint?.();
  const original = Object.getOwnPropertyDescriptor(document, "elementFromPoint");
  Object.defineProperty(document, "elementFromPoint", {
    configurable: true,
    value: resolve,
  });
  restoreElementFromPoint = () => {
    if (original) {
      Object.defineProperty(document, "elementFromPoint", original);
    } else {
      Reflect.deleteProperty(document, "elementFromPoint");
    }
    restoreElementFromPoint = null;
  };
}

function beginBoardPointer(
  card: HTMLElement,
  pointerId = 11,
  x = 10,
  y = 10
): BoardPointer {
  const handle = within(card).getByTitle(/drag|move/i);
  fireEvent.pointerDown(handle, {
    button: 0,
    pointerId,
    pointerType: "mouse",
    clientX: x,
    clientY: y,
  });
  return { handle, pointerId, x, y };
}

function moveBoardPointer(
  pointer: BoardPointer,
  x: number,
  y: number,
  pointerId = pointer.pointerId
) {
  fireEvent.pointerMove(pointer.handle, {
    pointerId,
    pointerType: "mouse",
    clientX: x,
    clientY: y,
  });
}

function endBoardPointer(
  pointer: BoardPointer,
  x = pointer.x,
  y = pointer.y,
  pointerId = pointer.pointerId
) {
  fireEvent.pointerUp(pointer.handle, {
    pointerId,
    pointerType: "mouse",
    clientX: x,
    clientY: y,
  });
}

function cancelBoardPointer(pointer: BoardPointer, pointerId = pointer.pointerId) {
  fireEvent.pointerCancel(pointer.handle, {
    pointerId,
    pointerType: "mouse",
    clientX: pointer.x,
    clientY: pointer.y,
  });
}

function finishBoardPointerOn(
  pointer: BoardPointer,
  moveLane: HTMLElement,
  releaseLane = moveLane
) {
  let hit: Element | null = moveLane;
  mockElementFromPoint(() => hit);
  const x = pointer.x + 12;
  moveBoardPointer(pointer, x, pointer.y);
  hit = releaseLane;
  endBoardPointer(pointer, x, pointer.y);
}

afterEach(() => {
  restoreElementFromPoint?.();
  vi.unstubAllGlobals();
});

describe("personalized campaign routes", () => {
  it("shows a floating card and landing placeholder, and Escape cancels without a request", async () => {
    const data = salesCampaign([campaignTarget({current_draft: EXISTING_DRAFT})]);
    const fetcher = vi.fn(async () => new Response(JSON.stringify({data}), {headers:{"content-type":"application/json"}}));
    vi.stubGlobal("fetch", fetcher);
    mount(await CampaignDetailPage({params:Promise.resolve({id:"campaign-1"})}));
    const card = await screen.findByRole("article", {name:"Northstar"});
    const lane = screen.getByRole("region", {name:"Draft"});
    mockElementFromPoint(() => lane);
    const pointer = beginBoardPointer(card);
    moveBoardPointer(pointer, 30, 30);
    expect(screen.getByTestId("board-drag-preview")).toHaveTextContent("Northstar");
    expect(within(lane).getByTestId("board-drop-placeholder")).toBeInTheDocument();
    fireEvent.keyDown(window, {key:"Escape"});
    expect(screen.queryByTestId("board-drag-preview")).not.toBeInTheDocument();
    expect(screen.queryByTestId("board-drop-placeholder")).not.toBeInTheDocument();
    endBoardPointer(pointer, 30, 30);
    // Reads (board + default sender) are fine; cancelling must not write.
    const calls = fetcher.mock.calls as unknown as [unknown, RequestInit?][];
    expect(calls.filter(([, init]) => init?.method && init.method !== "GET")).toHaveLength(0);
  });

  it("scrolls at the board edge and stops the animation when canceled", async () => {
    const data = salesCampaign([campaignTarget({current_draft: EXISTING_DRAFT})]);
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({data}), {headers:{"content-type":"application/json"}})));
    mount(await CampaignDetailPage({params:Promise.resolve({id:"campaign-1"})}));
    const card = await screen.findByRole("article", {name:"Northstar"});
    const board = screen.getByRole("region", {name:"Sales campaign kanban"});
    vi.spyOn(board, "getBoundingClientRect").mockReturnValue({left:0,right:300,top:0,bottom:300,width:300,height:300,x:0,y:0,toJSON:()=>({})});
    let frame!: FrameRequestCallback;
    const cancel = vi.fn();
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { frame = callback; return 123; });
    vi.stubGlobal("cancelAnimationFrame", cancel);
    mockElementFromPoint(() => screen.getByRole("region", {name:"Draft"}));
    const pointer = beginBoardPointer(card);
    moveBoardPointer(pointer, 290, 150);
    act(() => frame(16.67));
    expect(board.scrollLeft).toBeGreaterThan(0);
    expect(screen.getByTestId("board-drag-preview").style.transform).toContain("280px");
    expect(document.body.style.cursor).toBe("grabbing");
    fireEvent.keyDown(window, {key:"Escape"});
    expect(cancel).toHaveBeenCalledWith(123);
    expect(document.body.style.cursor).not.toBe("grabbing");
  });

  it("accepts rapid round trips during a slow save and serializes the latest destination", async () => {
    const data = salesCampaign([campaignTarget({current_draft: EXISTING_DRAFT})]);
    const saves: {body: Record<string, unknown>; resolve: (r:Response)=>void}[] = [];
    vi.stubGlobal("fetch", vi.fn(async (_input:RequestInfo | URL, init?:RequestInit) => {
      if (init?.method === "PATCH") return new Promise<Response>(resolve => saves.push({body:JSON.parse(String(init.body)),resolve}));
      return new Response(JSON.stringify({data}), {headers:{"content-type":"application/json"}});
    }));
    mount(await CampaignDetailPage({params:Promise.resolve({id:"campaign-1"})}));
    let card = await screen.findByRole("article", {name:"Northstar"});
    finishBoardPointerOn(beginBoardPointer(card), screen.getByRole("region", {name:"Draft"}));
    await waitFor(() => expect(saves).toHaveLength(1));
    card = within(screen.getByRole("region", {name:"Draft"})).getByRole("article", {name:"Northstar"});
    finishBoardPointerOn(beginBoardPointer(card, 12), screen.getByRole("region", {name:"Review"}));
    card = within(screen.getByRole("region", {name:"Review"})).getByRole("article", {name:"Northstar"});
    finishBoardPointerOn(beginBoardPointer(card, 13), screen.getByRole("region", {name:"Researched"}));
    expect(within(screen.getByRole("region", {name:"Researched"})).getByRole("article", {name:"Northstar"})).toBeInTheDocument();
    expect(saves).toHaveLength(1);
    await act(async () => saves[0]!.resolve(new Response(JSON.stringify({data:campaignTarget({stage:"draft",workflow_stage:"draft",version:2})}), {headers:{"content-type":"application/json"}})));
    await waitFor(() => expect(saves).toHaveLength(2));
    expect(saves[1]!.body).toMatchObject({stage:"research",expected_target_version:2});
    await act(async () => saves[1]!.resolve(new Response(JSON.stringify({data:campaignTarget({stage:"research",workflow_stage:"research",version:3})}), {headers:{"content-type":"application/json"}})));
    await waitFor(() => expect(within(screen.getByRole("region", {name:"Researched"})).getByRole("article", {name:"Northstar"})).toHaveAttribute("aria-busy","false"));
    expect(saves).toHaveLength(2);
  });

  it("moves immediately while saving and restores the original lane on failure", async () => {
    const data = salesCampaign([campaignTarget({current_draft: EXISTING_DRAFT})]);
    let rejectMove!: (response: Response) => void;
    vi.stubGlobal("fetch", vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === "PATCH") return new Promise<Response>(resolve => { rejectMove = resolve; });
      return new Response(JSON.stringify({data}), {headers:{"content-type":"application/json"}});
    }));
    mount(await CampaignDetailPage({params:Promise.resolve({id:"campaign-1"})}));
    const card = await screen.findByRole("article", {name:"Northstar"});
    const pointer = beginBoardPointer(card);
    const lane = screen.getByRole("region", {name:"Draft"});
    finishBoardPointerOn(pointer, lane);
    expect(within(lane).getByRole("article", {name:"Northstar"})).toHaveAttribute("aria-busy", "true");
    await waitFor(() => expect(rejectMove).toBeTypeOf("function"));
    rejectMove(new Response(JSON.stringify({type:"about:blank",title:"Conflict",status:409,code:"VERSION_MISMATCH"}), {status:409,headers:{"content-type":"application/json"}}));
    await waitFor(() => expect(within(screen.getByRole("region", {name:"Researched"})).getByRole("article", {name:"Northstar"})).toHaveAttribute("aria-busy", "false"));
  });

  it("uses Contacts context in the top bar while the sidebar stays on Companies", () => {
    mount(<TopBar />);

    expect(screen.getByText("Contacts", { exact: true })).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Help with Contacts" })
    ).toBeInTheDocument();
  });

  it("finds MCP service credentials through the command palette", async () => {
    mount(<CommandPalette open onOpenChange={() => undefined} />);

    fireEvent.change(
      screen.getByPlaceholderText(enCommon.commandPalette.placeholder),
      { target: { value: "MCP" } }
    );
    expect(await screen.findByText("Members & agent access")).toBeInTheDocument();
  });

  it("renders seven primary sidebar links and highlights Companies for contact routes", () => {
    mount(<SidebarNavContent />);

    const links = within(screen.getByRole("navigation")).getAllByRole("link");
    expect(links.map((link) => link.getAttribute("href"))).toEqual([
      "/app/dashboard",
      "/app/companies",
      "/app/campaigns",
      "/app/inbox",
      "/app/templates",
      "/app/meetings",
      "/app/settings",
    ]);
    const companiesLink = within(screen.getByRole("navigation")).getByRole("link", {
      name: "Companies",
    });
    expect(companiesLink).not.toHaveAttribute("aria-current");
    expect(companiesLink).toHaveClass("bg-sidebar-accent");
  });

  it("uses delivery-oriented Korean labels for queued and provider-accepted columns", () => {
    expect(koCampaigns.personalized.board.columns.queued).toBe("발송 대기");
    expect(koCampaigns.personalized.board.columns.accepted).toBe("전송됨");
    expect(koCampaigns.personalized.board.acceptedNote).toContain("SMTP");
  });

  it("keeps the standalone CRM evidence panel pending-only by default", async () => {
    let requestedUrl: string | null = null;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        requestedUrl = String(input);
        return new Response(JSON.stringify({ data: [] }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );

    mount(<ResearchEvidencePanel companyId="company-1" />);

    await waitFor(() => expect(requestedUrl).not.toBeNull());
    const url = new URL(requestedUrl!, "http://localhost");
    expect(url.searchParams.get("review_status")).toBe("pending");
    expect(screen.getByRole("combobox", { name: "Evidence review filter" })).toHaveTextContent(
      "Pending only"
    );
  });

  it("renders permission-filtered CRM note bodies only when the activity summary returns one", async () => {
    let noteSummary: Record<string, unknown> = { note_id: "note-1" };
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(
          JSON.stringify({
            data: [
              {
                id: "note-activity-1",
                company_id: "company-1",
                contact_id: null,
                mail_account_id: null,
                thread_id: null,
                booking_id: null,
                actor_principal_id: "owner-1",
                kind: "note.created",
                source_event_id: "note-event-1",
                occurred_at: "2026-10-09T12:00:00Z",
                summary: noteSummary,
              },
            ],
          }),
          { status: 200, headers: { "content-type": "application/json" } }
        )
      )
    );

    const renderNotes = () =>
      mount(
        <ActivityFeed
          path="/api/v1/companies/company-1/activities"
          ns="companies"
          filter={(activity) => activityGroup(activity) === "note"}
          emptyTitle="No notes"
          queryKeySuffix="campaign-card-notes"
          includeNoteBody
        />
      );

    const withoutBody = renderNotes();
    await screen.findByText("Note");
    expect(screen.queryByText("Sensitive CRM note body")).toBeNull();
    withoutBody.unmount();

    noteSummary = { note_id: "note-1", body: "Sensitive CRM note body" };
    renderNotes();
    expect(await screen.findByText("Sensitive CRM note body")).toBeInTheDocument();
  });

  it("uses the server-resolved sole eligible sender without adding a sender selector", async () => {
    const sender = {
      smtp_profile_id: "smtp-sole",
      profile_name: "Only ready sender",
      mail_account_id: "mailbox-1",
      mailbox_name: "Sales mailbox",
      delivery_channel_id: "channel-1",
      sender_identity_id: "sender-1",
      from_address: "sales@example.test",
      from_name: "Sales Team",
      reply_to: null,
      status: "active",
      ready: true,
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = new URL(String(input), "http://localhost");
        const data = url.pathname === "/api/v1/sales-campaigns/sender"
          ? { sender }
          : [];
        return new Response(JSON.stringify({ data }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );
    const page = await NewCampaignPage({ searchParams: Promise.resolve({}) });
    mount(page);

    expect(await screen.findByText(/Only ready sender/)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Campaign name"), {
      target: { value: "Sole sender campaign" },
    });
    expect(screen.getByRole("button", { name: "Create campaign" })).toBeEnabled();
    expect(screen.queryByLabelText(/mailbox|delivery channel|sender profile/i)).toBeNull();
  });

  it("routes only explicit legacy revision and clone bookmarks to the bulk builder", async () => {
    const source = "550e8400-e29b-41d4-a716-446655440000";
    const revision = await NewCampaignPage({
      searchParams: Promise.resolve({ source, mode: "revision" }),
    });
    const clone = await NewCampaignPage({
      searchParams: Promise.resolve({ source, mode: "clone" }),
    });
    const invalidMode = await NewCampaignPage({
      searchParams: Promise.resolve({ source, mode: "other" }),
    });
    const invalidSource = await NewCampaignPage({
      searchParams: Promise.resolve({ source: [source, source], mode: "clone" }),
    });

    expect(revision.type).toBe(CampaignBuilder);
    expect(revision.props).toMatchObject({ mode: "revision", sourceCampaignId: source });
    expect(clone.type).toBe(CampaignBuilder);
    expect(clone.props).toMatchObject({ mode: "clone", sourceCampaignId: source });
    expect(invalidMode.type).toBe(PersonalizedCampaignCreate);
    expect(invalidSource.type).toBe(PersonalizedCampaignCreate);
  });

  it("links related destinations from the Companies and Meetings headers", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(JSON.stringify({ data: [], page: { next_cursor: null, has_more: false } }), {
          status: 200,
          headers: { "content-type": "application/json" },
        })
      )
    );

    const client = () =>
      new QueryClient({
        defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
      });
    const messages = {
      common: enCommon,
      companies: enCompanies,
      meetings: enMeetings,
      status: enStatus,
    };

    const companiesView = render(
      <QueryClientProvider client={client()}>
        <NextIntlClientProvider locale="en" messages={messages}>
          <CompaniesScreen />
        </NextIntlClientProvider>
      </QueryClientProvider>
    );
    expect(await screen.findByRole("heading", { name: "Companies" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Contacts" })).toHaveAttribute(
      "href",
      "/app/contacts"
    );
    companiesView.unmount();

    render(
      <QueryClientProvider client={client()}>
        <NextIntlClientProvider locale="en" messages={messages}>
          <MeetingsPage />
        </NextIntlClientProvider>
      </QueryClientProvider>
    );
    expect(await screen.findByRole("heading", { name: "Meetings" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Meeting types" })).toHaveAttribute(
      "href",
      "/app/meeting-types"
    );
  });

  it("shows the personalized workflow and keeps bulk campaigns on a labeled path", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(JSON.stringify({ data: [] }), {
          status: 200,
          headers: { "content-type": "application/json" },
        })
      )
    );

    mount(<CampaignsPage />);

    expect(
      await screen.findByRole("heading", { name: "Sales campaigns" })
    ).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "New sales campaign" })
    ).toHaveAttribute("href", "/app/campaigns/new");
    expect(screen.getByRole("link", { name: "Bulk campaigns" })).toHaveAttribute(
      "href",
      "/app/campaigns/bulk"
    );
    expect(screen.getByRole("link", { name: "Approvals" })).toHaveAttribute(
      "href",
      "/app/approvals"
    );
  });

  it("blocks campaign creation when the default marketing sender is missing without showing mailbox selectors", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = new URL(String(input), "http://localhost");
        const data =
          url.pathname === "/api/v1/sales-campaigns/sender"
            ? { sender: null }
            : [];
        return new Response(JSON.stringify({ data }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );
    mount(await NewCampaignPage({ searchParams: Promise.resolve({}) }));

    expect(
      await screen.findByRole("heading", { name: "New sales campaign" })
    ).toBeInTheDocument();
    expect(await screen.findByRole("link", { name: "Configure sender in SMTP settings" })).toHaveAttribute(
      "href",
      "/app/settings/mail-accounts"
    );
    expect(screen.queryByLabelText(/mailbox|delivery channel/i)).toBeNull();
    expect(screen.getByRole("button", { name: "Create campaign" })).toBeDisabled();
  });

  it("keeps separate company drafts in their server-reported board columns", async () => {
    const data = salesCampaign([
      campaignTarget({
        id: "northstar-target",
        company_name: "Northstar",
        stage: "draft",
        workflow_stage: "draft",
        current_draft: {
          draft_id: "northstar-draft",
          subject: "Toronto expansion planning",
          status: "draft",
          version: 2,
          outbound_message_id: null,
        },
      }),
      campaignTarget({
        id: "atlas-target",
        company_id: "company-2",
        contact_id: "contact-2",
        email_address_id: "email-2",
        company_name: "Atlas Security",
        contact_name: "Jae Kim",
        email_address: "jae@atlas.example",
        stage: "review",
        workflow_stage: "review",
        current_draft: {
          draft_id: "atlas-draft",
          subject: "Atlas access review",
          status: "draft",
          version: 3,
          outbound_message_id: null,
        },
      }),
      campaignTarget({
        id: "accepted-target",
        company_id: "company-3",
        contact_id: "contact-3",
        email_address_id: "email-3",
        company_name: "Acme Analytics",
        contact_name: "Lee Han",
        email_address: "lee@acme.example",
        stage: "accepted",
        workflow_stage: "review",
        outbound: {
          outbound_message_id: "outbound-accepted",
          status: "accepted",
          payload_hash: "ef".repeat(32),
          approval_status: "approved",
          approval_expires_at: null,
          approved_at: "2026-10-10T00:00:00Z",
          provider_accepted: true,
          accepted_at: "2026-10-10T00:00:00Z",
          failure_code: null,
          open_count: 0,
          click_count: 0,
          known_reply_count: 0,
          last_reply_at: null,
          not_before: "2026-10-09T12:00:00Z",
        },
      }),
      campaignTarget({
        id: "attention-target",
        company_id: "company-4",
        contact_id: "contact-4",
        email_address_id: "email-4",
        company_name: "Redwood Systems",
        contact_name: "Sam Cho",
        email_address: "sam@redwood.example",
        stage: "attention",
        workflow_stage: "review",
        outbound: {
          outbound_message_id: "outbound-unknown",
          status: "failed",
          payload_hash: "ab".repeat(32),
          approval_status: "approved",
          approval_expires_at: null,
          approved_at: "2026-10-10T00:00:00Z",
          provider_accepted: false,
          accepted_at: null,
          failure_code: "SMTP_FAILURE",
          open_count: 0,
          click_count: 0,
          known_reply_count: 0,
          last_reply_at: null,
          not_before: "2026-10-09T13:00:00Z",
        },
      }),
      campaignTarget({
        id: "queued-target",
        company_name: "Borealis",
        stage: "queued",
        workflow_stage: "review",
        current_draft: {
          draft_id: "queued-draft",
          subject: "Borealis planning",
          status: "frozen",
          version: 1,
          outbound_message_id: "outbound-queued",
        },
        outbound: {
          outbound_message_id: "outbound-queued",
          status: "queued",
          payload_hash: "bc".repeat(32),
          approval_status: "approved",
          approval_expires_at: null,
          approved_at: "2026-10-10T00:00:00Z",
          provider_accepted: false,
          accepted_at: null,
          failure_code: null,
          open_count: 0,
          click_count: 0,
          known_reply_count: 0,
          last_reply_at: null,
          not_before: "2026-10-10T12:30:00Z",
        },
      }),
    ]);
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = new URL(String(input), "http://localhost");
        const response =
          url.pathname === "/api/v1/sales-campaigns/campaign-1"
            ? data
            : [];
        return new Response(JSON.stringify({ data: response }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );
    const page = await CampaignDetailPage({
      params: Promise.resolve({ id: "campaign-1" }),
    });

    mount(page);

    expect(
      await screen.findByRole("heading", { name: "Q4 account planning" })
    ).toBeInTheDocument();
    const kanban = screen.getByRole("region", { name: "Sales campaign kanban" });
    expect(kanban).toHaveClass("overflow-auto");
    expect(kanban).toHaveAttribute("tabindex", "0");
    expect(kanban.firstElementChild).toHaveClass("min-w-[1680px]");
    expect(screen.getByRole("region", { name: "Researched" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Draft" })).toHaveTextContent(
      "Toronto expansion planning"
    );
    expect(screen.getByRole("region", { name: "Review" })).toHaveTextContent(
      "Atlas access review"
    );
    expect(screen.getByRole("region", { name: "Queued" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Accepted" })).toHaveTextContent(
      "Acme Analytics"
    );
    expect(screen.getByRole("region", { name: "Attention" })).toHaveTextContent(
      "Redwood Systems"
    );
    const nextBatch = screen.getByText(/^Next send batch:/);
    expect(nextBatch).toHaveTextContent(formatDateTime("2026-10-10T12:30:00Z", "en"));
    expect(nextBatch).not.toHaveTextContent(formatDateTime("2026-10-09T12:00:00Z", "en"));
    expect(nextBatch).not.toHaveTextContent(formatDateTime("2026-10-09T13:00:00Z", "en"));
    expect(screen.getByRole("article", { name: "Acme Analytics" })).toHaveTextContent(
      `Original batch time: ${formatDateTime("2026-10-09T12:00:00Z", "en")}`
    );
    expect(screen.getByRole("article", { name: "Redwood Systems" })).toHaveTextContent(
      `Original batch time: ${formatDateTime("2026-10-09T13:00:00Z", "en")}`
    );
  });

  it("opens the company research and the subject and body for that company's own draft", async () => {
    const data = salesCampaign([
      campaignTarget({
        id: "northstar-target",
        stage: "draft",
        workflow_stage: "draft",
        current_draft: {
          draft_id: "northstar-draft",
          subject: "Toronto expansion planning",
          status: "draft",
          version: 2,
          outbound_message_id: null,
        },
      }),
    ]);
    const refreshedData = salesCampaign([
      campaignTarget({
        id: "northstar-target",
        company_name: "Northstar",
        stage: "draft",
        workflow_stage: "draft",
        current_draft: {
          draft_id: "northstar-draft",
          subject: "Remote subject",
          status: "draft",
          version: 3,
          outbound_message_id: null,
        },
      }),
    ]);
    const draft = {
      draft_id: "northstar-draft",
      sales_campaign_id: "campaign-1",
      sales_target_id: "northstar-target",
      outbound_message_id: null,
      mail_account_id: "mailbox-1",
      owner_principal_id: "owner-1",
      visibility: "mailbox",
      thread_id: null,
      reply_to_message_id: null,
      expected_last_inbound_id: null,
      to_email_address_id: "email-1",
      template_version_id: null,
      purpose: "marketing",
      subject: "Toronto expansion planning",
      body_text: "I saw Northstar opened its Toronto office.",
      body_html: "<p>I saw Northstar opened its Toronto office.</p>",
      variables: {},
      status: "draft",
      version: 2,
      created_at: "2026-10-10T00:00:00Z",
      updated_at: "2026-10-10T00:00:00Z",
    };
    const crmCompany = {
      id: "company-1",
      name: "Northstar",
      domain: "northstar.example",
      website_url: "https://northstar.example",
      industry: "Cloud identity security",
      country_code: "CA",
      lifecycle_stage: "prospect",
      owner_principal_id: null,
      description: "Northstar opened a Toronto office in 2025.",
      custom_fields: {},
      archived_at: null,
      version: 2,
      created_at: "2026-10-09T00:00:00Z",
      updated_at: "2026-10-10T00:00:00Z",
      allowed_actions: ["crm.read"],
    };
    const companyNote = {
      id: "note-1",
      company_id: "company-1",
      contact_id: null,
      mail_account_id: null,
      thread_id: null,
      booking_id: null,
      actor_principal_id: "owner-1",
      actor_display_name: "Mina Park",
      kind: "note.created",
      source_event_id: "note-event-1",
      occurred_at: "2026-10-09T12:00:00Z",
      summary: {
        note_id: "note-1",
        body: "Ops confirmed the Toronto expansion plan.",
      },
    };
    const reviewedEvidence = {
      id: "evidence-reviewed-1",
      company_id: "company-1",
      contact_id: null,
      lead_proposal_id: null,
      field_path: "industry",
      source_url: "https://gitlab.com/northstar/research",
      observed_at: "2026-10-09T00:00:00Z",
      excerpt: "The Toronto office opened in 2025.",
      value: "Cloud identity security",
      evidence_type: "observed",
      confidence: 0.95,
      review_status: "verified",
      created_by: "agent-1",
      reviewed_by: "owner-1",
      created_at: "2026-10-09T00:00:00Z",
    };
    const researchStatuses: (string | null)[] = [];
    const paths: string[] = [];
    let campaignReads = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = new URL(String(input), "http://localhost");
        paths.push(url.pathname);
        let responseData: unknown = [];
        if (url.pathname === "/api/v1/sales-campaigns/campaign-1") {
          campaignReads += 1;
          responseData = campaignReads > 1 ? refreshedData : data;
        } else if (url.pathname === "/api/v1/message-drafts/northstar-draft") {
          responseData = draft;
        } else if (url.pathname === "/api/v1/companies/company-1") {
          responseData = crmCompany;
        } else if (url.pathname === "/api/v1/companies/company-1/activities") {
          responseData = [companyNote];
        } else if (url.pathname === "/api/v1/research-evidence") {
          const reviewStatus = url.searchParams.get("review_status");
          researchStatuses.push(reviewStatus);
          responseData = reviewStatus === "pending" ? [] : [reviewedEvidence];
        } else if (url.pathname === "/api/v1/me") {
          responseData = { principal_id: "owner-1", kind: "human" };
        } else if (url.pathname === "/api/v1/mail-accounts") {
          responseData = [
            {
              id: "mailbox-1",
              primary_address: "sales@example.test",
              status: "active",
              allowed_actions: ["mail.read", "mail.compose", "mail.send"],
            },
          ];
        } else if (url.pathname === "/api/v1/delivery-channels") {
          responseData = [
            {
              id: "channel-1",
              mail_account_id: "mailbox-1",
              sender_identity_id: "sender-1",
              purpose: "marketing",
              name: "Campaign channel",
              enabled: true,
              policy_status: "allowed",
            },
          ];
        } else if (url.pathname === "/api/v1/meeting-types") {
          responseData = [
            {
              id: "meeting-1",
              host_principal_id: "owner-1",
              calendar_connection_id: "calendar-1",
              name: "Planning call",
              duration_minutes: 30,
              status: "active",
            },
          ];
        }
        return new Response(JSON.stringify({ data: responseData }), {
          status: 200,
          headers: { "content-type": "application/json", etag: '"2"' },
        });
      })
    );
    const page = await CampaignDetailPage({
      params: Promise.resolve({ id: "campaign-1" }),
    });
    mount(page);

    fireEvent.click(
      await screen.findByRole("button", { name: "Open Northstar details" })
    );
    const dialog = await screen.findByRole("dialog");
    await waitFor(() =>
      expect(paths).toContain("/api/v1/message-drafts/northstar-draft")
    );
    expect(within(dialog).getByText("Evidence and sources")).toBeInTheDocument();
    expect(await within(dialog).findByText("Northstar opened a Toronto office in 2025.")).toBeInTheDocument();
    expect(await within(dialog).findByText("Ops confirmed the Toronto expansion plan.")).toBeInTheDocument();
    expect(await within(dialog).findByRole("link", { name: "https://gitlab.com/northstar/research" })).toHaveAttribute("href", "https://gitlab.com/northstar/research");
    expect(researchStatuses).toContain(null);
    expect(within(dialog).getByText("From: Sales Team <sales@example.test>")).toBeInTheDocument();
    expect(within(dialog).getByText("Reply-To: reply@example.test")).toBeInTheDocument();
    expect(await within(dialog).findByText("Draft for Northstar")).toBeInTheDocument();
    expect(await within(dialog).findByLabelText("Subject")).toHaveValue(
      "Toronto expansion planning"
    );
    expect(within(dialog).queryByLabelText("Plain-text fallback")).toBeNull();
    expect(await within(dialog).findByRole("textbox", { name: "Email message" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Edit HTML" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Sample preview" })).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "Plain text" }));
    expect(await within(dialog).findByLabelText("Plain-text fallback")).toHaveValue(
      "I saw Northstar opened its Toronto office."
    );

    fireEvent.change(within(dialog).getByLabelText("Subject"), {
      target: { value: "Local subject edit" },
    });
    fireEvent.change(within(dialog).getByLabelText("Plain-text fallback"), {
      target: { value: "Local body edit" },
    });
    await waitFor(() => expect(campaignReads).toBeGreaterThan(1), { timeout: 5_000 });
    expect(within(dialog).getByLabelText("Subject")).toHaveValue("Local subject edit");
    expect(within(dialog).getByLabelText("Plain-text fallback")).toHaveValue("Local body edit");
    expect(within(dialog).getByText(/This draft changed on the server/)).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Save changes" })).toBeDisabled();
    fireEvent.click(within(dialog).getByRole("button", { name: "Formatted email" }));
    fireEvent.click(within(dialog).getByRole("button", { name: "Sample preview" }));
    expect(await screen.findByText("Sample data only · not saved as recipient values")).toBeInTheDocument();
  });

  it.each([{contactStatus:"active", verification:"verified", eligible:true}, {contactStatus:"new", verification:"unknown", eligible:false}])("adds a target with $contactStatus contacts and $verification email", async ({contactStatus, verification, eligible}) => {
    const data = salesCampaign([]);
    const calls: { url: string; init: RequestInit }[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), "http://localhost");
        const request = init ?? {};
        calls.push({ url: `${url.pathname}${url.search}`, init: request });
        let responseData: unknown = [];
        if (url.pathname === "/api/v1/sales-campaigns/campaign-1") {
          responseData = data;
        } else if (url.pathname === "/api/v1/companies") {
          responseData = [
            {
              id: "company-1",
              name: "Northstar",
              domain: "northstar.example",
              website_url: null,
              industry: null,
              country_code: null,
              lifecycle_stage: "prospect",
              owner_principal_id: "owner-1",
              description: null,
              custom_fields: {},
              archived_at: null,
              version: 1,
              updated_at: "2026-10-10T00:00:00Z",
              created_at: "2026-10-10T00:00:00Z",
            },
          ];
        } else if (url.pathname === "/api/v1/contacts") {
          expect(url.searchParams.get("status")).toBeNull();
          responseData = [
            {
              id: "contact-1",
              company_id: "company-1",
              name: "Mina Park",
              job_title: "VP of Operations",
              status: contactStatus,
              version: 1,
              updated_at: "2026-10-10T00:00:00Z",
              created_at: "2026-10-10T00:00:00Z",
            },
          ];
        } else if (url.pathname === "/api/v1/contacts/contact-1") {
          responseData = {
            id: "contact-1",
            company_id: "company-1",
            name: "Mina Park",
            job_title: "VP of Operations",
            status: contactStatus,
            version: 1,
            updated_at: "2026-10-10T00:00:00Z",
            created_at: "2026-10-10T00:00:00Z",
            emails: [
              {
                email_address_id: "email-1",
                address: "mina@northstar.example",
                is_primary: true,
                verification_status: verification,
                marketing_eligibility: { eligible, reason_codes: eligible ? [] : ["SEND_BASIS_UNVERIFIED"] },
                suppressions: [],
              },
            ],
          };
        } else if (
          url.pathname === "/api/v1/sales-campaigns/campaign-1/targets" &&
          request.method === "POST"
        ) {
          responseData = campaignTarget({ id: "target-added", stage: "research" });
        }
        return new Response(JSON.stringify({ data: responseData }), {
          status: request.method === "POST" ? 201 : 200,
          headers: { "content-type": "application/json" },
        });
      })
    );
    const page = await CampaignDetailPage({
      params: Promise.resolve({ id: "campaign-1" }),
    });
    mount(page);

    fireEvent.click((await screen.findAllByRole("button", { name: "Add company" }))[0]!);
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("combobox", { name: "Search companies…" }));
    fireEvent.change(await screen.findByRole("textbox", { name: "Search companies…" }), {
      target: { value: "Northstar" },
    });
    fireEvent.click(await screen.findByRole("button", { name: /Northstar/ }));
    await waitFor(() =>
      expect(
        within(dialog).getByRole("combobox", { name: "Contact" })
      ).toBeEnabled()
    );
    fireEvent.click(within(dialog).getByRole("combobox", { name: "Contact" }));
    fireEvent.click(await screen.findByRole("option", { name: /Mina Park/ }));
    // A single address is picked automatically — no second email selection.
    await waitFor(() =>
      expect(within(dialog).getByText("mina@northstar.example")).toBeInTheDocument()
    );
    expect(
      within(dialog).queryByRole("combobox", { name: "Recipient email" })
    ).not.toBeInTheDocument();
    await waitFor(() =>
      expect(within(dialog).getByRole("button", { name: "Add company" })).toBeEnabled()
    );
    fireEvent.click(within(dialog).getByRole("button", { name: "Add company" }));

    await waitFor(() =>
      expect(
        calls.some(
          (call) =>
            call.url === "/api/v1/sales-campaigns/campaign-1/targets" &&
            call.init.method === "POST"
        )
      ).toBe(true)
    );
    const add = calls.find(
      (call) => call.url === "/api/v1/sales-campaigns/campaign-1/targets"
    )!;
    expect(JSON.parse(String(add.init.body))).toEqual({
      company_id: "company-1",
      contact_id: "contact-1",
      email_address_id: "email-1",
    });
  });

  it("opens the draft editor instead of PATCHing when a card without a draft is moved to Draft", async () => {
    const data = salesCampaign([
      campaignTarget({
        id: "research-target",
        company_name: "Northstar",
        stage: "research",
        workflow_stage: "research",
        version: 1,
        current_draft: null,
      }),
    ]);
    const patches: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), "http://localhost");
        if (init?.method === "PATCH") patches.push(url.pathname);
        const response = url.pathname === "/api/v1/sales-campaigns/campaign-1"
          ? data
          : [];
        return new Response(JSON.stringify({ data: response }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );
    const page = await CampaignDetailPage({
      params: Promise.resolve({ id: "campaign-1" }),
    });
    mount(page);

    const card = await screen.findByRole("article", { name: "Northstar" });
    fireEvent.click(within(card).getByRole("button", { name: /Move Northstar to Draft/ }));

    expect(
      await screen.findByText(/Write a draft for this company first/)
    ).toBeInTheDocument();
    expect(patches).toEqual([]);
  });

  it("keeps Save draft enabled and names the empty fields when it is pressed", async () => {
    const data = salesCampaign([
      campaignTarget({ id: "empty-target", stage: "research", current_draft: null }),
    ]);
    const posts: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), "http://localhost");
        if (init?.method === "POST") posts.push(url.pathname);
        return new Response(JSON.stringify({ data: [] }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );
    mount(
      <CampaignTargetDetailDialog
        campaign={data.campaign}
        target={data.targets[0]!}
        queueNotice={null}
        open
        onOpenChange={() => undefined}
        onChanged={() => undefined}
      />
    );

    const save = await screen.findByRole("button", { name: "Save draft" });
    expect(save).toBeEnabled();
    fireEvent.click(save);

    expect(await screen.findByText(/Please fill in:/)).toHaveTextContent(/Subject/);
    expect(screen.getByText("Enter a subject")).toBeInTheDocument();
    expect(screen.getByText("Enter the message body")).toBeInTheDocument();
    expect(posts).toEqual([]);
  });

  it("allows an accessible backward move from Review to Draft", async () => {
    const data = salesCampaign([
      campaignTarget({
        id: "review-target",
        company_name: "Northstar",
        stage: "review",
        workflow_stage: "review",
        version: 5,
        current_draft: {
          draft_id: "review-draft",
          subject: "Northstar planning",
          status: "draft",
          version: 2,
          outbound_message_id: null,
        },
      }),
    ]);
    let patchBody: Record<string, unknown> | null = null;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), "http://localhost");
        if (
          init?.method === "PATCH" &&
          url.pathname.endsWith("/targets/review-target")
        ) {
          patchBody = JSON.parse(String(init.body)) as Record<string, unknown>;
          data.targets[0]!.stage = "draft";
          data.targets[0]!.workflow_stage = "draft";
          data.targets[0]!.version = 6;
          return new Response(JSON.stringify({ data: data.targets[0] }), {
            status: 200,
            headers: { "content-type": "application/json" },
          });
        }
        const response = url.pathname === "/api/v1/sales-campaigns/campaign-1"
          ? data
          : [];
        return new Response(JSON.stringify({ data: response }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );
    const page = await CampaignDetailPage({
      params: Promise.resolve({ id: "campaign-1" }),
    });
    mount(page);

    const card = await screen.findByRole("article", { name: "Northstar" });
    fireEvent.click(
      within(card).getByRole("button", { name: "Move Northstar back to Draft" })
    );

    await waitFor(() => expect(patchBody).toEqual({
      stage: "draft",
      expected_target_version: 5,
    }));
    await waitFor(() => {
      expect(screen.getByRole("region", { name: "Draft" })).toHaveTextContent("Northstar");
    });
  });

  it("uses pointer ownership, cancellation, board guards, and the release lane", async () => {
    const data = salesCampaign([
      campaignTarget({
        id: "northstar-target",
        stage: "research",
        workflow_stage: "research",
        version: 2,
        current_draft: EXISTING_DRAFT,
      }),
    ]);
    const stageUpdates: Record<string, unknown>[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), "http://localhost");
        if (
          init?.method === "PATCH" &&
          url.pathname.endsWith("/targets/northstar-target")
        ) {
          const body = JSON.parse(String(init.body)) as Record<string, unknown>;
          stageUpdates.push(body);
          data.targets[0]!.stage = "review";
          data.targets[0]!.workflow_stage = "review";
          data.targets[0]!.version += 1;
          return new Response(JSON.stringify({ data: data.targets[0] }), {
            status: 200,
            headers: { "content-type": "application/json" },
          });
        }
        const response =
          url.pathname === "/api/v1/sales-campaigns/campaign-1" ? data : [];
        return new Response(JSON.stringify({ data: response }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );
    const page = await CampaignDetailPage({
      params: Promise.resolve({ id: "campaign-1" }),
    });
    mount(page);

    const card = await screen.findByRole("article", { name: "Northstar" });
    const handle = within(card).getByTitle(/drag|move/i);
    const setPointerCapture = vi.fn();
    const hasPointerCapture = vi.fn(() => true);
    const releasePointerCapture = vi.fn();
    Object.defineProperties(handle, {
      setPointerCapture: { configurable: true, value: setPointerCapture },
      hasPointerCapture: { configurable: true, value: hasPointerCapture },
      releasePointerCapture: { configurable: true, value: releasePointerCapture },
    });
    expect(card).toHaveAttribute("draggable", "false");
    expect(handle).toHaveAttribute("draggable", "false");
    expect(handle).toHaveClass("cursor-grab", "select-none", "touch-none");

    const draftLane = screen.getByRole("region", { name: "Draft" });
    const reviewLane = screen.getByRole("region", { name: "Review" });
    const foreignLane = document.createElement("section");
    foreignLane.dataset.salesCampaignId = "another-campaign";
    foreignLane.dataset.salesCampaignStage = "draft";
    let hitElement: Element | null = draftLane;
    mockElementFromPoint(() => hitElement);

    const rightClickPointer = { handle, pointerId: 10, x: 10, y: 10 };
    fireEvent.pointerDown(handle, {
      button: 2,
      pointerId: rightClickPointer.pointerId,
      clientX: rightClickPointer.x,
      clientY: rightClickPointer.y,
    });
    hitElement = draftLane;
    moveBoardPointer(rightClickPointer, 22, 10);
    endBoardPointer(rightClickPointer, 22, 10);
    expect(stageUpdates).toHaveLength(0);

    // A small movement stays a click and never changes the server stage.
    const clickPointer = beginBoardPointer(card, 11, 10, 10);
    moveBoardPointer(clickPointer, 12, 10);
    endBoardPointer(clickPointer, 12, 10);
    expect(stageUpdates).toHaveLength(0);

    // A visually similar lane from another campaign is outside this board.
    const foreignPointer = beginBoardPointer(card, 12, 10, 10);
    hitElement = foreignLane;
    moveBoardPointer(foreignPointer, 22, 10);
    endBoardPointer(foreignPointer, 22, 10);
    expect(stageUpdates).toHaveLength(0);

    // Ignore a second pointer and clear the owner when the original pointer cancels.
    const canceledPointer = beginBoardPointer(card, 13, 10, 10);
    const competingPointer = { handle, pointerId: 14, x: 10, y: 10 };
    hitElement = draftLane;
    fireEvent.pointerDown(handle, {
      button: 0,
      pointerId: competingPointer.pointerId,
      clientX: competingPointer.x,
      clientY: competingPointer.y,
    });
    moveBoardPointer(competingPointer, 22, 10);
    endBoardPointer(competingPointer, 22, 10);
    expect(stageUpdates).toHaveLength(0);
    cancelBoardPointer(canceledPointer);
    endBoardPointer(canceledPointer, 22, 10);
    expect(stageUpdates).toHaveLength(0);

    const lostCapturePointer = beginBoardPointer(card, 16, 10, 10);
    fireEvent.lostPointerCapture(handle, {
      pointerId: lostCapturePointer.pointerId,
      clientX: 10,
      clientY: 10,
    });
    endBoardPointer(lostCapturePointer, 22, 10);
    expect(stageUpdates).toHaveLength(0);

    // Drop uses the lane under pointerup, not the lane seen on the last move.
    const pointer = beginBoardPointer(card, 15, 10, 10);
    hitElement = draftLane;
    moveBoardPointer(pointer, 22, 10);
    expect(card).toHaveClass("opacity-50");
    expect(draftLane).toHaveClass("ring-2");
    hitElement = reviewLane;
    endBoardPointer(pointer, 22, 10);
    endBoardPointer(pointer, 22, 10);

    await waitFor(() => expect(stageUpdates).toHaveLength(1));
    expect(stageUpdates[0]).toEqual({
      stage: "review",
      expected_target_version: 2,
    });
    expect(setPointerCapture.mock.calls.map(([pointerId]) => pointerId)).toEqual([
      11,
      12,
      13,
      16,
      15,
    ]);
    expect(releasePointerCapture.mock.calls.map(([pointerId]) => pointerId)).toEqual([
      11,
      12,
      13,
      16,
      15,
    ]);
  });

  it("keeps the server stage after a rejected drag and refreshes the board", async () => {
    const data = salesCampaign([
      campaignTarget({
        id: "northstar-target",
        company_name: "Northstar",
        stage: "research",
        workflow_stage: "research",
        version: 2,
        current_draft: EXISTING_DRAFT,
      }),
    ]);
    let detailReads = 0;
    let stageUpdates = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), "http://localhost");
        if (
          init?.method === "PATCH" &&
          url.pathname.endsWith("/targets/northstar-target")
        ) {
          stageUpdates += 1;
          return new Response(
            JSON.stringify({
              type: "about:blank",
              title: "Version conflict",
              status: 409,
              code: "VERSION_MISMATCH",
              detail: "The target has changed.",
            }),
            { status: 409, headers: { "content-type": "application/json" } }
          );
        }
        const response = url.pathname === "/api/v1/sales-campaigns/campaign-1"
          ? (detailReads += 1, data)
          : [];
        return new Response(JSON.stringify({ data: response }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );
    const page = await CampaignDetailPage({
      params: Promise.resolve({ id: "campaign-1" }),
    });
    mount(page);

    const card = await screen.findByRole("article", { name: "Northstar" });
    const pointer = beginBoardPointer(card);
    finishBoardPointerOn(pointer, screen.getByRole("region", { name: "Draft" }));

    await waitFor(() => {
      expect(stageUpdates).toBe(1);
      expect(detailReads).toBeGreaterThan(1);
    });
    expect(screen.getByRole("region", { name: "Researched" })).toHaveTextContent("Northstar");
    expect(screen.getByRole("region", { name: "Draft" })).not.toHaveTextContent("Northstar");
    expect(screen.getByRole("alert")).toHaveTextContent(
      "The server did not confirm this stage change."
    );
  });

  it("shows the frozen recipient HTML in a sandbox and hides immediate send", async () => {
    const data = salesCampaign([
      campaignTarget({
        id: "northstar-target",
        stage: "review",
        workflow_stage: "review",
        current_draft: {
          draft_id: "northstar-draft",
          subject: "Planning review",
          status: "frozen",
          version: 4,
          outbound_message_id: "outbound-1",
        },
        outbound: {
          outbound_message_id: "outbound-1",
          status: "draft",
          payload_hash: "cd".repeat(32),
          approval_status: "pending",
          approval_expires_at: null,
          approved_at: null,
          provider_accepted: false,
          accepted_at: null,
          failure_code: null,
          open_count: 0,
          click_count: 0,
          known_reply_count: 0,
          last_reply_at: null,
        },
      }),
    ]);
    const draft = {
      draft_id: "northstar-draft",
      sales_campaign_id: "campaign-1",
      sales_target_id: "northstar-target",
      outbound_message_id: "outbound-1",
      mail_account_id: "mailbox-1",
      owner_principal_id: "owner-1",
      visibility: "mailbox",
      thread_id: null,
      reply_to_message_id: null,
      expected_last_inbound_id: null,
      to_email_address_id: "email-1",
      template_version_id: "template-version-1",
      purpose: "marketing",
      subject: "Planning review",
      body_text: "Pick a time for our planning call.",
      body_html: "<p>Pick a time for our planning call.</p>",
      variables: {},
      status: "frozen",
      version: 4,
      created_at: "2026-10-10T00:00:00Z",
      updated_at: "2026-10-10T00:00:00Z",
    };
    const outbound = {
      outbound_message_id: "outbound-1",
      status: "draft",
      failure_code: null,
      provider_accepted: null,
      delivery_status: "unknown",
      thread_id: null,
      approval_status: "pending",
      approval_valid: false,
      approval_snapshot_matches: true,
      approval_expires_at: null,
      engagement: {
        tracking_enabled: false,
        open_request_count: 0,
        click_request_count: 0,
      },
      known_replies: [],
      automatic_resubmit_allowed: false,
      next_action: "none",
      purpose: "marketing",
      from_address: "sales@example.test",
      to_address: "mina@northstar.example",
      subject: "Planning review",
      body_text: "Pick a time for our planning call.",
      body_html:
        '<p>Pick a time for our planning call.</p><a href="https://book.example/slot">Choose a time</a>',
      not_before: null,
      accepted_at: null,
      version: 5,
      created_at: "2026-10-10T00:00:00Z",
      payload_hash: "cd".repeat(32),
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = new URL(String(input), "http://localhost");
        let responseData: unknown = [];
        if (url.pathname === "/api/v1/sales-campaigns/campaign-1") {
          responseData = data;
        } else if (url.pathname === "/api/v1/message-drafts/northstar-draft") {
          responseData = draft;
        } else if (url.pathname === "/api/v1/outbound-messages/outbound-1") {
          responseData = outbound;
        } else if (url.pathname === "/api/v1/me") {
          responseData = { principal_id: "owner-1", kind: "human" };
        } else if (url.pathname === "/api/v1/mail-accounts") {
          responseData = [
            {
              id: "mailbox-1",
              primary_address: "sales@example.test",
              status: "active",
              allowed_actions: ["mail.read", "mail.compose", "mail.send"],
            },
          ];
        } else if (url.pathname === "/api/v1/delivery-channels") {
          responseData = [
            {
              id: "channel-1",
              mail_account_id: "mailbox-1",
              sender_identity_id: "sender-1",
              purpose: "marketing",
              name: "Campaign channel",
              enabled: true,
              policy_status: "allowed",
            },
          ];
        } else if (url.pathname === "/api/v1/meeting-types") {
          responseData = [
            {
              id: "meeting-1",
              host_principal_id: "owner-1",
              calendar_connection_id: "calendar-1",
              name: "Planning call",
              duration_minutes: 30,
              status: "active",
            },
          ];
        }
        return new Response(JSON.stringify({ data: responseData }), {
          status: 200,
          headers: { "content-type": "application/json", etag: '"4"' },
        });
      })
    );
    const page = await CampaignDetailPage({
      params: Promise.resolve({ id: "campaign-1" }),
    });
    mount(page);

    fireEvent.click(
      await screen.findByRole("button", { name: "Open Northstar details" })
    );
    const dialog = await screen.findByRole("dialog");
    const preview = await within(dialog).findByTitle("Frozen recipient preview");

    expect(preview).toHaveAttribute("sandbox", "");
    expect(preview.getAttribute("srcdoc")).toContain("https://book.example/slot");
    expect(preview.getAttribute("srcdoc")).not.toContain("{{meeting.booking_url}}");
    expect(
      within(dialog).queryByRole("button", { name: "Send message" })
    ).toBeNull();
  });

  it("keeps accepted follow-up creation limited to human reviewers", async () => {
    const campaign = salesCampaign([]).campaign;
    const target = campaignTarget({
      id: "accepted-target",
      stage: "accepted",
      workflow_stage: "review",
      current_draft: {
        draft_id: "accepted-draft",
        subject: "Northstar planning",
        status: "frozen",
        version: 4,
        outbound_message_id: "accepted-outbound",
      },
      outbound: {
        outbound_message_id: "accepted-outbound",
        status: "accepted",
        payload_hash: "fa".repeat(32),
        approval_status: "approved",
        approval_expires_at: null,
        approved_at: "2026-10-10T00:00:00Z",
        provider_accepted: true,
        accepted_at: "2026-10-10T12:05:00Z",
        failure_code: null,
        open_count: 0,
        click_count: 0,
        known_reply_count: 0,
        last_reply_at: null,
        not_before: "2026-10-10T12:00:00Z",
      },
    });
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = new URL(String(input), "http://localhost");
        let responseData: unknown = [];
        if (url.pathname === "/api/v1/me") {
          responseData = { principal_id: "agent-1", kind: "agent" };
        } else if (url.pathname === "/api/v1/message-drafts/accepted-draft") {
          responseData = {
            draft_id: "accepted-draft",
            sales_campaign_id: "campaign-1",
            sales_target_id: "accepted-target",
            outbound_message_id: "accepted-outbound",
            mail_account_id: "mailbox-1",
            owner_principal_id: "owner-1",
            visibility: "mailbox",
            thread_id: null,
            reply_to_message_id: null,
            expected_last_inbound_id: null,
            to_email_address_id: "email-1",
            template_version_id: "template-version-3",
            purpose: "marketing",
            subject: "Northstar planning",
            body_text: "Accepted text",
            body_html: "<p>Accepted text</p>",
            variables: {},
            status: "frozen",
            version: 4,
            created_at: "2026-10-10T00:00:00Z",
            updated_at: "2026-10-10T00:00:00Z",
          };
        } else if (url.pathname === "/api/v1/outbound-messages/accepted-outbound") {
          responseData = null;
        }
        return new Response(JSON.stringify({ data: responseData }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );

    mount(
      <CampaignTargetDetailDialog
        campaign={campaign}
        target={target}
        queueNotice={null}
        open
        onOpenChange={() => undefined}
        onChanged={() => undefined}
      />
    );

    expect(await screen.findByText(/Only a human reviewer can start a follow-up/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Write follow-up draft" })).toBeNull();
  });

  it("creates an explicit human follow-up draft from an accepted snapshot without queuing or sending", async () => {
    const data = salesCampaign([
      campaignTarget({
        id: "accepted-target",
        stage: "accepted",
        workflow_stage: "review",
        version: 8,
        current_draft: {
          draft_id: "accepted-draft",
          subject: "Northstar planning",
          status: "frozen",
          version: 4,
          outbound_message_id: "accepted-outbound",
        },
        outbound: {
          outbound_message_id: "accepted-outbound",
          status: "accepted",
          payload_hash: "fa".repeat(32),
          approval_status: "approved",
          approval_expires_at: null,
          approved_at: "2026-10-10T00:00:00Z",
          provider_accepted: true,
          accepted_at: "2026-10-10T12:05:00Z",
          failure_code: null,
          open_count: 0,
          click_count: 0,
          known_reply_count: 0,
          last_reply_at: null,
          not_before: "2026-10-10T12:00:00Z",
        },
      }),
    ]);
    const acceptedDraft = {
      draft_id: "accepted-draft",
      sales_campaign_id: "campaign-1",
      sales_target_id: "accepted-target",
      outbound_message_id: "accepted-outbound",
      mail_account_id: "mailbox-1",
      owner_principal_id: "owner-1",
      visibility: "mailbox",
      thread_id: null,
      reply_to_message_id: null,
      expected_last_inbound_id: null,
      to_email_address_id: "email-1",
      template_version_id: "template-version-3",
      purpose: "marketing",
      subject: "Northstar planning",
      body_text: "I saw Northstar's Toronto opening.",
      body_html: "<p>I saw Northstar's Toronto opening.</p>",
      variables: { company: "Northstar" },
      status: "frozen",
      version: 4,
      created_at: "2026-10-10T00:00:00Z",
      updated_at: "2026-10-10T00:00:00Z",
    };
    const followUpDraft = {
      ...acceptedDraft,
      draft_id: "follow-up-draft",
      outbound_message_id: null,
      status: "draft",
      version: 1,
    };
    const acceptedOutbound = {
      outbound_message_id: "accepted-outbound",
      status: "accepted",
      failure_code: null,
      provider_accepted: true,
      delivery_status: "accepted",
      thread_id: null,
      approval_status: "approved",
      approval_valid: true,
      approval_snapshot_matches: true,
      approval_expires_at: null,
      engagement: { tracking_enabled: false, open_request_count: 0, click_request_count: 0 },
      known_replies: [],
      automatic_resubmit_allowed: false,
      next_action: "none",
      purpose: "marketing",
      from_address: "sales@example.test",
      to_address: "mina@northstar.example",
      subject: acceptedDraft.subject,
      body_text: acceptedDraft.body_text,
      body_html: acceptedDraft.body_html,
      not_before: "2026-10-10T12:00:00Z",
      accepted_at: "2026-10-10T12:05:00Z",
      version: 5,
      created_at: "2026-10-10T00:00:00Z",
      payload_hash: "fa".repeat(32),
    };
    const requests: { path: string; method: string; body: unknown }[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), "http://localhost");
        const method = init?.method ?? "GET";
        const body = init?.body ? JSON.parse(String(init.body)) : null;
        requests.push({ path: url.pathname, method, body });
        let responseData: unknown = [];
        if (url.pathname === "/api/v1/sales-campaigns/campaign-1") {
          responseData = data;
        } else if (url.pathname === "/api/v1/message-drafts/accepted-draft") {
          responseData = acceptedDraft;
        } else if (url.pathname === "/api/v1/message-drafts/follow-up-draft") {
          responseData = followUpDraft;
        } else if (url.pathname === "/api/v1/outbound-messages/accepted-outbound") {
          responseData = acceptedOutbound;
        } else if (url.pathname === "/api/v1/me") {
          responseData = { principal_id: "owner-1", kind: "human" };
        } else if (url.pathname === "/api/v1/mail-accounts") {
          responseData = [{
            id: "mailbox-1",
            primary_address: "sales@example.test",
            status: "active",
            allowed_actions: ["mail.read", "mail.compose", "mail.send"],
          }];
        } else if (url.pathname === "/api/v1/delivery-channels") {
          responseData = [{
            id: "channel-1",
            mail_account_id: "mailbox-1",
            sender_identity_id: "sender-1",
            purpose: "marketing",
            name: "Campaign channel",
            enabled: true,
            policy_status: "allowed",
          }];
        } else if (url.pathname === "/api/v1/meeting-types") {
          responseData = [{
            id: "meeting-1",
            host_principal_id: "owner-1",
            calendar_connection_id: "calendar-1",
            name: "Planning call",
            duration_minutes: 30,
            status: "active",
          }];
        } else if (
          method === "POST" &&
          url.pathname === "/api/v1/sales-campaigns/campaign-1/targets/accepted-target/drafts"
        ) {
          data.targets[0]!.current_draft = {
            draft_id: "follow-up-draft",
            subject: acceptedDraft.subject,
            status: "draft",
            version: 1,
            outbound_message_id: null,
          };
          data.targets[0]!.stage = "draft";
          data.targets[0]!.workflow_stage = "draft";
          data.targets[0]!.version = 9;
          data.targets[0]!.outbound = null;
          responseData = followUpDraft;
        }
        return new Response(JSON.stringify({ data: responseData }), {
          status: method === "POST" ? 201 : 200,
          headers: { "content-type": "application/json", etag: '"4"' },
        });
      })
    );
    const page = await CampaignDetailPage({
      params: Promise.resolve({ id: "campaign-1" }),
    });
    mount(page);

    fireEvent.click(
      await screen.findByRole("button", { name: "Open Northstar details" })
    );
    const dialog = await screen.findByRole("dialog");
    const frozenPreview = await within(dialog).findByTitle("Frozen recipient preview");
    expect(frozenPreview).toHaveAttribute("sandbox", "");
    expect(frozenPreview.getAttribute("srcdoc")).toContain("Northstar's Toronto opening");
    expect(await within(dialog).findByText(/A follow-up starts as a separate draft/)).toBeInTheDocument();
    fireEvent.click(
      await within(dialog).findByRole("button", { name: "Write follow-up draft" })
    );

    await waitFor(() => {
      const request = requests.find((row) =>
        row.path.endsWith("/targets/accepted-target/drafts")
      );
      expect(request?.body).toMatchObject({
        subject: "Northstar planning",
        body_text: "I saw Northstar's Toronto opening.",
        template_version_id: "template-version-3",
        explicit_follow_up: true,
      });
    });
    expect(await within(dialog).findByLabelText("Subject")).toHaveValue(
      "Northstar planning"
    );
    expect(data.targets[0]!.stage).toBe("draft");
    expect(requests.some((row) => row.path.endsWith("/queue"))).toBe(false);
    expect(requests.some((row) => row.path.endsWith("/submit"))).toBe(false);
    expect(within(dialog).getByRole("button", { name: "Save changes" })).toBeEnabled();
  });

  it("creates a separate editable copy of a frozen Review snapshot", async () => {
    const data = salesCampaign([
      campaignTarget({
        id: "review-target",
        stage: "review",
        workflow_stage: "review",
        version: 7,
        current_draft: {
          draft_id: "review-draft",
          subject: "Northstar planning",
          status: "frozen",
          version: 4,
          outbound_message_id: "outbound-review",
        },
        outbound: {
          outbound_message_id: "outbound-review",
          status: "draft",
          payload_hash: "de".repeat(32),
          approval_status: "pending",
          approval_expires_at: null,
          approved_at: null,
          provider_accepted: false,
          accepted_at: null,
          failure_code: null,
          open_count: 0,
          click_count: 0,
          known_reply_count: 0,
          last_reply_at: null,
        },
      }),
    ]);
    const frozenDraft = {
      draft_id: "review-draft",
      sales_campaign_id: "campaign-1",
      sales_target_id: "review-target",
      outbound_message_id: "outbound-review",
      mail_account_id: "mailbox-1",
      owner_principal_id: "owner-1",
      visibility: "mailbox",
      thread_id: null,
      reply_to_message_id: null,
      expected_last_inbound_id: null,
      to_email_address_id: "email-1",
      template_version_id: "template-version-3",
      purpose: "marketing",
      subject: "Northstar planning",
      body_text: "I saw Northstar's Toronto opening.",
      body_html: "<p>I saw Northstar's Toronto opening.</p>",
      variables: { company: "Northstar" },
      status: "frozen",
      version: 4,
      created_at: "2026-10-10T00:00:00Z",
      updated_at: "2026-10-10T00:00:00Z",
    };
    const copiedDraft = {
      ...frozenDraft,
      draft_id: "review-copy",
      outbound_message_id: null,
      status: "draft",
      version: 1,
    };
    const requests: { path: string; method: string; body: unknown }[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), "http://localhost");
        const method = init?.method ?? "GET";
        const body = init?.body ? JSON.parse(String(init.body)) : null;
        requests.push({ path: url.pathname, method, body });
        let responseData: unknown = [];
        if (url.pathname === "/api/v1/sales-campaigns/campaign-1") {
          responseData = data;
        } else if (url.pathname === "/api/v1/message-drafts/review-draft") {
          responseData = frozenDraft;
        } else if (url.pathname === "/api/v1/message-drafts/review-copy") {
          responseData = copiedDraft;
        } else if (
          method === "POST" &&
          url.pathname === "/api/v1/sales-campaigns/campaign-1/targets/review-target/drafts"
        ) {
          data.targets[0]!.current_draft = {
            draft_id: "review-copy",
            subject: frozenDraft.subject,
            status: "draft",
            version: 1,
            outbound_message_id: null,
          };
          data.targets[0]!.stage = "draft";
          data.targets[0]!.workflow_stage = "draft";
          data.targets[0]!.version = 8;
          data.targets[0]!.outbound = null;
          responseData = copiedDraft;
        } else if (url.pathname === "/api/v1/me") {
          responseData = { principal_id: "owner-1", kind: "human" };
        } else if (url.pathname === "/api/v1/mail-accounts") {
          responseData = [{
            id: "mailbox-1",
            primary_address: "sales@example.test",
            status: "active",
            allowed_actions: ["mail.read", "mail.compose", "mail.send"],
          }];
        } else if (url.pathname === "/api/v1/delivery-channels") {
          responseData = [{
            id: "channel-1",
            mail_account_id: "mailbox-1",
            sender_identity_id: "sender-1",
            purpose: "marketing",
            name: "Campaign channel",
            enabled: true,
            policy_status: "allowed",
          }];
        } else if (url.pathname === "/api/v1/meeting-types") {
          responseData = [];
        } else if (url.pathname === "/api/v1/outbound-messages/outbound-review") {
          responseData = null;
        }
        return new Response(JSON.stringify({ data: responseData }), {
          status: method === "POST" ? 201 : 200,
          headers: { "content-type": "application/json", etag: '"4"' },
        });
      })
    );
    const page = await CampaignDetailPage({
      params: Promise.resolve({ id: "campaign-1" }),
    });
    mount(page);

    const frozenCard = await screen.findByRole("article", { name: "Northstar" });
    expect(
      within(frozenCard).queryByRole("button", { name: "Move Northstar back to Draft" })
    ).toBeNull();
    const pointer = beginBoardPointer(frozenCard);
    finishBoardPointerOn(pointer, screen.getByRole("region", { name: "Draft" }));
    const dialog = await screen.findByRole("dialog");
    expect(await within(dialog).findByText(/A frozen review cannot move backward/)).toBeInTheDocument();
    expect(requests.some((request) => request.method === "PATCH")).toBe(false);
    fireEvent.click(
      await within(dialog).findByRole("button", { name: "Create editable copy" })
    );

    expect(await within(dialog).findByLabelText("Subject")).toHaveValue(
      "Northstar planning"
    );
    await waitFor(() => {
      expect(data.targets[0]!.stage).toBe("draft");
    });
    const createRequest = requests.find((request) =>
      request.path.endsWith("/targets/review-target/drafts")
    );
    expect(createRequest?.body).toEqual({
      subject: "Northstar planning",
      body_text: "I saw Northstar's Toronto opening.",
      body_html: "<p>I saw Northstar's Toronto opening.</p>",
      template_version_id: "template-version-3",
      variables: { company: "Northstar" },
    });
    expect(
      within(dialog).queryByRole("button", { name: "Create editable copy" })
    ).toBeNull();
  });

  it("queues a frozen card on drag without a separate approval", async () => {
    const data = salesCampaign([
      campaignTarget({
        id: "northstar-target",
        stage: "review",
        workflow_stage: "review",
        current_draft: {
          draft_id: "northstar-draft",
          subject: "Planning review",
          status: "frozen",
          version: 4,
          outbound_message_id: "outbound-1",
        },
        outbound: {
          outbound_message_id: "outbound-1",
          status: "draft",
          payload_hash: "ab".repeat(32),
          approval_status: "pending",
          approval_expires_at: null,
          approved_at: null,
          provider_accepted: false,
          accepted_at: null,
          failure_code: null,
          open_count: 0,
          click_count: 0,
          known_reply_count: 0,
          last_reply_at: null,
        },
      }),
    ]);
    const calls: { url: string; method: string }[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), "http://localhost");
        calls.push({ url: url.pathname, method: init?.method ?? "GET" });
        const response =
          url.pathname === "/api/v1/sales-campaigns/campaign-1"
            ? data
            : [];
        return new Response(JSON.stringify({ data: response }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );
    const page = await CampaignDetailPage({
      params: Promise.resolve({ id: "campaign-1" }),
    });
    mount(page);

    const card = await screen.findByRole("article", { name: "Northstar" });
    const pointer = beginBoardPointer(card);
    finishBoardPointerOn(pointer, screen.getByRole("region", { name: "Queued" }));

    await waitFor(() =>
      expect(
        calls.some(
          (call) => call.url.endsWith("/queue") && call.method === "POST"
        )
      ).toBe(true)
    );
    expect(screen.queryByText(/Approve the frozen message before scheduling/i)).toBeNull();
  });

  it("freezes an unfrozen draft automatically and then queues the card", async () => {
    const data = salesCampaign([
      campaignTarget({
        id: "northstar-target",
        stage: "review",
        workflow_stage: "review",
        version: 3,
        current_draft: { ...EXISTING_DRAFT, draft_id: "northstar-draft" },
      }),
    ]);
    const calls: { url: string; method: string; body?: unknown }[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), "http://localhost");
        const method = init?.method ?? "GET";
        calls.push({
          url: url.pathname,
          method,
          body: init?.body ? JSON.parse(String(init.body)) : undefined,
        });
        let payload: unknown = [];
        if (url.pathname === "/api/v1/message-drafts/northstar-draft") {
          payload = { draft_id: "northstar-draft", version: 1, status: "draft" };
        } else if (url.pathname.endsWith("/freeze")) {
          payload = {
            outbound_message_id: "outbound-1",
            status: "draft",
            payload_hash: "cd".repeat(32),
            version: 1,
          };
          data.targets[0]!.current_draft = {
            ...EXISTING_DRAFT,
            draft_id: "northstar-draft",
            status: "frozen",
            outbound_message_id: "outbound-1",
          };
          data.targets[0]!.outbound = {
            outbound_message_id: "outbound-1",
            status: "draft",
            payload_hash: "cd".repeat(32),
            approval_status: null,
            approval_expires_at: null,
            approved_at: null,
            provider_accepted: false,
            accepted_at: null,
            failure_code: null,
            open_count: 0,
            click_count: 0,
            known_reply_count: 0,
            last_reply_at: null,
          } as never;
          data.targets[0]!.version = 4;
        } else if (url.pathname.endsWith("/queue")) {
          payload = {
            submission: { status: "queued", job_id: "job-1", provider_accepted: false },
            batch_at: "2026-10-11T05:10:00Z",
          };
        } else if (url.pathname === "/api/v1/sales-campaigns/campaign-1") {
          payload = data;
        }
        return new Response(JSON.stringify({ data: payload }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );
    mount(await CampaignDetailPage({ params: Promise.resolve({ id: "campaign-1" }) }));

    const card = await screen.findByRole("article", { name: "Northstar" });
    finishBoardPointerOn(beginBoardPointer(card), screen.getByRole("region", { name: "Queued" }));

    await waitFor(() =>
      expect(calls.some((c) => c.url.endsWith("/queue") && c.method === "POST")).toBe(true)
    );
    const freezeIndex = calls.findIndex((c) => c.url.endsWith("/freeze") && c.method === "POST");
    const queueIndex = calls.findIndex((c) => c.url.endsWith("/queue") && c.method === "POST");
    expect(freezeIndex).toBeGreaterThanOrEqual(0);
    expect(freezeIndex).toBeLessThan(queueIndex);
    expect(calls[freezeIndex]!.body).toMatchObject({
      delivery_channel_id: "channel-1",
      tracking_requested: false,
    });
    expect(calls[queueIndex]!.body).toMatchObject({
      outbound_message_id: "outbound-1",
      expected_payload_hash: "cd".repeat(32),
      expected_target_version: 4,
    });
  });

  it("queues an approved drag with the frozen hash and target version", async () => {
    const data = salesCampaign([
      campaignTarget({
        id: "northstar-target",
        stage: "review",
        workflow_stage: "review",
        version: 9,
        current_draft: {
          draft_id: "northstar-draft",
          subject: "Planning review",
          status: "frozen",
          version: 4,
          outbound_message_id: "outbound-1",
        },
        outbound: {
          outbound_message_id: "outbound-1",
          status: "draft",
          payload_hash: "cd".repeat(32),
          approval_status: "approved",
          approval_expires_at: null,
          approved_at: "2026-10-10T00:00:00Z",
          provider_accepted: false,
          accepted_at: null,
          failure_code: null,
          open_count: 0,
          click_count: 0,
          known_reply_count: 0,
          last_reply_at: null,
        },
      }),
    ]);
    const calls: { url: string; init: RequestInit }[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), "http://localhost");
        calls.push({ url: url.pathname, init: init ?? {} });
        if (url.pathname.endsWith("/targets/northstar-target/queue")) {
          data.targets[0]!.stage = "queued";
          data.targets[0]!.outbound!.status = "queued";
          return new Response(
            JSON.stringify({
              data: {
                submission: {
                  outbound_message_id: "outbound-1",
                  status: "queued",
                  job_id: "job-1",
                  provider_accepted: false,
                },
                batch_at: "2026-10-10T12:10:00Z",
              },
            }),
            { status: 202, headers: { "content-type": "application/json" } }
          );
        }
        const response =
          url.pathname === "/api/v1/sales-campaigns/campaign-1"
            ? data
            : [];
        return new Response(JSON.stringify({ data: response }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      })
    );
    const page = await CampaignDetailPage({
      params: Promise.resolve({ id: "campaign-1" }),
    });
    mount(page);

    const card = await screen.findByRole("article", { name: "Northstar" });
    const pointer = beginBoardPointer(card);
    const queueLane = screen.getByRole("region", { name: "Queued" });
    finishBoardPointerOn(pointer, queueLane);
    endBoardPointer(pointer, pointer.x + 12, pointer.y);

    await waitFor(() => {
      expect(
        calls.some((call) => call.url.endsWith("/targets/northstar-target/queue"))
      ).toBe(true);
    });
    const queueCalls = calls.filter((call) =>
      call.url.endsWith("/targets/northstar-target/queue")
    );
    expect(queueCalls).toHaveLength(1);
    const queueCall = queueCalls[0]!;
    expect(queueCall.init.method).toBe("POST");
    expect(JSON.parse(String(queueCall.init.body))).toEqual({
      outbound_message_id: "outbound-1",
      expected_payload_hash: "cd".repeat(32),
      expected_target_version: 9,
    });
  });
});
