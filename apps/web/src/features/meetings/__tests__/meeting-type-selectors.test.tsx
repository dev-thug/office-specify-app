import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { afterEach, describe, expect, it, vi } from "vitest";

import campaigns from "../../../../messages/en/campaigns.json";
import common from "../../../../messages/en/common.json";
import meetings from "../../../../messages/en/meetings.json";
import { CampaignBuilder } from "@/features/campaigns/campaign-builder";
import { MEETING_TYPES_QUERY_KEY } from "@/lib/api/meetings";
import { BookingLinkDialog } from "../booking-link-dialog";
import { MeetingTypesScreen } from "../meeting-types-screen";

const currentPrincipalId = "e951ff7a-0000-4000-8000-000000000001";
const otherPrincipalId = "7a0a70e3-0000-4000-8000-000000000002";
const currentCalendarId = "calendar-current-user";
const otherCalendarId = "calendar-other-user";
const liveMeetingType = {
  id: "731afb54-0a92-4d4a-a624-16655c77b397",
  host_principal_id: currentPrincipalId,
  calendar_connection_id: currentCalendarId,
  name: "[테스트] Office 내부 미팅 예약",
  duration_minutes: 30,
  buffer_before_minutes: 10,
  buffer_after_minutes: 10,
  minimum_notice_minutes: 1440,
  booking_horizon_days: 30,
  status: "active",
  version: 1,
};

function jsonResponse(status: number, data: unknown, paged = false) {
  return new Response(
    JSON.stringify({
      data,
      ...(paged ? { page: { has_more: false, next_cursor: null } } : {}),
    }),
    { status, headers: { "content-type": "application/json" } }
  );
}

function createApi(initialMeetingTypes: typeof liveMeetingType[] = [], currentHumanFirst = false) {
  let meetingTypes = [...initialMeetingTypes];
  let createRequest: Record<string, unknown> | undefined;
  const principals = [
    { id: otherPrincipalId, kind: "human", status: "active", name: "Other human" },
    { id: currentPrincipalId, kind: "human", status: "active", name: "Administrator" },
  ];
  if (currentHumanFirst) principals.reverse();

  const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://office.test");
    const method = init?.method ?? "GET";

    if (url.pathname === "/api/v1/me") {
      return jsonResponse(200, {
        principal_id: currentPrincipalId,
        kind: "human",
        role: "admin",
        workspace_id: "workspace-1",
        display_name: "Administrator",
        mail_accounts: [],
      });
    }
    if (url.pathname === "/api/v1/principals") {
      return jsonResponse(200, principals, true);
    }
    if (url.pathname === "/api/v1/calendar-connections") {
      return jsonResponse(
        200,
        [
          {
            id: otherCalendarId,
            host_principal_id: otherPrincipalId,
            provider: "office_calendar",
            status: "active",
          },
          {
            id: currentCalendarId,
            host_principal_id: currentPrincipalId,
            provider: "office_calendar",
            status: "active",
          },
        ],
        true
      );
    }
    if (url.pathname === "/api/v1/meeting-types") {
      if (method === "POST") {
        createRequest = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
        const created = {
          ...liveMeetingType,
          calendar_connection_id: String(createRequest.calendar_connection_id),
          name: String(createRequest.name),
        };
        meetingTypes = [...meetingTypes, created];
        return jsonResponse(201, created);
      }
      return jsonResponse(200, meetingTypes, true);
    }
    return jsonResponse(200, [], true);
  });

  return {
    fetcher,
    getCreateRequest: () => createRequest,
    getMeetingTypes: () => meetingTypes,
  };
}

function renderMeetingTypesWorkspace(
  queryClient: QueryClient,
  includeCampaignBuilder = false
) {
  return render(
    <NextIntlClientProvider
      locale="en"
      messages={{ campaigns, common, meetings }}
    >
      <QueryClientProvider client={queryClient}>
        <>
          {includeCampaignBuilder ? <CampaignBuilder mode="create" /> : null}
          <MeetingTypesScreen />
        </>
      </QueryClientProvider>
    </NextIntlClientProvider>
  );
}

function newQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: Infinity },
      mutations: { retry: false },
    },
  });
}

async function waitForMeetingTypeSetupData(
  queryClient: QueryClient,
  includeCurrentUser = false
) {
  await waitFor(() => {
    if (includeCurrentUser) expect(queryClient.getQueryData(["me"])).toBeDefined();
    expect(queryClient.getQueryData(["meetings", "principals"])).toBeDefined();
    expect(queryClient.getQueryData(["meetings", "calendar-connections"])).toBeDefined();
  });
}

