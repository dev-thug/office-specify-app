"use client";

import { useMutation } from "@tanstack/react-query";
import { useLocale, useTranslations } from "next-intl";
import { useRef, useState } from "react";
import { toast } from "sonner";

import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { ApiError } from "@/lib/api/problem";
import { idempotencyKey } from "@/lib/api/client";
import {
  cancelBooking,
  recordBookingAttendance,
  rescheduleBooking,
  type Booking,
  type BookingAttendanceOutcome,
} from "@/lib/api/meetings";
import { meetingsErrorMessage } from "@/features/meetings/error-message";
import {
  detectTimezone,
  formatDateTime,
  localInputToUtcIso,
  utcToLocalInput,
} from "@/features/meetings/datetime";
import { TimezoneSelect } from "@/features/meetings/timezone-select";

interface DialogProps {
  booking: Booking;
  /** GET 상세에서 읽은 ETag/version — If-Match 로 보낸다. */
  etag: string | number | undefined;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onDone: () => void;
}

/**
 * 일정 변경 — 새 시작 시각을 선택한 시간대로 해석해 UTC로 보낸다.
 * 멱등 키는 이 대화상자의 한 번의 변경 작업당 하나: 확정 응답 후에만 재발급.
 */
export function RescheduleDialog({
  booking,
  etag,
  open,
  onOpenChange,
  onDone,
}: DialogProps) {
  const t = useTranslations("meetings");
  const tCommon = useTranslations("common");
  const locale = useLocale();
  const [tz, setTz] = useState(() => booking.guest_timezone ?? detectTimezone());
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const idemRef = useRef(idempotencyKey());

  const mutation = useMutation({
    mutationFn: (startsAt: string) =>
      rescheduleBooking(
        booking.id,
        { starts_at: startsAt },
        { etag: etag ?? "", idempotencyKey: idemRef.current }
      ),
    onSuccess: () => {
      toast.success(t("reschedule.success"));
      onDone();
      onOpenChange(false);
    },
    onError: (err) => {
      setError(meetingsErrorMessage(err, (k) => t(`errors.${k}`)));
      if (
        err instanceof ApiError &&
        err.problem.retryable !== true &&
        err.status < 500
      ) {
        idemRef.current = idempotencyKey();
      }
    },
  });

  const submit = () => {
    const iso = localInputToUtcIso(value, tz);
    if (!iso) {
      setError(t("reschedule.invalidTime"));
      return;
    }
    setError(null);
    mutation.mutate(iso);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("reschedule.title")}</DialogTitle>
          <DialogDescription>{t("reschedule.description")}</DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-4">
          <p className="text-sm text-muted-foreground">
            {t("reschedule.currentTime")}:{" "}
            <span className="font-medium text-foreground">
              {formatDateTime(booking.starts_at, { tz, locale })}
            </span>
          </p>
          <div className="grid gap-1.5">
            <Label htmlFor="reschedule-tz">{t("reschedule.timezone")}</Label>
            <TimezoneSelect
              id="reschedule-tz"
              value={tz}
              onValueChange={setTz}
              ariaLabel={t("reschedule.timezone")}
              pinned={booking.guest_timezone ? [booking.guest_timezone] : []}
              className="w-full"
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="reschedule-start">{t("reschedule.newStart")}</Label>
            <Input
              id="reschedule-start"
              type="datetime-local"
              value={value}
              onChange={(e) => setValue(e.target.value)}
              placeholder={utcToLocalInput(booking.starts_at, tz)}
            />
          </div>
          {error ? (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          ) : null}
        </div>
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            {tCommon("close")}
          </Button>
          <Button
            type="button"
            disabled={mutation.isPending || etag === undefined || etag === ""}
            onClick={submit}
          >
            {mutation.isPending ? t("reschedule.submitting") : t("reschedule.submit")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** 예약 취소 — 202 접수, 외부 확인 전 "canceling". */
export function CancelBookingDialog({
  booking,
  etag,
  open,
  onOpenChange,
  onDone,
}: DialogProps) {
  const t = useTranslations("meetings");
  const tCommon = useTranslations("common");
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const idemRef = useRef(idempotencyKey());

  const mutation = useMutation({
    mutationFn: () =>
      cancelBooking(
        booking.id,
        reason ? { reason } : {},
        { etag: etag ?? "", idempotencyKey: idemRef.current }
      ),
    onSuccess: () => {
      toast.success(t("cancel.success"));
      onDone();
      onOpenChange(false);
    },
    onError: (err) => {
      setError(meetingsErrorMessage(err, (k) => t(`errors.${k}`)));
      if (
        err instanceof ApiError &&
        err.problem.retryable !== true &&
        err.status < 500
      ) {
        idemRef.current = idempotencyKey();
      }
    },
  });

  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t("cancel.title")}</AlertDialogTitle>
          <AlertDialogDescription>
            {t("cancel.description")}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <div className="grid gap-1.5">
          <Label htmlFor="cancel-reason">{t("cancel.reason")}</Label>
          <Input
            id="cancel-reason"
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder={t("cancel.reasonPlaceholder")}
          />
          <p className="text-2xs text-muted-foreground">{t("cancel.note")}</p>
          {error ? (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          ) : null}
        </div>
        <AlertDialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            {tCommon("close")}
          </Button>
          <Button
            type="button"
            variant="destructive"
            disabled={mutation.isPending || etag === undefined || etag === ""}
            onClick={() => mutation.mutate()}
          >
            {mutation.isPending ? t("cancel.submitting") : t("cancel.confirm")}
          </Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

/** Record a human attendance outcome after the booking's end time. */
export function AttendanceDialog({ booking, etag, open, onOpenChange, onDone }: DialogProps) {
  const t = useTranslations("meetings");
  const tCommon = useTranslations("common");
  const [outcome, setOutcome] = useState<BookingAttendanceOutcome>(
    booking.attendance_status === "no_show" ? "no_show" : "attended"
  );
  const [note, setNote] = useState(booking.outcome_note ?? "");
  const [error, setError] = useState<string | null>(null);
  const idemRef = useRef(idempotencyKey());

  const mutation = useMutation({
    mutationFn: () => recordBookingAttendance(
      booking.id,
      { attendance_status: outcome, outcome_note: note.trim() || undefined },
      { etag: etag ?? "", idempotencyKey: idemRef.current }
    ),
    onSuccess: () => {
      toast.success(t("attendance.success"));
      onDone();
      onOpenChange(false);
      idemRef.current = idempotencyKey();
    },
    onError: (cause) => {
      setError(meetingsErrorMessage(cause, (key) => t(`errors.${key}`)));
      if (cause instanceof ApiError && cause.problem.retryable !== true && cause.status < 500) idemRef.current = idempotencyKey();
    },
  });

  return <Dialog open={open} onOpenChange={onOpenChange}>
    <DialogContent>
      <DialogHeader><DialogTitle>{t("attendance.title")}</DialogTitle><DialogDescription>{t("attendance.description")}</DialogDescription></DialogHeader>
      <div className="flex flex-col gap-2" role="radiogroup" aria-label={t("attendance.title")}>
        {([ ["attended", t("attendance.attended")], ["no_show", t("attendance.noShow")] ] as const).map(([value, label]) => <label key={value} className="flex cursor-pointer items-center gap-2 rounded-lg border border-border px-3 py-2 text-sm has-checked:border-ring has-checked:bg-accent/50"><input type="radio" name="attendance-outcome" value={value} checked={outcome === value} onChange={() => setOutcome(value)} className="accent-primary" />{label}</label>)}
      </div>
      <div className="grid gap-1"><Label htmlFor="attendance-note">{t("attendance.note")}</Label><Textarea id="attendance-note" value={note} onChange={(event) => setNote(event.target.value)} maxLength={2000} rows={3} placeholder={t("attendance.notePlaceholder")} /></div>
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      <DialogFooter>
        <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>{tCommon("close")}</Button>
        <Button type="button" disabled={mutation.isPending || etag === undefined || etag === ""} onClick={() => mutation.mutate()}>{mutation.isPending ? t("attendance.submitting") : t("attendance.submit")}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}
