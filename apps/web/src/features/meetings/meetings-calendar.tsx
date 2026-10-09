"use client";

import { useLocale, useTranslations } from "next-intl";
import { useMemo, useState } from "react";
import { ChevronLeftIcon, ChevronRightIcon } from "lucide-react";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import type { Booking } from "@/lib/api/meetings";
import {
  formatDayLabel,
  formatTime,
  localDateKey,
  localDateOf,
  parseInstant,
  sameLocalDate,
  shiftLocalDate,
  todayInZone,
  weekStartOf,
  localDateRange,
  type LocalDate,
} from "@/features/meetings/datetime";

interface MeetingsCalendarProps {
  bookings: Booking[];
  /** 표시 시간대 — 브라우저 감지값이 기본. */
  timezone: string;
  loading?: boolean;
  error?: Error | null;
  onRetry?: () => void;
  onOpen?: (bookingId: string) => void;
}

/**
 * 주간 달력 — 로컬 날짜 튜플 기준으로 7열을 만들고 각 예약을 tz 기준 로컬
 * 날짜에 배치한다. DST 전환 주에도 인스턴트→로컬 날짜 매핑만 쓰므로 안전하다.
 */
export function MeetingsCalendar({
  bookings,
  timezone,
  loading,
  error,
  onRetry,
  onOpen,
}: MeetingsCalendarProps) {
  const t = useTranslations("meetings.calendar");
  const locale = useLocale();
  const [weekAnchor, setWeekAnchor] = useState<LocalDate>(() =>
    weekStartOf(todayInZone(timezone))
  );

  const days = useMemo(() => localDateRange(weekAnchor, 7), [weekAnchor]);
  const today = useMemo(() => todayInZone(timezone), [timezone]);

  // 로컬 날짜 키 → 그 날짜에 시작하는 예약 (시작 시각순).
  const byDay = useMemo(() => {
    const map = new Map<string, Booking[]>();
    for (const b of bookings) {
      const at = parseInstant(b.starts_at);
      if (Number.isNaN(at)) continue;
      const key = localDateKey(localDateOf(at, timezone));
      const list = map.get(key) ?? [];
      list.push(b);
      map.set(key, list);
    }
    for (const list of map.values()) {
      list.sort((a, b) => parseInstant(a.starts_at) - parseInstant(b.starts_at));
    }
    return map;
  }, [bookings, timezone]);

  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <div className="flex items-center gap-1">
          <Button
            type="button"
            variant="outline"
            size="icon-sm"
            aria-label={t("prevWeek")}
            onClick={() => setWeekAnchor((w) => shiftLocalDate(w, -7))}
          >
            <ChevronLeftIcon className="size-4" aria-hidden />
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setWeekAnchor(weekStartOf(todayInZone(timezone)))}
          >
            {t("today")}
          </Button>
          <Button
            type="button"
            variant="outline"
            size="icon-sm"
            aria-label={t("nextWeek")}
            onClick={() => setWeekAnchor((w) => shiftLocalDate(w, 7))}
          >
            <ChevronRightIcon className="size-4" aria-hidden />
          </Button>
        </div>
        <p className="text-sm font-medium">
          {formatDayLabel(days[0]!, locale)} – {formatDayLabel(days[6]!, locale)}
        </p>
        <p className="ml-auto text-2xs text-muted-foreground">
          {t("timezoneNote", { tz: timezone })}
        </p>
      </div>

      {error ? (
        <ErrorState error={error} onRetry={onRetry} />
      ) : loading ? (
        <div className="grid grid-cols-7 gap-2" aria-busy="true">
          {Array.from({ length: 7 }).map((_, i) => (
            <Skeleton key={i} className="h-40 w-full" />
          ))}
        </div>
      ) : bookings.length === 0 ? (
        <EmptyState title={t("empty")} compact />
      ) : (
        <div
          className="grid grid-cols-7 gap-2 overflow-x-auto"
          role="grid"
          aria-label={t("timezoneNote", { tz: timezone })}
        >
          {days.map((day) => {
            const key = localDateKey(day);
            const dayBookings = byDay.get(key) ?? [];
            const isToday = sameLocalDate(day, today);
            return (
              <div
                key={key}
                role="gridcell"
                className={cn(
                  "flex min-w-[120px] flex-col gap-1.5 rounded-2xl border border-border p-2",
                  isToday && "border-ring/60 bg-accent/40"
                )}
              >
                <div
                  className={cn(
                    "text-2xs",
                    isToday
                      ? "font-semibold text-foreground"
                      : "text-muted-foreground"
                  )}
                >
                  {formatDayLabel(day, locale)}
                </div>
                {dayBookings.map((b) => (
                  <button
                    key={b.id}
                    type="button"
                    onClick={() => onOpen?.(b.id)}
                    className="flex flex-col gap-1 rounded-lg border border-border bg-card px-2 py-1.5 text-left outline-none transition-colors hover:bg-accent/60 focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-[3px]"
                  >
                    <span className="text-2xs tabular-nums text-muted-foreground">
                      {formatTime(b.starts_at, { tz: timezone, locale })}–
                      {formatTime(b.ends_at, { tz: timezone, locale })}
                    </span>
                    <span className="truncate text-xs font-medium">
                      {b.guest_name}
                    </span>
                    <StatusBadge status={b.status} className="scale-90 self-start" />
                  </button>
                ))}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