afterEach(() => vi.unstubAllGlobals());

describe("Meeting type host and selector data", () => {
  it("uses the authenticated human and resolves principal names from the API", async () => {
    const api = createApi([liveMeetingType]);
    vi.stubGlobal("fetch", api.fetcher);
    const queryClient = newQueryClient();
    renderMeetingTypesWorkspace(queryClient);
    await waitForMeetingTypeSetupData(queryClient);

    const row = await screen.findByRole("row", { name: /\[테스트\] Office 내부 미팅 예약/ });
    expect(within(row).getByText("Administrator")).toBeInTheDocument();
    await waitFor(() => expect(queryClient.getQueryData(["me"])).toBeDefined());

    fireEvent.click(screen.getByRole("button", { name: meetings.types.create }));
    const dialog = await screen.findByRole("dialog");
    const hostField = within(dialog).getByLabelText(meetings.types.fieldHost);

    expect(hostField).toHaveValue("Administrator");
    expect(hostField).toHaveAttribute("readonly");
    expect(
      within(dialog).getByRole("combobox", { name: meetings.types.fieldCalendar })
    ).toHaveTextContent(meetings.types.calendarOffice);
  });

  it("refreshes an already-mounted campaign selector after a meeting type is created", async () => {
    const api = createApi([], true);
    vi.stubGlobal("fetch", api.fetcher);
    const queryClient = newQueryClient();
    renderMeetingTypesWorkspace(queryClient, true);

    await waitFor(() => {
      expect(
        api.fetcher.mock.calls.some(([input]) =>
          new URL(String(input), "http://office.test").pathname.endsWith("/meeting-types")
        )
      ).toBe(true);
    });
    await waitForMeetingTypeSetupData(queryClient, true);
    await waitFor(() => {
      const meetingTypeQueries = queryClient
        .getQueryCache()
        .getAll()
        .filter((query) => {
          const key = query.queryKey.map(String).join("/");
          return key === "meetings/types" || key.includes("meeting-types");
        });
      expect(meetingTypeQueries.length).toBeGreaterThan(0);
      expect(meetingTypeQueries.every((query) => query.state.status === "success")).toBe(true);
    });

    fireEvent.click(screen.getByRole("button", { name: meetings.types.create }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText(meetings.types.fieldName), {
      target: { value: liveMeetingType.name },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: meetings.types.save }));

    await waitFor(() => {
      expect(api.getCreateRequest()?.calendar_connection_id).toBe(currentCalendarId);
      expect(api.getMeetingTypes()).toEqual([
        expect.objectContaining({
          id: liveMeetingType.id,
          name: liveMeetingType.name,
          host_principal_id: currentPrincipalId,
          status: "active",
        }),
      ]);
    });

    await waitFor(() => {
      const meetingTypeQueries = queryClient
        .getQueryCache()
        .getAll()
        .filter((query) => {
          const key = query.queryKey.map(String).join("/");
          return key === "meetings/types" || key.includes("meeting-types");
        });
      expect(meetingTypeQueries).toHaveLength(1);
      const pages = (
        meetingTypeQueries[0]?.state.data as
          | { pages: Array<{ data: typeof liveMeetingType[] }> }
          | undefined
      )?.pages;
      expect(pages?.flatMap((page) => page.data)).toEqual([
        expect.objectContaining({ name: liveMeetingType.name, status: "active" }),
      ]);
    });
  });

  it("does not allow issuing a link for a saved meeting type that is no longer active", async () => {
    const queryClient = newQueryClient();
    queryClient.setQueryData(MEETING_TYPES_QUERY_KEY, {
      pages: [{ data: [liveMeetingType], page: { has_more: false, next_cursor: null } }],
      pageParams: [null],
    });
    queryClient.setQueryData(["meetings", "principals"], {
      pages: [{
        data: [{ id: currentPrincipalId, kind: "human", status: "active", name: "Administrator" }],
        page: { has_more: false, next_cursor: null },
      }],
      pageParams: [null],
    });

    render(
      <NextIntlClientProvider locale="en" messages={{ common, meetings }}>
        <QueryClientProvider client={queryClient}>
          <BookingLinkDialog
            open
            onOpenChange={vi.fn()}
            presetTypeId="inactive-meeting-type"
          />
        </QueryClientProvider>
      </NextIntlClientProvider>
    );

    await waitFor(() => {
      expect(screen.getByRole("combobox", { name: meetings.link.meetingType })).toBeInTheDocument();
    });
    expect(screen.getByRole("button", { name: meetings.link.issue })).toBeDisabled();
  });
});
