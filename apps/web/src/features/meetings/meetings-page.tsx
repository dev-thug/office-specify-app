"use client";

import { useLocale, useTranslations } from "next-intl";
import { useMemo, useState } from "react";
import { CalendarPlusIcon } from "lucide-react";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  BOOKINGS_PATH,
  MEETING_TYPES_LIST_LIMIT,
  PRINCIPALS_PATH,
  principalDisplayName,
  type Booking,
  type BookingStatus,
  type PrincipalRef,
} from "@/lib/api/meetings";
import { useCursorList } from "@/lib/api/use-cursor-list";
import {
  bookingNeedsInvestigation,
} from "@/features/meetings/booking-actions";
import { BookingDetailSheet } from "@/features/meetings/booking-detail-sheet";
import { BookingLinkDialog } from "@/features/meetings/booking-link-dialog";
import { MeetingsCalendar } from "@/features/meetings/meetings-calendar";
import {
  detectTimezone,
  formatDateTime,
  formatTime,
  localDayStartUtc,
  shiftLocalDate,
  todayInZone,
} from "@/features/meetings/datetime";

const BOOKING_STATUSES: BookingStatus[] = [
  "held",
  "confirming",
  "confirmed",
  "canceling",
  "canceled",
  "failed",
  "unknown",
  "expired",
];

type RangePreset = "upcoming" | "past" | "all";

/**
 * 목록 뷰 한 행 — 외부 sync·참석 결과·상태를 §7.3 규칙으로 표시한다.
 * confirming/unknown 은 절대 confirmed 로 렌더하지 않는다 (StatusBadge 가 보장).
 */
