"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import type * as React from "react";
import { useLocale, useTranslations } from "next-intl";
import { useQuery } from "@tanstack/react-query";
import {
  ArrowLeftIcon,
  CalendarCheckIcon,
  CheckIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  ClockIcon,
  GlobeIcon,
  LinkIcon,
  LockIcon,
  MapPinIcon,
  VideoIcon,
} from "lucide-react";

import { ErrorState } from "@/components/patterns/error-state";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { ApiError } from "@/lib/api/problem";
import {
  createPublicBooking,
  getPublicBookingLink,
  getPublicLinkBooking,
  getPublicSlots,
  type PublicBookingAccepted,
  type PublicBookingLink,
  type PublicSlot,
  type PublicSlotsData,
} from "@/lib/api/booking";
import { idempotencyKey } from "@/lib/api/client";
import { cn } from "@/lib/utils";
import {
  COMMON_TIMEZONES,
  compareLocalDate,
  detectTimezone,
  formatDayLabel,
  formatTime,
  isValidTimezone,
  isoWeekdayOf,
  localDateKey,
  localDateOf,
  parseInstant,
  sameLocalDate,
  shiftLocalDate,
  timezoneOffsetAt,
  todayInZone,
  type LocalDate,
} from "@/features/meetings/datetime";

type Translator = ReturnType<typeof useTranslations>;
type Month = { y: number; m: number };

function daysInMonth({ y, m }: Month): number {
  return new Date(Date.UTC(y, m, 0)).getUTCDate();
}

function addMonths({ y, m }: Month, delta: number): Month {
  const index = y * 12 + (m - 1) + delta;
  return { y: Math.floor(index / 12), m: (index % 12) + 1 };
}

function compareMonth(a: Month, b: Month): number {
  return a.y * 12 + a.m - (b.y * 12 + b.m);
}

/** 예약 가능한 달의 조회 범위 — 오늘 이전과 예약 가능 기간 이후는 잘라낸다(서버 상한 31일). */
function monthRange(month: Month, today: LocalDate, horizonEnd: LocalDate): { from: LocalDate; to: LocalDate } | null {
  const first = { y: month.y, m: month.m, d: 1 };
  const last = { y: month.y, m: month.m, d: daysInMonth(month) };
  const from = compareLocalDate(first, today) < 0 ? today : first;
  const to = compareLocalDate(last, horizonEnd) > 0 ? horizonEnd : last;
  return compareLocalDate(from, to) > 0 ? null : { from, to };
}

export function PublicBookingScreen({ token }: { token: string }) {
  const t = useTranslations("booking.book");
  const locale = useLocale();
  const router = useRouter();
  const link = useQuery({ queryKey: ["public-booking", token, "link"], queryFn: () => getPublicBookingLink(token), retry: false });
  // 이미 예약에 쓰인 초대 링크를 다시 열면 그 예약의 관리 페이지로 보낸다.
  const usedUp = link.error instanceof ApiError && link.error.status === 410;
  const existing = useQuery({
    queryKey: ["public-booking", token, "existing"],
    enabled: usedUp,
    queryFn: () => getPublicLinkBooking(token),
    retry: false,
  });
  const manageUrl = existing.data?.data.manage_url;
  useEffect(() => {
    if (manageUrl) router.replace(manageUrl);
  }, [manageUrl, router]);

  if (link.isPending || (usedUp && (existing.isPending || manageUrl))) return <ScreenSkeleton />;
  if (link.isError) {
    const invalid = link.error instanceof ApiError && (link.error.status === 404 || link.error.status === 410);
    return invalid
      ? <Unavailable t={t} />
      : <div className="mx-auto w-full max-w-md"><ErrorState title={t("errorLoad")} description={t("errorRetryHint")} error={link.error} onRetry={() => void link.refetch()} /></div>;
  }
  return <BookingFlow token={token} data={link.data.data} t={t} locale={locale} />;
}

