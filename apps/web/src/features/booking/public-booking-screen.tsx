"use client";

import Link from "next/link";
import { useMemo, useRef, useState, type FormEvent } from "react";
import { useLocale, useTranslations } from "next-intl";
import { useQuery } from "@tanstack/react-query";
import { CalendarClockIcon, GlobeIcon, UserIcon } from "lucide-react";

import { ErrorState } from "@/components/patterns/error-state";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { ApiError } from "@/lib/api/problem";
import { createPublicBooking, getPublicBookingLink, getPublicSlots, type PublicBookingAccepted } from "@/lib/api/booking";
import { idempotencyKey } from "@/lib/api/client";
import { COMMON_TIMEZONES, detectTimezone, formatDateTime, localDateKey, shiftLocalDate, timezoneOffsetAt, todayInZone } from "@/features/meetings/datetime";

export function PublicBookingScreen({ token }: { token: string }) {
  const t = useTranslations("booking.book");
  const locale = useLocale();
  const [timezoneOverride, setTimezoneOverride] = useState<string | null>(null);
  const [weekOffset, setWeekOffset] = useState(0);
  const [selectedStart, setSelectedStart] = useState("");
  const [guestName, setGuestName] = useState("");
  const [guestEmail, setGuestEmail] = useState("");
  const [memo, setMemo] = useState("");
  const [pending, setPending] = useState(false);
  const [result, setResult] = useState<PublicBookingAccepted | null>(null);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const bookingIdem = useRef(idempotencyKey());

  const link = useQuery({ queryKey: ["public-booking", token, "link"], queryFn: () => getPublicBookingLink(token), retry: false });
  const detectedTimezone = useMemo(() => detectTimezone(), []);
  const timezone = timezoneOverride ?? link.data?.data.timezone ?? detectedTimezone;
  const range = useMemo(() => {
    const start = shiftLocalDate(todayInZone(timezone), weekOffset * 7);
    const end = shiftLocalDate(start, 6);
    return { from: localDateKey(start), to: localDateKey(end) };
  }, [timezone, weekOffset]);
  const slots = useQuery({
    queryKey: ["public-booking", token, "slots", range, timezone],
    enabled: link.isSuccess,
    queryFn: () => getPublicSlots(token, { ...range, timezone }),
    retry: false,
  });
  function changeWeek(next: number) {
    setWeekOffset(next);
    setSelectedStart("");
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!selectedStart || !guestName.trim() || !guestEmail.trim()) {
      setSubmitError(t("selectSlotFirst"));
      return;
    }
    setPending(true);
    setSubmitError(null);
    try {
      const accepted = await createPublicBooking(token, {
        guest_name: guestName.trim(),
        guest_email: guestEmail.trim(),
        starts_at: selectedStart,
        timezone,
        privacy_notice_version: data.privacy_notice_version,
        ...(memo.trim() ? { memo: memo.trim() } : {}),
      }, { idempotencyKey: bookingIdem.current });
      bookingIdem.current = idempotencyKey();
      setResult(accepted.data);
    } catch (cause) {
      const apiError = cause instanceof ApiError ? cause : null;
      if (apiError?.code === "SLOT_UNAVAILABLE") {
        setSubmitError(t("errorSlotGone"));
        setSelectedStart("");
        await slots.refetch();
      } else if (apiError?.code === "BOOKING_RECIPIENT_MISMATCH") {
        setSubmitError(t("errorRecipientMismatch"));
      } else if (apiError?.status === 429) {
        setSubmitError(t("errorRateLimited"));
      } else if (apiError?.code === "CALENDAR_AVAILABILITY_UNAVAILABLE") {
        setSubmitError(t("errorCalendar"));
      } else {
        setSubmitError(apiError?.problem.detail ?? t("errorGeneric"));
      }
      if (apiError && apiError.problem.retryable !== true && apiError.status < 500) bookingIdem.current = idempotencyKey();
    } finally {
      setPending(false);
    }
  }

  if (link.isPending) return <div className="flex flex-col gap-5" aria-busy="true"><Skeleton className="h-8 w-60" /><Skeleton className="h-24 w-full rounded-2xl" /><Skeleton className="h-40 w-full rounded-2xl" /></div>;
  if (link.isError) {
    const invalid = link.error instanceof ApiError && (link.error.status === 404 || link.error.status === 410);
    return invalid ? <Unavailable t={t} /> : <ErrorState title={t("errorLoad")} error={link.error} onRetry={() => void link.refetch()} />;
  }

  const data = link.data.data;
  if (result) {
    return <div className="flex flex-col gap-5">
      <div><h1 className="text-2xl font-semibold">{t("confirmingTitle")}</h1><p className="mt-1 text-sm text-muted-foreground">{result.message ?? t("confirmingBody")}</p></div>
      <p className="rounded-2xl border border-border bg-card px-4 py-3 text-sm">{formatDateTime(result.starts_at ?? selectedStart, { tz: result.timezone ?? timezone, locale })}</p>
      {result.manage_url ? <div className="flex flex-col gap-2 rounded-2xl border border-border bg-card p-4"><Link className="font-medium underline underline-offset-4" href={result.manage_url}>{t("manageLink")}</Link><p className="text-xs text-muted-foreground">{t("manageNote")}</p></div> : null}
    </div>;
  }

  return <div className="flex flex-col gap-5">
    <div><h1 className="text-2xl font-semibold">{data.name ?? data.title ?? t("title")}</h1><p className="mt-1 text-sm text-muted-foreground">{data.description ?? t("description")}</p></div>
    <dl className="grid gap-2 rounded-2xl border border-border bg-card px-4 py-3 text-sm">
      <div className="flex items-center gap-2"><UserIcon className="size-4 text-muted-foreground" aria-hidden /><dt className="text-muted-foreground">{t("host")}</dt><dd>{data.host_display_name ?? data.host_name ?? "—"}</dd></div>
      <div className="flex items-center gap-2"><GlobeIcon className="size-4 text-muted-foreground" aria-hidden /><dt className="text-muted-foreground">{t("timezone")}</dt><dd>{timezone}</dd></div>
      {data.duration_minutes ? <div className="flex items-center gap-2"><CalendarClockIcon className="size-4 text-muted-foreground" aria-hidden /><dt className="text-muted-foreground">{t("duration", { count: data.duration_minutes })}</dt><dd>{data.duration_minutes} {t("minutesUnit")}</dd></div> : null}
    </dl>
    <div className="grid gap-1.5">
      <Label htmlFor="booking-timezone">{t("selectTimezone")}</Label>
      <Select value={timezone} onValueChange={(next) => { setTimezoneOverride(next); setSelectedStart(""); }}>
        <SelectTrigger id="booking-timezone"><SelectValue /></SelectTrigger>
        <SelectContent>{[...new Set([timezone, ...COMMON_TIMEZONES])].map((zone) => <SelectItem key={zone} value={zone}>{zone}</SelectItem>)}</SelectContent>
      </Select>
    </div>
    <section aria-label={t("selectSlot")} className="flex flex-col gap-3">
      <div className="flex items-center justify-between gap-2"><h2 className="flex items-center gap-1.5 text-sm font-medium"><CalendarClockIcon className="size-4" aria-hidden />{t("selectSlot")}</h2><div className="flex gap-2"><Button type="button" size="xs" variant="outline" disabled={weekOffset <= 0} onClick={() => changeWeek(Math.max(0, weekOffset - 1))}>{t("prevWeek")}</Button><Button type="button" size="xs" variant="outline" onClick={() => changeWeek(weekOffset + 1)}>{t("nextWeek")}</Button></div></div>
      {slots.isPending ? <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">{Array.from({ length: 6 }).map((_, index) => <Skeleton key={index} className="h-10 w-full" />)}</div> : null}
      {slots.isError ? <ErrorState title={t("errorSlots")} description={slots.error instanceof ApiError && slots.error.code === "CALENDAR_AVAILABILITY_UNAVAILABLE" ? t("staleRetry") : undefined} error={slots.error} onRetry={() => void slots.refetch()} /> : null}
      {slots.isSuccess && slots.data.data.slots.length === 0 ? <p className="rounded-2xl border border-dashed border-border p-5 text-center text-sm text-muted-foreground">{t("slotsEmpty")}</p> : null}
      {slots.isSuccess && slots.data.data.slots.length > 0 ? <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">{slots.data.data.slots.map((slot) => <Button key={slot.starts_at} type="button" variant={selectedStart === slot.starts_at ? "default" : "outline"} aria-pressed={selectedStart === slot.starts_at} onClick={() => setSelectedStart(slot.starts_at)}>{formatDateTime(slot.starts_at, { tz: timezone, locale })} · {timezoneOffsetAt(slot.starts_at, timezone, locale)}</Button>)}</div> : null}
      {slots.data?.data.availability_checked_at ? <p className="text-xs text-muted-foreground">{t("checkedAt", { time: formatDateTime(slots.data.data.availability_checked_at, { tz: timezone, locale }) })}</p> : null}
      <p className="text-xs text-muted-foreground">{t("notGuaranteed")}</p>
    </section>
    <form className="flex flex-col gap-4" onSubmit={(event) => void submit(event)}>
      <div className="grid gap-1.5"><Label htmlFor="booking-guest-name">{t("name")}</Label><Input id="booking-guest-name" autoComplete="name" required maxLength={300} value={guestName} onChange={(event) => setGuestName(event.target.value)} placeholder={t("namePlaceholder")} /></div>
      <div className="grid gap-1.5"><Label htmlFor="booking-guest-email">{t("email")}</Label><Input id="booking-guest-email" type="email" autoComplete="email" required maxLength={320} value={guestEmail} onChange={(event) => setGuestEmail(event.target.value)} placeholder={t("emailPlaceholder")} /></div>
      <div className="grid gap-1.5"><Label htmlFor="booking-guest-memo">{t("memo")}</Label><Textarea id="booking-guest-memo" rows={3} maxLength={2000} value={memo} onChange={(event) => setMemo(event.target.value)} placeholder={t("memoPlaceholder")} /></div>
      {submitError ? <p role="alert" className="text-sm text-destructive">{submitError}</p> : null}
      <Button type="submit" disabled={pending || !selectedStart}>{pending ? t("submitting") : t("submit")}</Button>
      <p className="text-xs text-muted-foreground">{t("footnote")}</p>
    </form>
  </div>;
}

function Unavailable({ t }: { t: ReturnType<typeof useTranslations> }) {
  return <div className="flex flex-col gap-2"><h1 className="text-2xl font-semibold">{t("linkExpiredTitle")}</h1><p className="text-sm text-muted-foreground">{t("linkExpiredBody")}</p></div>;
}
