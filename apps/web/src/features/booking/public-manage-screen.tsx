"use client";

import { useRef, useState } from "react";
import { useLocale, useTranslations } from "next-intl";
import { useQuery } from "@tanstack/react-query";

import { ErrorState } from "@/components/patterns/error-state";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { ApiError } from "@/lib/api/problem";
import { cancelManagedBooking, getManagedBooking, getManagedSlots, rescheduleManagedBooking, type PublicSlot } from "@/lib/api/booking";
import { BookingCard, MeetingSummary, SlotPicker, guestTimezone } from "./public-booking-screen";
import { idempotencyKey } from "@/lib/api/client";
import { ArrowLeftIcon, CalendarCheckIcon, LinkIcon, RefreshCwIcon } from "lucide-react";
import { detectTimezone, formatDayLabel, formatTime, localDateOf, parseInstant } from "@/features/meetings/datetime";

export function PublicManageScreen({ manageToken }: { manageToken: string }) {
  const t = useTranslations("booking.manage");
  const locale = useLocale();
  const [rescheduleOpen, setRescheduleOpen] = useState(false);
  const [cancelOpen, setCancelOpen] = useState(false);
  const tb = useTranslations("booking.book");
  const [newSlot, setNewSlot] = useState<PublicSlot | null>(null);
  const [pickerTimezone, setPickerTimezone] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const idem = useRef(idempotencyKey());
  const changeRefreshUntil = useRef(0);
  const booking = useQuery({
    queryKey: ["public-booking", manageToken, "manage"],
    queryFn: () => getManagedBooking(manageToken),
    retry: false,
    refetchInterval: (query) => {
      const status = query.state.data?.data.status;
      return status === "held" || status === "confirming" || status === "canceling" || Date.now() < changeRefreshUntil.current ? 2000 : false;
    },
  });
  const data = booking.data?.data;
  const zone = data?.timezone ?? data?.guest_timezone ?? detectTimezone();
  const canCancel = data?.status === "held" || data?.status === "confirmed";
  const canReschedule = data?.status === "confirmed";

  function openReschedule() {
    setPickerTimezone((current) => current ?? guestTimezone(zone));
    setNewSlot(null);
    setError(null);
    setNotice(null);
    setRescheduleOpen(true);
  }

  async function reschedule() {
    if (!data?.version || !newSlot) return;
    const timezone = pickerTimezone ?? zone;
    setPending(true);
    setError(null);
    try {
      await rescheduleManagedBooking(manageToken, { starts_at: newSlot.starts_at, timezone, expected_version: data.version }, { idempotencyKey: idem.current });
      // A reschedule leaves the booking confirmed while its change job runs.
      // Briefly refresh so the displayed time catches up without a manual reload.
      changeRefreshUntil.current = Date.now() + 30_000;
      idem.current = idempotencyKey();
      setNotice(t("rescheduleDone"));
      setRescheduleOpen(false);
      setNewSlot(null);
      await booking.refetch();
    } catch (cause) {
      const apiError = cause instanceof ApiError ? cause : null;
      const slotGone = apiError?.status === 409 || apiError?.code === "SLOT_UNAVAILABLE";
      setError(slotGone ? t("errorSlotGone") : apiError?.status === 412 ? t("versionConflict") : apiError?.problem.detail ?? t("errorGeneric"));
      if (slotGone) setNewSlot(null);
      if (apiError && apiError.problem.retryable !== true && apiError.status < 500) idem.current = idempotencyKey();
      if (apiError?.status === 412) await booking.refetch();
    } finally {
      setPending(false);
    }
  }

  async function cancel() {
    if (!data?.version) return;
    setPending(true);
    setError(null);
    try {
      await cancelManagedBooking(manageToken, { expected_version: data.version }, { idempotencyKey: idem.current });
      idem.current = idempotencyKey();
      setNotice(t("cancelDone"));
      setCancelOpen(false);
      await booking.refetch();
    } catch (cause) {
      const apiError = cause instanceof ApiError ? cause : null;
      setError(apiError?.status === 412 ? t("versionConflict") : apiError?.problem.detail ?? t("errorGeneric"));
      if (apiError && apiError.problem.retryable !== true && apiError.status < 500) idem.current = idempotencyKey();
      if (apiError?.status === 412) await booking.refetch();
    } finally {
      setPending(false);
    }
  }

  if (booking.isPending) return <div className="mx-auto flex w-full max-w-md flex-col items-center gap-4 pt-10" aria-busy="true"><Skeleton className="h-6 w-20 rounded-full" /><Skeleton className="h-8 w-56" /><Skeleton className="h-40 w-full rounded-2xl" /></div>;
  if (booking.isError) {
    const invalid = booking.error instanceof ApiError && (booking.error.status === 404 || booking.error.status === 410);
    return invalid
      ? <div className="mx-auto flex w-full max-w-sm flex-col items-center gap-3 pt-10 text-center"><span className="flex size-12 items-center justify-center rounded-full bg-muted"><LinkIcon className="size-5 text-muted-foreground" aria-hidden /></span><h1 className="text-xl font-semibold tracking-tight">{t("title")}</h1><p className="text-sm text-muted-foreground break-keep">{t("notFound")}</p></div>
      : <ErrorState title={t("errorLoad")} description={t("errorRetryHint")} error={booking.error} onRetry={() => void booking.refetch()} />;
  }
  if (!data) return null;

  if (rescheduleOpen && data.starts_at && data.ends_at) {
    const timezone = pickerTimezone ?? zone;
    const current: PublicSlot = { starts_at: data.starts_at, ends_at: data.ends_at };
    return <BookingCard>
      <MeetingSummary
        meeting={{ name: data.meeting_name, description: data.meeting_description, host: data.host_display_name, durationMinutes: data.duration_minutes }}
        slot={newSlot ?? current}
        slotLabel={newSlot ? t("newTime") : t("currentTime")}
        timezone={timezone}
        t={tb}
        locale={locale}
      />
      <section className="flex flex-col gap-4 pt-6 lg:p-8">
        <Button type="button" size="sm" variant="ghost" className="self-start" onClick={() => (newSlot ? setNewSlot(null) : setRescheduleOpen(false))}>
          <ArrowLeftIcon aria-hidden />{newSlot ? t("pickAgain") : t("backToBooking")}
        </Button>
        {error ? <Alert variant="destructive"><AlertDescription>{error}</AlertDescription></Alert> : null}
        {newSlot ? (
          <div className="flex max-w-md flex-col gap-5">
            <div className="flex flex-col gap-1">
              <h2 className="text-lg font-semibold tracking-tight">{t("rescheduleConfirmTitle")}</h2>
              <p className="text-sm text-muted-foreground break-keep">{t("rescheduleDescription")}</p>
            </div>
            <div className="grid gap-2 rounded-2xl border border-border p-4 text-sm">
              <SlotLine label={t("currentTime")} slot={current} timezone={timezone} locale={locale} muted />
              <SlotLine label={t("newTime")} slot={newSlot} timezone={timezone} locale={locale} />
            </div>
            <Button type="button" size="xl" className="w-full sm:w-auto sm:self-start" disabled={pending} onClick={() => void reschedule()}>
              {pending ? t("rescheduleSubmitting") : t("rescheduleSubmit")}
            </Button>
          </div>
        ) : (
          <SlotPicker
            queryKey={["public-booking", manageToken, "manage-slots"]}
            fetchSlots={(query) => getManagedSlots(manageToken, query)}
            horizonDays={data.booking_horizon_days}
            timezone={timezone}
            onTimezoneChange={setPickerTimezone}
            onPick={(slot) => { setError(null); setNewSlot(slot); }}
            title={t("rescheduleTitle")}
            t={tb}
            locale={locale}
          />
        )}
      </section>
    </BookingCard>;
  }

  const note = statusNote(data.status, t);
  return <div className="mx-auto flex w-full max-w-md flex-col gap-6 pt-4 sm:pt-8">
    <div className="flex flex-col items-center gap-3 text-center">
      <StatusBadge status={data.status} />
      <h1 className="text-2xl font-semibold tracking-tight break-keep">{statusTitle(data.status, t)}</h1>
      {note ? <p className="text-sm leading-relaxed text-muted-foreground break-keep">{note}</p> : null}
    </div>
    {notice ? <Alert variant="success"><AlertDescription>{notice}</AlertDescription></Alert> : null}
    {error && !cancelOpen ? <Alert variant="destructive"><AlertDescription>{error}</AlertDescription></Alert> : null}
    <div className="rounded-2xl border border-border bg-card p-5">
      {data.meeting_name ? <p className="text-sm font-medium">{data.meeting_name}</p> : null}
      {data.starts_at ? (
        <div className="mt-3 flex items-start gap-3">
          <CalendarCheckIcon className="mt-1 size-5 shrink-0 text-muted-foreground" aria-hidden />
          <div>
            <p className="text-lg font-semibold">{formatDayLabel(localDateOf(parseInstant(data.starts_at), zone), locale)}</p>
            <p className="tabular-nums">{formatTime(data.starts_at, { tz: zone, locale })}{data.ends_at ? ` – ${formatTime(data.ends_at, { tz: zone, locale })}` : ""}</p>
            <p className="mt-1 text-xs text-muted-foreground">{zone}</p>
          </div>
        </div>
      ) : null}
      <dl className="mt-5 grid grid-cols-2 gap-4 border-t border-border pt-4 text-sm">
        <Field label={t("host")} value={data.host_display_name} />
        <Field label={t("guest")} value={data.guest_name} />
      </dl>
    </div>
    <div className="flex flex-col gap-2">
      {canReschedule ? <Button size="lg" variant="outline" className="w-full" onClick={openReschedule}>{t("reschedule")}</Button> : null}
      {canCancel ? <Button size="lg" variant="ghost" className="w-full text-destructive hover:text-destructive" onClick={() => { setError(null); setCancelOpen(true); }}>{t("cancel")}</Button> : null}
      {!canReschedule && !canCancel ? <Button size="sm" variant="ghost" className="self-center" onClick={() => void booking.refetch()}><RefreshCwIcon aria-hidden />{t("refresh")}</Button> : null}
    </div>
    <Dialog open={cancelOpen} onOpenChange={setCancelOpen}>
      <DialogContent>
        <DialogHeader><DialogTitle>{t("cancelTitle")}</DialogTitle><DialogDescription>{t("cancelDescription")}</DialogDescription></DialogHeader>
        {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
        <DialogFooter><Button variant="outline" onClick={() => setCancelOpen(false)}>{t("keep")}</Button><Button variant="destructive" disabled={pending} onClick={() => void cancel()}>{pending ? t("cancelSubmitting") : t("cancelConfirm")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  </div>;
}

function SlotLine({ label, slot, timezone, locale, muted }: { label: string; slot: PublicSlot; timezone: string; locale: string; muted?: boolean }) {
  return <div className={muted ? "flex justify-between gap-3 text-muted-foreground line-through decoration-1" : "flex justify-between gap-3 font-medium"}>
    <span className="no-underline">{label}</span>
    <span className="tabular-nums">{formatDayLabel(localDateOf(parseInstant(slot.starts_at), timezone), locale)} {formatTime(slot.starts_at, { tz: timezone, locale })}</span>
  </div>;
}

function statusTitle(status: string, t: ReturnType<typeof useTranslations>): string {
  switch (status) {
    case "confirmed": return t("confirmedTitle");
    case "held":
    case "confirming": return t("pendingTitle");
    case "canceling": return t("cancelingTitle");
    case "canceled": return t("canceledTitle");
    case "failed":
    case "expired": return t("failedTitle");
    default: return t("unknownTitle");
  }
}

function Field({ label, value }: { label: string; value: string | null | undefined }) {
  return <div><dt className="text-xs text-muted-foreground">{label}</dt><dd className="mt-1">{value || "—"}</dd></div>;
}

function statusNote(status: string, t: ReturnType<typeof useTranslations>): string | undefined {
  switch (status) {
    case "held": return t("heldNote");
    case "confirming": return t("confirmingNote");
    case "canceling": return t("cancelingNote");
    case "unknown": return t("unknownNote");
    case "failed": return t("failedNote");
    case "canceled": return t("canceledNote");
    case "expired": return t("expiredNote");
    case "confirmed": return t("confirmedNote");
    default: return undefined;
  }
}