export function MeetingsPage() {
  const t = useTranslations("meetings");
  const tCommon = useTranslations("common");
  const tStatus = useTranslations("status");
  const locale = useLocale();

  const [status, setStatus] = useState<BookingStatus | "all">("all");
  const [hostId, setHostId] = useState<string>("all");
  const [preset, setPreset] = useState<RangePreset>("upcoming");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [linkOpen, setLinkOpen] = useState(false);

  const displayTz = useMemo(() => detectTimezone(), []);

  // 기간 프리셋 → UTC 범위 (표시 시간대의 자정 경계를 ISO로 환산).
  const range = useMemo(() => {
    if (preset === "all") return { from: undefined, to: undefined };
    const today = todayInZone(displayTz);
    if (preset === "upcoming") {
      const end = shiftLocalDate(today, 31);
      return {
        from: new Date(localDayStartUtc(today, displayTz)).toISOString(),
        to: new Date(localDayStartUtc(end, displayTz)).toISOString(),
      };
    }
    const start = shiftLocalDate(today, -30);
    const end = shiftLocalDate(today, 1);
    return {
      from: new Date(localDayStartUtc(start, displayTz)).toISOString(),
      to: new Date(localDayStartUtc(end, displayTz)).toISOString(),
    };
  }, [preset, displayTz]);

  const bookingsQuery = useCursorList<Booking>({
    queryKey: ["meetings", "bookings", { preset, range }],
    path: BOOKINGS_PATH,
    params: {
      from: range.from,
      to: range.to,
    },
  });

  const principalsQuery = useCursorList<PrincipalRef>({
    queryKey: ["meetings", "principals"],
    path: PRINCIPALS_PATH,
    limit: MEETING_TYPES_LIST_LIMIT,
    staleTime: 60_000,
  });

  const allBookings = useMemo(
    () => bookingsQuery.data?.pages.flatMap((p) => p.data) ?? [],
    [bookingsQuery.data]
  );
  // The REST contract currently filters only by time range; status and host remain local.
  const bookings = useMemo(
    () => allBookings.filter((booking) =>
      (status === "all" || booking.status === status) &&
      (hostId === "all" || booking.host_principal_id === hostId)
    ),
    [allBookings, status, hostId]
  );
  const principals = useMemo(
    () => principalsQuery.data?.pages.flatMap((p) => p.data) ?? [],
    [principalsQuery.data]
  );
  const principalName = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of principals) {
      const name = principalDisplayName(p);
      if (name) map.set(p.id, name);
    }
    return (id: string | undefined) =>
      (id && map.get(id)) || (id ? tCommon("nameUnavailable") : "—");
  }, [principals, tCommon]);

  const selectedBooking = allBookings.find((row) => row.id === selectedId);
  const selectedHostName = selectedBooking?.host_display_name ?? principalName(selectedBooking?.host_principal_id);

  const columns: ColumnDef<Booking>[] = [
    {
      key: "time",
      header: t("list.time"),
      cell: (b) => (
        <div className="flex flex-col">
          <span className="font-medium">
            {formatDateTime(b.starts_at, { tz: displayTz, locale })}–
            {formatTime(b.ends_at, { tz: displayTz, locale })}
          </span>
          {b.guest_timezone && b.guest_timezone !== displayTz ? (
            <span className="text-2xs text-muted-foreground">
              {t("calendar.guestTz", { tz: b.guest_timezone })}
            </span>
          ) : null}
        </div>
      ),
    },
    {
      key: "guest",
      header: t("list.guest"),
      cell: (b) => (
        <div className="flex flex-col">
          <span>{b.guest_name}</span>
          {b.guest_email ? (
            <span className="text-2xs text-muted-foreground">{b.guest_email}</span>
          ) : null}
        </div>
      ),
    },
    {
      key: "host",
      header: t("list.host"),
      cell: (b) => b.host_display_name ?? principalName(b.host_principal_id),
    },
    {
      key: "status",
      header: t("list.status"),
      cell: (b) => <StatusBadge status={b.status} />,
    },
    {
      key: "sync",
      header: t("list.sync"),
      cell: (b) => {
        if (bookingNeedsInvestigation(b.status)) {
          return <Badge variant="amber-subtle">{t("sync.unknown")}</Badge>;
        }
        if (b.provider_event_id) {
          return <Badge variant="teal-subtle">{t("sync.synced")}</Badge>;
        }
        return <Badge variant="secondary">{t("sync.pending")}</Badge>;
      },
    },
    {
      key: "outcome",
      header: t("list.outcome"),
      cell: (booking) => booking.attendance_status ? (
        <StatusBadge
          status={booking.attendance_status === "attended" ? "done" : "canceled"}
          label={t(`outcome.${booking.attendance_status}`)}
        />
      ) : <span className="text-muted-foreground">{t("outcome.none")}</span>,
    },
    {
      key: "actions",
      header: t("list.actions"),
      className: "w-16 text-right",
      cell: (b) => (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          onClick={() => setSelectedId(b.id)}
        >
          {t("list.open")}
        </Button>
      ),
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader
        title={t("page.title")}
        description={t("page.description")}
        actions={
          <Button
            type="button"
            size="sm"
            onClick={() => setLinkOpen(true)}
          >
            <CalendarPlusIcon className="size-4" aria-hidden />
            {t("actions.issueLink")}
          </Button>
        }
      />

      <div className="flex flex-wrap items-end gap-3">
        <div className="grid gap-1.5">
          <span className="text-2xs text-muted-foreground">
            {t("filters.rangePreset")}
          </span>
          <Select
            value={preset}
            onValueChange={(v) => setPreset(v as RangePreset)}
          >
            <SelectTrigger size="sm" aria-label={t("filters.rangePreset")}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="upcoming">{t("filters.rangeUpcoming")}</SelectItem>
              <SelectItem value="past">{t("filters.rangePast")}</SelectItem>
              <SelectItem value="all">{t("filters.rangeAll")}</SelectItem>
            </SelectContent>
          </Select>
        </div>
        <div className="grid gap-1.5">
          <span className="text-2xs text-muted-foreground">
            {t("filters.status")}
          </span>
          <Select
            value={status}
            onValueChange={(v) => setStatus(v as BookingStatus | "all")}
          >
            <SelectTrigger size="sm" aria-label={t("filters.status")}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">{t("filters.statusAll")}</SelectItem>
              {BOOKING_STATUSES.map((s) => (
                <SelectItem key={s} value={s}>
                  {tStatus.has(s) ? tStatus(s) : s}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="grid gap-1.5">
          <span className="text-2xs text-muted-foreground">
            {t("filters.host")}
          </span>
          <Select value={hostId} onValueChange={setHostId}>
            <SelectTrigger size="sm" aria-label={t("filters.host")}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">{t("filters.hostAll")}</SelectItem>
              {principals.map((p) => (
                <SelectItem key={p.id} value={p.id}>
                  {principalDisplayName(p) ?? tCommon("nameUnavailable")}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <p className="ml-auto text-2xs text-muted-foreground">
          {t("filters.timezone", { tz: displayTz })}
        </p>
      </div>

      <Tabs defaultValue="list">
        <TabsList>
          <TabsTrigger value="list">{t("tabs.list")}</TabsTrigger>
          <TabsTrigger value="calendar">{t("tabs.calendar")}</TabsTrigger>
        </TabsList>
        <TabsContent value="list">
          <DataTable<Booking>
            columns={columns}
            rows={bookings}
            rowKey={(b) => b.id}
            loading={bookingsQuery.isPending}
            error={bookingsQuery.error}
            onRetry={() => bookingsQuery.refetch()}
            page={
              bookingsQuery.data?.pages.at(-1)?.page
                ? {
                    next_cursor:
                      bookingsQuery.data.pages.at(-1)!.page!.next_cursor,
                    has_more: bookingsQuery.data.pages.at(-1)!.page!.has_more,
                  }
                : undefined
            }
            onLoadMore={() => bookingsQuery.fetchNextPage()}
            loadingMore={bookingsQuery.isFetchingNextPage}
            caption={t("list.caption")}
            emptyState={
              <EmptyState
                title={t("list.emptyTitle")}
                description={t("list.emptyDescription")}
              />
            }
          />
        </TabsContent>
        <TabsContent value="calendar">
          <MeetingsCalendar
            bookings={bookings}
            timezone={displayTz}
            loading={bookingsQuery.isPending}
            error={bookingsQuery.error}
            onRetry={() => bookingsQuery.refetch()}
            onOpen={(id) => setSelectedId(id)}
          />
        </TabsContent>
      </Tabs>

      <BookingDetailSheet
        bookingId={selectedId}
        hostName={selectedHostName}
        onOpenChange={(open) => !open && setSelectedId(null)}
      />
      <BookingLinkDialog open={linkOpen} onOpenChange={setLinkOpen} />
    </div>
  );
}