function BookingFlow({ token, data, t, locale }: { token: string; data: PublicBookingLink; t: Translator; locale: string }) {
  const [timezone, setTimezone] = useState(() => guestTimezone(data.timezone));
  const [selectedSlot, setSelectedSlot] = useState<PublicSlot | null>(null);
  const [pickerKey, setPickerKey] = useState(0);
  const [result, setResult] = useState<PublicBookingAccepted | null>(null);
  const topRef = useRef<HTMLDivElement>(null);

  if (result) {
    return <BookingDone result={result} slot={selectedSlot} timezone={timezone} t={t} locale={locale} />;
  }

  return <BookingCard ref={topRef}>
    <MeetingSummary
      meeting={{ name: data.name ?? data.title, description: data.description, host: data.host_display_name ?? data.host_name, durationMinutes: data.duration_minutes, locationKind: data.location_kind, locationDetail: data.location_detail }}
      slot={selectedSlot}
      timezone={timezone}
      t={t}
      locale={locale}
      onChange={selectedSlot ? () => setSelectedSlot(null) : undefined}
    />
    <section className="flex flex-col pt-6 lg:p-8">
      {selectedSlot ? (
        <GuestForm
          key={selectedSlot.starts_at}
          token={token}
          data={data}
          slot={selectedSlot}
          timezone={timezone}
          t={t}
          onBack={() => setSelectedSlot(null)}
          onSlotGone={() => { setSelectedSlot(null); setPickerKey((key) => key + 1); }}
          onDone={setResult}
        />
      ) : (
        <SlotPicker
          key={pickerKey}
          queryKey={["public-booking", token, "slots"]}
          fetchSlots={(query) => getPublicSlots(token, query)}
          horizonDays={data.booking_horizon_days}
          timezone={timezone}
          onTimezoneChange={setTimezone}
          onPick={(slot) => { setSelectedSlot(slot); topRef.current?.scrollIntoView({ behavior: "smooth", block: "start" }); }}
          t={t}
          locale={locale}
        />
      )}
    </section>
  </BookingCard>;
}

/** 게스트는 자기 시간대로 본다 — 감지에 실패하면 호스트 시간대로 떨어진다. */
export function guestTimezone(fallback?: string | null): string {
  const detected = detectTimezone();
  return isValidTimezone(detected) ? detected : fallback ?? "UTC";
}

/** 예약·시간 변경 화면이 함께 쓰는 2단 카드 (데스크톱: 요약 | 본문). */
export function BookingCard({ ref, children }: { ref?: React.Ref<HTMLDivElement>; children: React.ReactNode }) {
  return <div ref={ref} className="mx-auto w-full max-w-5xl scroll-mt-4">
    <div className="grid overflow-hidden lg:min-h-[600px] lg:grid-cols-[320px_1fr] lg:rounded-3xl lg:border lg:border-border lg:bg-card">
      {children}
    </div>
  </div>;
}

