"use client";

import { useRef, useState, type FormEvent } from "react";
import { useLocale, useTranslations } from "next-intl";
import { useQuery } from "@tanstack/react-query";

import { ErrorState } from "@/components/patterns/error-state";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { ApiError } from "@/lib/api/problem";
import { cancelManagedBooking, getManagedBooking, rescheduleManagedBooking } from "@/lib/api/booking";
import { idempotencyKey } from "@/lib/api/client";
import { detectTimezone, formatDateTime, localInputToUtcIso, utcToLocalInput } from "@/features/meetings/datetime";

export function PublicManageScreen({ manageToken }: { manageToken: string }) {
  const t = useTranslations("booking.manage");
  const locale = useLocale();
  const [rescheduleOpen, setRescheduleOpen] = useState(false);
  const [cancelOpen, setCancelOpen] = useState(false);
  const [startsAt, setStartsAt] = useState("");
  const [timezone, setTimezone] = useState("");
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
    if (data?.starts_at) setStartsAt(utcToLocalInput(data.starts_at, zone));
    setTimezone(zone);
    setError(null);
    setNotice(null);
    setRescheduleOpen(true);
  }

  async function reschedule(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!data?.version) return;
    const utcStart = localInputToUtcIso(startsAt, timezone);
    if (!utcStart) {
      setError(t("errorGeneric"));
      return;
    }
    setPending(true);
    setError(null);
    try {
      await rescheduleManagedBooking(manageToken, { starts_at: utcStart, timezone, expected_version: data.version }, { idempotencyKey: idem.current });
      // A reschedule leaves the booking confirmed while its change job runs.
      // Briefly refresh so the displayed time catches up without a manual reload.
      changeRefreshUntil.current = Date.now() + 30_000;
      idem.current = idempotencyKey();
      setNotice(t("rescheduleDone"));
      setRescheduleOpen(false);
      await booking.refetch();
    } catch (cause) {
      const apiError = cause instanceof ApiError ? cause : null;
      setError(apiError?.status === 409 || apiError?.code === "SLOT_UNAVAILABLE" ? t("errorSlotGone") : apiError?.status === 412 ? t("versionConflict") : apiError?.problem.detail ?? t("errorGeneric"));
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

  if (booking.isPending) return <div className="flex flex-col gap-5" aria-busy="true"><Skeleton className="h-8 w-60" /><Skeleton className="h-12 w-full rounded-2xl" /><Skeleton className="h-32 w-full rounded-2xl" /></div>;
  if (booking.isError) {
    const invalid = booking.error instanceof ApiError && (booking.error.status === 404 || booking.error.status === 410);
    return invalid ? <div className="flex flex-col gap-2"><h1 className="text-2xl font-semibold">{t("title")}</h1><p className="text-sm text-muted-foreground">{t("notFound")}</p></div> : <ErrorState title={t("errorLoad")} error={booking.error} onRetry={() => void booking.refetch()} />;
  }
  if (!data) return null;

  const note = statusNote(data.status, t);
  return <div className="flex flex-col gap-5">
    <div><h1 className="text-2xl font-semibold">{t("title")}</h1><p className="mt-1 text-sm text-muted-foreground">{t("description")}</p></div>
    <div className="flex items-center gap-2 rounded-2xl border border-border bg-card px-4 py-3 text-sm"><span className="text-muted-foreground">{t("status")}</span><StatusBadge status={data.status} /></div>
    {note ? <p className="text-sm text-muted-foreground">{note}</p> : null}
    {notice ? <Alert variant="success"><AlertDescription>{notice}</AlertDescription></Alert> : null}
    {error ? <Alert variant="destructive"><AlertDescription>{error}</AlertDescription></Alert> : null}
    <dl className="grid gap-3 rounded-2xl border border-border bg-card px-4 py-3 text-sm sm:grid-cols-2">
      <Field label={t("meeting")} value={data.meeting_name} />
      <Field label={t("host")} value={data.host_display_name} />
      <Field label={t("guest")} value={data.guest_name} />
      <Field label={t("time")} value={data.starts_at ? formatDateTime(data.starts_at, { tz: zone, locale }) : undefined} />
      <Field label={t("timezone")} value={zone} />
    </dl>
    <div className="flex flex-wrap gap-2">
      {canReschedule ? <Button variant="outline" size="sm" onClick={openReschedule}>{t("reschedule")}</Button> : null}
      {canCancel ? <Button variant="destructive" size="sm" onClick={() => { setError(null); setCancelOpen(true); }}>{t("cancel")}</Button> : null}
      <Button variant="ghost" size="sm" onClick={() => void booking.refetch()}>{t("refresh")}</Button>
    </div>
    <Dialog open={rescheduleOpen} onOpenChange={setRescheduleOpen}>
      <DialogContent>
        <DialogHeader><DialogTitle>{t("rescheduleTitle")}</DialogTitle><DialogDescription>{t("rescheduleDescription")}</DialogDescription></DialogHeader>
        <form className="grid gap-3" onSubmit={(event) => void reschedule(event)}>
          <div className="grid gap-1"><Label htmlFor="manage-timezone">{t("rescheduleTimezone")}</Label><Input id="manage-timezone" value={timezone} onChange={(event) => setTimezone(event.target.value)} /></div>
          <div className="grid gap-1"><Label htmlFor="manage-start">{t("newStart")}</Label><Input id="manage-start" type="datetime-local" required value={startsAt} onChange={(event) => setStartsAt(event.target.value)} /></div>
          <p className="text-xs text-muted-foreground">{t("rescheduleDescription")}</p>
          <DialogFooter><Button type="button" variant="outline" onClick={() => setRescheduleOpen(false)}>{t("cancel")}</Button><Button type="submit" disabled={pending || !startsAt}>{pending ? t("rescheduleSubmitting") : t("rescheduleSubmit")}</Button></DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    <Dialog open={cancelOpen} onOpenChange={setCancelOpen}>
      <DialogContent>
        <DialogHeader><DialogTitle>{t("cancelTitle")}</DialogTitle><DialogDescription>{t("cancelDescription")}</DialogDescription></DialogHeader>
        {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
        <DialogFooter><Button variant="outline" onClick={() => setCancelOpen(false)}>{t("cancel")}</Button><Button variant="destructive" disabled={pending} onClick={() => void cancel()}>{pending ? t("cancelSubmitting") : t("cancelConfirm")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  </div>;
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
