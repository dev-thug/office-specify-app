"use client";

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useLocale, useTranslations } from "next-intl";
import { useState } from "react";
import { RefreshCwIcon } from "lucide-react";

import { ErrorState } from "@/components/patterns/error-state";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { getBooking, type Booking } from "@/lib/api/meetings";
import { readEtag } from "@/lib/api/mutate";
import {
  bookingCanCancel,
  bookingCanRecordAttendance,
  bookingCanReschedule,
  bookingInFlight,
  bookingNeedsInvestigation,
} from "@/features/meetings/booking-actions";
import {
  AttendanceDialog,
  CancelBookingDialog,
  RescheduleDialog,
} from "@/features/meetings/booking-dialogs";
import {
  detectTimezone,
  formatDateTime,
  formatTime,
} from "@/features/meetings/datetime";

interface BookingDetailSheetProps {
  bookingId: string | null;
  onOpenChange: (open: boolean) => void;
}

/** 예약 상세 — 상태·외부 sync·참석 결과·변경/취소 액션 (UI09). */
export function BookingDetailSheet({
  bookingId,
  onOpenChange,
}: BookingDetailSheetProps) {
  const t = useTranslations("meetings");
  const locale = useLocale();
  const queryClient = useQueryClient();
  const displayTz = detectTimezone();

  const [dialog, setDialog] = useState<"reschedule" | "cancel" | "attendance" | null>(null);

  const query = useQuery({
    queryKey: ["meetings", "booking", bookingId],
    queryFn: () => getBooking(bookingId!),
    enabled: bookingId !== null,
  });

  const booking: Booking | undefined = query.data?.data;
  // If-Match 에 넣을 버전 — ETag 헤더 우선, 없으면 version 필드.
  const etag = query.data ? readEtag(query.data) ?? booking?.version : undefined;

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: ["meetings"] });
  };

  return (
    <Sheet open={bookingId !== null} onOpenChange={onOpenChange}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-md">
        <SheetHeader>
          <SheetTitle>{t("detail.title")}</SheetTitle>
          <SheetDescription>
            {booking
              ? `${booking.guest_name} · ${formatDateTime(booking.starts_at, { tz: displayTz, locale })}`
              : "…"}
          </SheetDescription>
        </SheetHeader>

        <div className="flex flex-col gap-4 px-4 pb-6">
          {query.isPending ? (
            <div className="flex flex-col gap-2" aria-busy="true">
              <Skeleton className="h-6 w-32" />
              <Skeleton className="h-24 w-full" />
              <Skeleton className="h-24 w-full" />
            </div>
          ) : query.isError || !booking ? (
            <ErrorState
              error={query.error}
              title={t("detail.loadError")}
              onRetry={() => query.refetch()}
            />
          ) : (
            <>
              <div className="flex items-center gap-2">
                <StatusBadge status={booking.status} />
                {booking.attendance_status ? <Badge variant="info-subtle">{t(`outcome.${booking.attendance_status}`)}</Badge> : null}
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  className="ml-auto"
                  aria-label={t("actions.refresh")}
                  onClick={() => query.refetch()}
                >
                  <RefreshCwIcon className="size-3.5" aria-hidden />
                </Button>
              </div>

              {t.has(`statusNote.${booking.status}`) ? (
                <Alert variant={
                  bookingNeedsInvestigation(booking.status)
                    ? "warning"
                    : booking.status === "confirmed"
                      ? "success"
                      : "info"
                }>
                  <AlertDescription>
                    {t(`statusNote.${booking.status}`)}
                  </AlertDescription>
                </Alert>
              ) : null}

              {bookingNeedsInvestigation(booking.status) ? (
                <Alert variant="warning">
                  <AlertTitle>{t("detail.investigateTitle")}</AlertTitle>
                  <AlertDescription>
                    {t("detail.investigateBody")}
                  </AlertDescription>
                </Alert>
              ) : null}

              <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 rounded-2xl border border-border px-4 py-3 text-sm">
                <dt className="text-muted-foreground">{t("detail.time")}</dt>
                <dd>
                  {formatDateTime(booking.starts_at, { tz: displayTz, locale })}–
                  {formatTime(booking.ends_at, { tz: displayTz, locale })}
                  {booking.guest_timezone ? (
                    <span className="ml-1 text-2xs text-muted-foreground">
                      ({t("calendar.guestTz", { tz: booking.guest_timezone })})
                    </span>
                  ) : null}
                </dd>
                <dt className="text-muted-foreground">{t("detail.guest")}</dt>
                <dd>{booking.guest_name}</dd>
                {booking.guest_email ? (
                  <>
                    <dt className="text-muted-foreground">
                      {t("detail.guestEmail")}
                    </dt>
                    <dd>{booking.guest_email}</dd>
                  </>
                ) : null}
                {booking.guest_email_verification_status ? (
                  <>
                    <dt className="text-muted-foreground">
                      {t("detail.emailVerification")}
                    </dt>
                    <dd>
                      <StatusBadge
                        status={booking.guest_email_verification_status}
                      />
                    </dd>
                  </>
                ) : null}
                <dt className="text-muted-foreground">{t("detail.host")}</dt>
                <dd>{booking.host_display_name ?? booking.host_principal_id}</dd>
                {booking.meeting_type_name ? (
                  <>
                    <dt className="text-muted-foreground">
                      {t("detail.meetingType")}
                    </dt>
                    <dd>{booking.meeting_type_name}</dd>
                  </>
                ) : null}
                {booking.hold_expires_at ? (
                  <>
                    <dt className="text-muted-foreground">
                      {t("detail.holdExpiresAt")}
                    </dt>
                    <dd>
                      {formatDateTime(booking.hold_expires_at, {
                        tz: displayTz,
                        locale,
                      })}
                    </dd>
                  </>
                ) : null}
                {booking.provider_event_id ? (
                  <>
                    <dt className="text-muted-foreground">
                      {t("detail.providerEvent")}
                    </dt>
                    <dd className="break-all font-mono text-xs">
                      {booking.provider_event_id}
                    </dd>
                  </>
                ) : null}
                {booking.failure_code ? (
                  <>
                    <dt className="text-muted-foreground">
                      {t("detail.failureCode")}
                    </dt>
                    <dd className="font-mono text-xs">{booking.failure_code}</dd>
                  </>
                ) : null}
                {booking.created_at ? (
                  <>
                    <dt className="text-muted-foreground">
                      {t("detail.createdAt")}
                    </dt>
                    <dd>
                      {formatDateTime(booking.created_at, {
                        tz: displayTz,
                        locale,
                      })}
                    </dd>
                  </>
                ) : null}
                <dt className="text-muted-foreground">{t("detail.version")}</dt>
                <dd className="tabular-nums">{booking.version ?? "—"}</dd>
              </dl>

              <div className="flex flex-wrap gap-2">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={!bookingCanReschedule(booking)}
                  onClick={() => setDialog("reschedule")}
                >
                  {t("actions.reschedule")}
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={!bookingCanCancel(booking)}
                  onClick={() => setDialog("cancel")}
                >
                  {t("actions.cancel")}
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={!bookingCanRecordAttendance(booking)}
                  onClick={() => setDialog("attendance")}
                >
                  {t("actions.recordAttendance")}
                </Button>
              </div>
              {bookingInFlight(booking.status) ? (
                <p className="text-2xs text-muted-foreground">
                  {t(`statusNote.${booking.status}`)}
                </p>
              ) : null}
            </>
          )}
        </div>
      </SheetContent>

      {booking ? (
        <>
          <RescheduleDialog
            booking={booking}
            etag={etag}
            open={dialog === "reschedule"}
            onOpenChange={(open) => setDialog(open ? "reschedule" : null)}
            onDone={invalidate}
          />
          <CancelBookingDialog
            booking={booking}
            etag={etag}
            open={dialog === "cancel"}
            onOpenChange={(open) => setDialog(open ? "cancel" : null)}
            onDone={invalidate}
          />
          <AttendanceDialog
            booking={booking}
            etag={etag}
            open={dialog === "attendance"}
            onOpenChange={(open) => setDialog(open ? "attendance" : null)}
            onDone={invalidate}
          />
        </>
      ) : null}
    </Sheet>
  );
}