/** 월 달력 + 그날의 시간 목록 + 시간대. 시간을 누르면 onPick 으로 넘긴다. */
export function SlotPicker({ queryKey, fetchSlots, horizonDays, timezone, onTimezoneChange, onPick, title, t, locale }: {
  queryKey: readonly unknown[];
  fetchSlots: (query: { from: string; to: string; timezone: string }) => Promise<{ data: PublicSlotsData }>;
  horizonDays?: number | null;
  timezone: string;
  onTimezoneChange: (next: string) => void;
  onPick: (slot: PublicSlot) => void;
  title?: string;
  t: Translator;
  locale: string;
}) {
  const today = useMemo(() => todayInZone(timezone), [timezone]);
  const horizonEnd = useMemo(() => shiftLocalDate(today, Math.max(0, (horizonDays || 60) - 1)), [today, horizonDays]);
  const firstMonth: Month = { y: today.y, m: today.m };
  const lastMonth: Month = { y: horizonEnd.y, m: horizonEnd.m };
  const [month, setMonth] = useState<Month>(firstMonth);
  const [pickedDate, setSelectedDate] = useState<LocalDate | null>(null);
  const [autoAdvanced, setAutoAdvanced] = useState(false);

  const range = useMemo(() => monthRange(month, today, horizonEnd), [month, today, horizonEnd]);
  const slots = useQuery({
    queryKey: [...queryKey, range && localDateKey(range.from), range && localDateKey(range.to), timezone],
    enabled: range !== null,
    queryFn: () => fetchSlots({ from: localDateKey(range!.from), to: localDateKey(range!.to), timezone }),
    retry: false,
  });

  const slotsByDay = useMemo(() => {
    const byDay = new Map<string, PublicSlot[]>();
    for (const slot of slots.data?.data.slots ?? []) {
      const key = localDateKey(localDateOf(parseInstant(slot.starts_at), timezone));
      byDay.set(key, [...(byDay.get(key) ?? []), slot]);
    }
    return byDay;
  }, [slots.data, timezone]);

  const monthHasSlots = slotsByDay.size > 0;
  const canGoNext = compareMonth(month, lastMonth) < 0;

  // 첫 달이 비어 있으면 한 번만 다음 달로 넘어간다 (렌더 중 상태 보정 패턴).
  if (slots.isSuccess && !monthHasSlots && !autoAdvanced && canGoNext) {
    setAutoAdvanced(true);
    setMonth(addMonths(month, 1));
  }
  // 고른 날짜가 이 달의 예약 가능일이 아니면 첫 예약 가능일을 보여 준다.
  const selectedDate = useMemo<LocalDate | null>(() => {
    if (pickedDate && slotsByDay.has(localDateKey(pickedDate))) return pickedDate;
    const firstDay = [...slotsByDay.keys()].sort()[0];
    if (!firstDay) return null;
    const [y = 0, m = 0, d = 0] = firstDay.split("-").map(Number);
    return { y, m, d };
  }, [pickedDate, slotsByDay]);

  function changeMonth(delta: number) {
    setAutoAdvanced(true);
    setSelectedDate(null);
    setMonth((current) => addMonths(current, delta));
  }

  function changeTimezone(next: string) {
    onTimezoneChange(next);
    setSelectedDate(null);
  }

  const daySlots = selectedDate ? slotsByDay.get(localDateKey(selectedDate)) ?? [] : [];

  return <div className="flex flex-col gap-6">
    <h2 className="text-lg font-semibold tracking-tight">{title ?? t("pickDate")}</h2>
    <div className="grid gap-8 md:grid-cols-[minmax(0,1fr)_220px]">
      <div className="flex flex-col gap-5">
        <MonthCalendar
          month={month}
          today={today}
          selected={selectedDate}
          available={slotsByDay}
          loading={slots.isPending && range !== null}
          canPrev={compareMonth(month, firstMonth) > 0}
          canNext={canGoNext}
          onPrev={() => changeMonth(-1)}
          onNext={() => changeMonth(1)}
          onSelect={setSelectedDate}
          t={t}
          locale={locale}
        />
        <div className="hidden md:block"><TimezonePicker id="booking-timezone" value={timezone} onChange={changeTimezone} t={t} locale={locale} /></div>
      </div>
      <div className="flex min-h-0 flex-col gap-3">
        {slots.isError ? (
          <ErrorState
            title={t("errorSlots")}
            description={slots.error instanceof ApiError && slots.error.code === "CALENDAR_AVAILABILITY_UNAVAILABLE" ? t("staleRetry") : t("errorRetryHint")}
            error={slots.error}
            onRetry={() => void slots.refetch()}
          />
        ) : slots.isPending && range !== null ? (
          <div className="grid grid-cols-3 gap-2 md:grid-cols-1">{Array.from({ length: 6 }).map((_, index) => <Skeleton key={index} className="h-11 w-full rounded-full" />)}</div>
        ) : !monthHasSlots ? (
          <div className="flex flex-col items-center gap-3 rounded-2xl bg-muted/60 px-4 py-8 text-center">
            <CalendarCheckIcon className="size-5 text-muted-foreground" aria-hidden />
            <p className="text-sm text-muted-foreground">{t("monthEmpty")}</p>
            {canGoNext ? <Button type="button" size="sm" variant="outline" onClick={() => changeMonth(1)}>{t("showNextMonth")}<ChevronRightIcon aria-hidden /></Button> : null}
          </div>
        ) : selectedDate ? (
          <>
            <p className="text-sm font-medium">{formatDayLabel(selectedDate, locale)}</p>
            <ul className="grid grid-cols-3 gap-2 md:max-h-[420px] md:grid-cols-1 md:overflow-y-auto md:pr-1">
              {daySlots.map((slot) => (
                <li key={slot.starts_at}>
                  <button
                    type="button"
                    onClick={() => onPick(slot)}
                    className="flex h-11 w-full items-center justify-center rounded-full border border-border bg-background text-sm font-medium tabular-nums transition-colors hover:border-foreground hover:bg-accent focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
                  >
                    {formatTime(slot.starts_at, { tz: timezone, locale })}
                  </button>
                </li>
              ))}
            </ul>
          </>
        ) : (
          <p className="text-sm text-muted-foreground">{t("pickDayFirst")}</p>
        )}
      </div>
      <div className="md:hidden"><TimezonePicker id="booking-timezone-mobile" value={timezone} onChange={changeTimezone} t={t} locale={locale} /></div>
    </div>
  </div>;
}

export interface MeetingInfo {
  name?: string | null;
  description?: string | null;
  host?: string | null;
  durationMinutes?: number | null;
  locationKind?: "online" | "offline" | null;
  locationDetail?: string | null;
  /** 예약 전에는 온라인 접속 링크를 숨기고 "확정 후 안내"만 보여 준다. */
  revealOnlineDetail?: boolean;
}

/** 미팅 방식 한 줄 — 온라인/오프라인 + 장소·접속 안내. */
export function MeetingLocation({ meeting, t, pendingNote }: { meeting: MeetingInfo; t: Translator; pendingNote?: string }) {
  if (!meeting.locationKind) return null;
  const offline = meeting.locationKind === "offline";
  const detail = offline || meeting.revealOnlineDetail ? meeting.locationDetail?.trim() : null;
  const isLink = detail ? /^https?:\/\//i.test(detail) : false;
  return <div className="flex items-start gap-1.5 text-sm text-muted-foreground">
    {offline ? <MapPinIcon className="mt-0.5 size-4 shrink-0" aria-hidden /> : <VideoIcon className="mt-0.5 size-4 shrink-0" aria-hidden />}
    <div className="min-w-0">
      <p>{offline ? t("locationOffline") : t("locationOnline")}</p>
      {detail ? (
        isLink
          ? <a href={detail} target="_blank" rel="noopener noreferrer" className="break-all text-foreground underline underline-offset-4">{detail}</a>
          : <p className="whitespace-pre-line break-keep text-foreground">{detail}</p>
      ) : !offline ? <p className="text-xs">{pendingNote ?? t("locationOnlinePending")}</p> : null}
    </div>
  </div>;
}

/** 왼쪽 요약 — 호스트·미팅·소요 시간, 그리고 고른(또는 현재) 일정. */
export function MeetingSummary({ meeting, slot, slotLabel, timezone, t, locale, onChange }: {
  meeting: MeetingInfo;
  slot: PublicSlot | null;
  slotLabel?: string;
  timezone: string;
  t: Translator;
  locale: string;
  onChange?: () => void;
}) {
  const host = meeting.host ?? "";
  return <aside className="flex flex-col gap-5 border-b border-border pb-6 lg:border-r lg:border-b-0 lg:p-8">
    {host ? (
      <div className="flex items-center gap-3">
        <span aria-hidden className="flex size-10 items-center justify-center rounded-full bg-primary text-sm font-semibold text-primary-foreground">{host.trim().charAt(0).toUpperCase()}</span>
        <div className="min-w-0">
          <p className="text-xs text-muted-foreground">{t("host")}</p>
          <p className="truncate text-sm font-medium">{host}</p>
        </div>
      </div>
    ) : null}
    <div className="flex flex-col gap-2">
      <h1 className="text-2xl font-semibold tracking-tight break-keep">{meeting.name ?? t("title")}</h1>
      {meeting.durationMinutes ? (
        <p className="flex items-center gap-1.5 text-sm text-muted-foreground"><ClockIcon className="size-4" aria-hidden />{t("durationMinutes", { count: meeting.durationMinutes })}</p>
      ) : null}
      <MeetingLocation meeting={meeting} t={t} />
    </div>
    {meeting.description ? <p className="text-sm leading-relaxed text-muted-foreground whitespace-pre-line break-keep">{meeting.description}</p> : null}
    {slot ? (
      <div className="flex items-start gap-3 rounded-2xl bg-muted/70 p-4">
        <CalendarCheckIcon className="mt-0.5 size-4 shrink-0" aria-hidden />
        <div className="min-w-0 flex-1 text-sm">
          {slotLabel ? <p className="mb-1 text-xs text-muted-foreground">{slotLabel}</p> : null}
          <p className="font-medium">{formatDayLabel(localDateOf(parseInstant(slot.starts_at), timezone), locale)}</p>
          <p className="tabular-nums">{formatTime(slot.starts_at, { tz: timezone, locale })} – {formatTime(slot.ends_at, { tz: timezone, locale })}</p>
          <p className="mt-1 text-xs text-muted-foreground">{timezone} ({timezoneOffsetAt(slot.starts_at, timezone, locale)})</p>
        </div>
        {onChange ? <Button type="button" size="xs" variant="ghost" onClick={onChange}>{t("change")}</Button> : null}
      </div>
    ) : null}
  </aside>;
}

function MonthCalendar({ month, today, selected, available, loading, canPrev, canNext, onPrev, onNext, onSelect, t, locale }: {
  month: Month;
  today: LocalDate;
  selected: LocalDate | null;
  available: Map<string, PublicSlot[]>;
  loading: boolean;
  canPrev: boolean;
  canNext: boolean;
  onPrev: () => void;
  onNext: () => void;
  onSelect: (date: LocalDate) => void;
  t: Translator;
  locale: string;
}) {
  const monthLabel = new Intl.DateTimeFormat(locale, { year: "numeric", month: "long", timeZone: "UTC" }).format(new Date(Date.UTC(month.y, month.m - 1, 1)));
  // 일요일 시작 — 2023-01-01은 일요일이다.
  const weekdays = Array.from({ length: 7 }, (_, index) =>
    new Intl.DateTimeFormat(locale, { weekday: "short", timeZone: "UTC" }).format(new Date(Date.UTC(2023, 0, 1 + index))));
  const leading = isoWeekdayOf({ y: month.y, m: month.m, d: 1 }) % 7;
  const days = Array.from({ length: daysInMonth(month) }, (_, index) => ({ y: month.y, m: month.m, d: index + 1 }));

  return <div className="flex flex-col gap-3">
    <div className="flex items-center justify-between">
      <p className="text-sm font-semibold" aria-live="polite">{monthLabel}</p>
      <div className="flex gap-1">
        <Button type="button" size="icon-sm" variant="ghost" disabled={!canPrev} onClick={onPrev} aria-label={t("prevMonth")}><ChevronLeftIcon aria-hidden /></Button>
        <Button type="button" size="icon-sm" variant="ghost" disabled={!canNext} onClick={onNext} aria-label={t("nextMonth")}><ChevronRightIcon aria-hidden /></Button>
      </div>
    </div>
    <div className="grid grid-cols-7 gap-y-1 text-center" role="grid" aria-busy={loading}>
      {weekdays.map((label) => <span key={label} className="pb-1 text-2xs font-medium text-muted-foreground">{label}</span>)}
      {Array.from({ length: leading }).map((_, index) => <span key={`pad-${index}`} />)}
      {days.map((date) => {
        const key = localDateKey(date);
        const isAvailable = available.has(key);
        const isSelected = selected !== null && sameLocalDate(selected, date);
        const isToday = sameLocalDate(today, date);
        return <div key={key} className="flex justify-center">
          <button
            type="button"
            disabled={!isAvailable}
            aria-pressed={isSelected}
            aria-label={formatDayLabel(date, locale)}
            onClick={() => onSelect(date)}
            className={cn(
              "relative flex size-10 items-center justify-center rounded-full text-sm tabular-nums transition-colors focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50 sm:size-11",
              isSelected
                ? "bg-primary font-semibold text-primary-foreground"
                : isAvailable
                  ? "bg-accent font-semibold text-foreground hover:bg-primary hover:text-primary-foreground"
                  : "cursor-default text-muted-foreground/50",
              loading && "animate-pulse",
            )}
          >
            {date.d}
            {isToday ? <span aria-hidden className={cn("absolute bottom-1.5 size-1 rounded-full", isSelected ? "bg-primary-foreground" : "bg-foreground")} /> : null}
          </button>
        </div>;
      })}
    </div>
  </div>;
}

function TimezonePicker({ id, value, onChange, t, locale }: { id: string; value: string; onChange: (next: string) => void; t: Translator; locale: string }) {
  const zones = useMemo(() => [...new Set([value, ...COMMON_TIMEZONES])], [value]);
  const [now] = useState(() => Date.now());
  return <div className="flex flex-col gap-1.5">
    <Label htmlFor={id} className="text-xs text-muted-foreground">{t("timezone")}</Label>
    <Select value={value} onValueChange={onChange}>
      <SelectTrigger id={id} className="w-full justify-start [&>span]:flex-1 [&>span]:text-left">
        <GlobeIcon className="size-4 text-muted-foreground" aria-hidden />
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {zones.map((zone) => <SelectItem key={zone} value={zone}>{zone.replaceAll("_", " ")} ({timezoneOffsetAt(now, zone, locale)})</SelectItem>)}
      </SelectContent>
    </Select>
  </div>;
}

function GuestForm({ token, data, slot, timezone, t, onBack, onSlotGone, onDone }: {
  token: string;
  data: PublicBookingLink;
  slot: PublicSlot;
  timezone: string;
  t: Translator;
  onBack: () => void;
  onSlotGone: () => void;
  onDone: (accepted: PublicBookingAccepted) => void;
}) {
  const invitee = data.invitee;
  const [guestName, setGuestName] = useState(invitee?.name ?? "");
  const [guestEmail, setGuestEmail] = useState(invitee?.email ?? "");
  const [memo, setMemo] = useState("");
  const [pending, setPending] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const bookingIdem = useRef(idempotencyKey());
  const nameRef = useRef<HTMLInputElement>(null);

  // 이름까지 채워져 있으면 바로 예약 버튼을 누를 수 있게 메모로 보낸다.
  const memoRef = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    (invitee?.name ? memoRef.current : nameRef.current)?.focus({ preventScroll: true });
  }, [invitee?.name]);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!guestName.trim() || !guestEmail.trim()) {
      setSubmitError(t("selectSlotFirst"));
      return;
    }
    setPending(true);
    setSubmitError(null);
    try {
      const accepted = await createPublicBooking(token, {
        guest_name: guestName.trim(),
        guest_email: guestEmail.trim(),
        starts_at: slot.starts_at,
        timezone,
        privacy_notice_version: data.privacy_notice_version,
        ...(memo.trim() ? { memo: memo.trim() } : {}),
      }, { idempotencyKey: bookingIdem.current });
      bookingIdem.current = idempotencyKey();
      onDone(accepted.data);
    } catch (cause) {
      const apiError = cause instanceof ApiError ? cause : null;
      if (apiError?.code === "SLOT_UNAVAILABLE") {
        setSubmitError(t("errorSlotGone"));
        onSlotGone();
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

  return <form className="flex max-w-md flex-col gap-5" onSubmit={(event) => void submit(event)}>
    <div className="flex items-center gap-2">
      <Button type="button" size="icon-sm" variant="ghost" onClick={onBack} aria-label={t("back")}><ArrowLeftIcon aria-hidden /></Button>
      <h2 className="text-lg font-semibold tracking-tight">{t("detailsTitle")}</h2>
    </div>
    <div className="grid gap-1.5"><Label htmlFor="booking-guest-name">{t("name")}</Label><Input ref={nameRef} id="booking-guest-name" className="h-11" autoComplete="name" required maxLength={300} value={guestName} onChange={(event) => setGuestName(event.target.value)} placeholder={t("namePlaceholder")} /></div>
    {invitee?.company_name ? (
      <div className="grid gap-1.5"><Label htmlFor="booking-guest-company">{t("company")}</Label><Input id="booking-guest-company" className="h-11 bg-muted/60" readOnly value={invitee.company_name} /></div>
    ) : null}
    <div className="grid gap-1.5">
      <Label htmlFor="booking-guest-email">{t("email")}</Label>
      <Input id="booking-guest-email" className={cn("h-11", invitee && "bg-muted/60")} type="email" inputMode="email" autoComplete="email" required maxLength={320} readOnly={Boolean(invitee)} aria-describedby={invitee ? "booking-guest-email-hint" : undefined} value={guestEmail} onChange={(event) => setGuestEmail(event.target.value)} placeholder={t("emailPlaceholder")} />
      {invitee ? <p id="booking-guest-email-hint" className="flex items-center gap-1.5 text-xs text-muted-foreground"><LockIcon className="size-3" aria-hidden />{t("emailLocked")}</p> : null}
    </div>
    <div className="grid gap-1.5"><Label htmlFor="booking-guest-memo">{t("memo")}</Label><Textarea ref={memoRef} id="booking-guest-memo" rows={4} maxLength={2000} value={memo} onChange={(event) => setMemo(event.target.value)} placeholder={t("memoPlaceholder")} /></div>
    {submitError ? <p role="alert" className="text-sm text-destructive">{submitError}</p> : null}
    <Button type="submit" size="xl" className="w-full sm:w-auto sm:self-start" disabled={pending}>{pending ? t("submitting") : t("submit")}</Button>
  </form>;
}

function BookingDone({ result, slot, timezone, t, locale }: {
  result: PublicBookingAccepted;
  slot: PublicSlot | null;
  timezone: string;
  t: Translator;
  locale: string;
}) {
  const startsAt = result.starts_at ?? slot?.starts_at;
  const endsAt = result.ends_at ?? slot?.ends_at;
  const zone = result.timezone ?? timezone;
  return <div className="mx-auto flex w-full max-w-md flex-1 flex-col items-center gap-6 pt-10 text-center sm:justify-center sm:pt-0 sm:pb-16">
    <span className="flex size-14 items-center justify-center rounded-full bg-success/15 text-success-on-subtle"><CheckIcon className="size-7" aria-hidden /></span>
    <div className="flex flex-col gap-2">
      <h1 className="text-2xl font-semibold tracking-tight">{t("confirmingTitle")}</h1>
      <p className="text-sm leading-relaxed text-muted-foreground break-keep">{result.message ?? t("confirmingBody")}</p>
    </div>
    {startsAt ? (
      <div className="w-full rounded-2xl border border-border bg-card p-4 text-sm">
        <p className="font-medium">{formatDayLabel(localDateOf(parseInstant(startsAt), zone), locale)}</p>
        <p className="tabular-nums">{formatTime(startsAt, { tz: zone, locale })}{endsAt ? ` – ${formatTime(endsAt, { tz: zone, locale })}` : ""}</p>
        <p className="mt-1 text-xs text-muted-foreground">{zone}</p>
      </div>
    ) : null}
    {result.manage_url ? (
      <div className="flex w-full flex-col items-center gap-2">
        <Button asChild size="lg" variant="outline" className="w-full"><Link href={result.manage_url}><LinkIcon aria-hidden />{t("manageLink")}</Link></Button>
        <p className="text-xs text-muted-foreground break-keep">{t("manageNote")}</p>
      </div>
    ) : null}
  </div>;
}

function ScreenSkeleton() {
  return <div className="mx-auto grid w-full max-w-5xl gap-8 lg:grid-cols-[320px_1fr]" aria-busy="true">
    <div className="flex flex-col gap-4"><Skeleton className="size-10 rounded-full" /><Skeleton className="h-8 w-56" /><Skeleton className="h-4 w-24" /></div>
    <div className="flex flex-col gap-4"><Skeleton className="h-6 w-48" /><Skeleton className="h-72 w-full rounded-2xl" /></div>
  </div>;
}

function Unavailable({ t }: { t: Translator }) {
  return <div className="mx-auto flex w-full max-w-sm flex-1 flex-col items-center gap-3 pt-10 text-center sm:justify-center sm:pt-0 sm:pb-16">
    <span className="flex size-12 items-center justify-center rounded-full bg-muted"><LinkIcon className="size-5 text-muted-foreground" aria-hidden /></span>
    <h1 className="text-xl font-semibold tracking-tight">{t("linkExpiredTitle")}</h1>
    <p className="text-sm text-muted-foreground break-keep">{t("linkExpiredBody")}</p>
  </div>;
}
