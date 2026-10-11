"use client";

import { useRef, useState, type FormEvent } from "react";
import { MapPinIcon, VideoIcon } from "lucide-react";
import { useLocale, useTranslations } from "next-intl";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { idempotencyKey } from "@/lib/api/client";
import { getMe } from "@/lib/api/auth";
import { readEtag } from "@/lib/api/mutate";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { CALENDAR_CONNECTIONS_PATH, MEETING_TYPES_PATH, MEETING_TYPES_LIST_LIMIT, MEETING_TYPES_QUERY_KEY, createMeetingType, getHostAvailability, principalDisplayName, putHostAvailability, updateMeetingType, type CalendarConnectionRef, type MeetingType, type MeetingTypeCreateInput, type MeetingTypePatchInput, type PrincipalRef } from "@/lib/api/meetings";
import { ApiError } from "@/lib/api/problem";
import { detectTimezone, formatDateTime } from "@/features/meetings/datetime";

type EditableMeetingType = Omit<MeetingTypeCreateInput, "calendar_connection_id"> & { status?: "active" | "disabled" };

export function MeetingTypesScreen() {
  const t = useTranslations("meetings.types");
  const common = useTranslations("common");
  const locale = useLocale();
  const queryClient = useQueryClient();
  const [tab, setTab] = useState("types");
  const [dialogOpen, setDialogOpen] = useState(false);
  const [editing, setEditing] = useState<MeetingType | null>(null);
  const [calendarId, setCalendarId] = useState("");
  const me = useQuery({
    queryKey: ["me"],
    queryFn: async () => (await getMe()).data,
    retry: false,
    staleTime: 60_000,
  });
  const currentPrincipalId = me.data?.principal_id ?? "";
  const [form, setForm] = useState<EditableMeetingType>({
    name: "",
    description: "",
    duration_minutes: 30,
    buffer_before_minutes: 10,
    buffer_after_minutes: 10,
    minimum_notice_minutes: 1440,
    booking_horizon_days: 30,
    location_kind: "online",
    location_detail: "",
  });
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const idem = useRef(idempotencyKey());

  const list = useCursorList<MeetingType>({
    queryKey: MEETING_TYPES_QUERY_KEY,
    path: MEETING_TYPES_PATH,
    limit: MEETING_TYPES_LIST_LIMIT,
  });
  const types = list.data?.pages.flatMap((page) => page.data ?? []) ?? [];
  const principals = useCursorList<PrincipalRef>({
    queryKey: ["meetings", "principals"],
    path: "/api/v1/principals",
    limit: MEETING_TYPES_LIST_LIMIT,
  });
  const calendars = useCursorList<CalendarConnectionRef>({
    queryKey: ["meetings", "calendar-connections"],
    path: CALENDAR_CONNECTIONS_PATH,
    limit: MEETING_TYPES_LIST_LIMIT,
  });
  const humans = principals.data?.pages.flatMap((page) => page.data ?? []).filter((person) => person.kind === "human" && person.status !== "disabled") ?? [];
  const activeCalendars = calendars.data?.pages.flatMap((page) => page.data ?? []).filter((calendar) => calendar.status === "active") ?? [];
  const hostCalendars = activeCalendars.filter((calendar) => calendar.host_principal_id === currentPrincipalId);
  const currentHuman = humans.find((person) => person.id === currentPrincipalId);
  const currentHostLabel = principalDisplayName(currentHuman) ?? me.data?.display_name ?? common("nameUnavailable");
  const defaultCalendarId = hostCalendars[0]?.id ?? "";
  const selectedCalendarId = calendarId || defaultCalendarId;
  const lastPage = list.data?.pages.at(-1);

  const columns: ColumnDef<MeetingType>[] = [
    { key: "name", header: t("colName"), cell: (row) => <span className="font-medium">{row.name}</span> },
    { key: "duration", header: t("colDuration"), cell: (row) => t("minutes", { count: row.duration_minutes }) },
    { key: "location", header: t("colLocation"), cell: (row) => <span className="flex items-center gap-1.5">{row.location_kind === "offline" ? <MapPinIcon className="size-3.5 text-muted-foreground" aria-hidden /> : <VideoIcon className="size-3.5 text-muted-foreground" aria-hidden />}{row.location_kind === "offline" ? t("locationOffline") : t("locationOnline")}</span> },
    { key: "buffers", header: t("colBuffers"), cell: (row) => `${row.buffer_before_minutes} / ${row.buffer_after_minutes}` },
    { key: "notice", header: t("colNotice"), cell: (row) => t("minutes", { count: row.minimum_notice_minutes }) },
    { key: "horizon", header: t("colHorizon"), cell: (row) => t("days", { count: row.booking_horizon_days }) },
    { key: "host", header: t("colHost"), cell: (row) => principalDisplayName(humans.find((person) => person.id === row.host_principal_id)) ?? common("nameUnavailable") },
    { key: "status", header: t("colStatus"), cell: (row) => <StatusBadge status={row.status} /> },
    { key: "actions", header: t("colActions"), cell: (row) => <Button variant="outline" size="xs" onClick={() => openEdit(row)}>{t("edit")}</Button> },
  ];

  function openCreate() {
    setEditing(null);
    setCalendarId(defaultCalendarId);
    setForm({ name: "", description: "", duration_minutes: 30, buffer_before_minutes: 10, buffer_after_minutes: 10, minimum_notice_minutes: 1440, booking_horizon_days: 30, location_kind: "online", location_detail: "" });
    setError(null);
    setDialogOpen(true);
  }

  function openEdit(item: MeetingType) {
    setEditing(item);
    setCalendarId(item.calendar_connection_id);
    setForm({
      name: item.name,
      description: item.description ?? "",
      duration_minutes: item.duration_minutes,
      buffer_before_minutes: item.buffer_before_minutes,
      buffer_after_minutes: item.buffer_after_minutes,
      minimum_notice_minutes: item.minimum_notice_minutes,
      booking_horizon_days: item.booking_horizon_days,
      location_kind: item.location_kind ?? "online",
      location_detail: item.location_detail ?? "",
      status: item.status as "active" | "disabled",
    });
    setError(null);
    setDialogOpen(true);
  }

  function setNumber(field: keyof EditableMeetingType, value: string) {
    setForm((state) => ({ ...state, [field]: value === "" ? 0 : Number(value) }));
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!form.name.trim()) return;
    setPending(true);
    setError(null);
    try {
      if (editing) {
        const patch: MeetingTypePatchInput = {
          name: form.name.trim(),
          description: form.description?.trim() || null,
          duration_minutes: form.duration_minutes,
          buffer_before_minutes: form.buffer_before_minutes,
          buffer_after_minutes: form.buffer_after_minutes,
          minimum_notice_minutes: form.minimum_notice_minutes,
          booking_horizon_days: form.booking_horizon_days,
          location_kind: form.location_kind,
          location_detail: form.location_detail?.trim() || null,
          status: form.status as "active" | "disabled" | undefined,
        };
        await updateMeetingType(editing.id, patch, { etag: editing.version ?? 0, idempotencyKey: idem.current });
      } else {
        if (!currentPrincipalId || !selectedCalendarId) {
          setError(t("calendarNone"));
          return;
        }
        const input: MeetingTypeCreateInput = {
          ...form,
          name: form.name.trim(),
          description: form.description?.trim() || undefined,
          location_detail: form.location_detail?.trim() || undefined,
          calendar_connection_id: selectedCalendarId,
        };
        await createMeetingType(input, { idempotencyKey: idem.current });
      }
      idem.current = idempotencyKey();
      setDialogOpen(false);
      await queryClient.invalidateQueries({ queryKey: MEETING_TYPES_QUERY_KEY });
    } catch (cause) {
      const apiError = cause instanceof ApiError ? cause : null;
      setError(apiError?.status === 412 || apiError?.status === 428 ? t("conflictBody") : apiError?.problem.detail ?? t("errors.generic"));
      if (apiError && apiError.problem.retryable !== true && apiError.status < 500) idem.current = idempotencyKey();
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader title={t("pageTitle")} description={t("pageDescription")} actions={<Button size="sm" onClick={openCreate}>{t("create")}</Button>} />
      <Tabs value={tab} onValueChange={setTab}>
        <TabsList><TabsTrigger value="types">{t("tabTypes")}</TabsTrigger><TabsTrigger value="availability">{t("tabAvailability")}</TabsTrigger></TabsList>
        <TabsContent value="types" className="pt-4">
          <DataTable
            caption={t("caption")}
            columns={columns}
            rows={types}
            rowKey={(row) => row.id}
            loading={list.isPending}
            error={list.error}
            onRetry={() => void list.refetch()}
            emptyState={<EmptyState title={t("emptyTitle")} description={t("emptyDescription")} />}
            page={lastPage?.page}
            onLoadMore={() => void list.fetchNextPage()}
            loadingMore={list.isFetchingNextPage}
          />
        </TabsContent>
        <TabsContent value="availability" className="pt-4">
          <AvailabilityEditor hosts={humans} t={t} locale={locale} />
        </TabsContent>
      </Tabs>
      <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
        <DialogContent>
          <DialogHeader><DialogTitle>{editing ? t("editTitle") : t("createTitle")}</DialogTitle><DialogDescription>{editing ? t("fieldHostFixed") : t("pageDescription")}</DialogDescription></DialogHeader>
          <form className="grid gap-3" onSubmit={(event) => void submit(event)}>
            <div className="grid gap-1"><Label htmlFor="meeting-type-name">{t("fieldName")}</Label><Input id="meeting-type-name" required value={form.name} onChange={(event) => setForm((state) => ({ ...state, name: event.target.value }))} maxLength={300} /></div>
            <div className="grid gap-1"><Label htmlFor="meeting-type-description">{t("fieldDescription")}</Label><Input id="meeting-type-description" value={form.description ?? ""} onChange={(event) => setForm((state) => ({ ...state, description: event.target.value }))} maxLength={2000} /></div>
            <fieldset className="grid gap-2">
              <legend className="mb-1 text-sm font-medium">{t("fieldLocation")}</legend>
              <div role="radiogroup" aria-label={t("fieldLocation")} className="grid grid-cols-2 gap-2">
                {(["online", "offline"] as const).map((kind) => (
                  <Button key={kind} type="button" role="radio" aria-checked={form.location_kind === kind} variant={form.location_kind === kind ? "default" : "outline"} onClick={() => setForm((state) => ({ ...state, location_kind: kind }))}>
                    {kind === "online" ? <VideoIcon aria-hidden /> : <MapPinIcon aria-hidden />}
                    {kind === "online" ? t("locationOnline") : t("locationOffline")}
                  </Button>
                ))}
              </div>
              <Label htmlFor="meeting-type-location-detail" className="mt-1">{form.location_kind === "offline" ? t("fieldLocationAddress") : t("fieldLocationLink")}</Label>
              <Input id="meeting-type-location-detail" value={form.location_detail ?? ""} onChange={(event) => setForm((state) => ({ ...state, location_detail: event.target.value }))} maxLength={500} placeholder={form.location_kind === "offline" ? t("fieldLocationAddressPlaceholder") : t("fieldLocationLinkPlaceholder")} />
              <p className="text-2xs text-muted-foreground">{form.location_kind === "offline" ? t("fieldLocationAddressHint") : t("fieldLocationLinkHint")}</p>
            </fieldset>
            {!editing ? <>
              <div className="grid gap-1"><Label htmlFor="meeting-type-host">{t("fieldHost")}</Label><Input id="meeting-type-host" readOnly value={currentHostLabel} /><p className="text-2xs text-muted-foreground">{t("fieldHostCurrent")}</p></div>
              <div className="grid gap-1"><Label htmlFor="meeting-type-calendar">{t("fieldCalendar")}</Label><Select value={selectedCalendarId} onValueChange={setCalendarId} disabled={!currentPrincipalId || calendars.isLoading || calendars.isError}><SelectTrigger id="meeting-type-calendar"><SelectValue placeholder={t("fieldCalendar")} /></SelectTrigger><SelectContent>{hostCalendars.map((calendar) => <SelectItem key={calendar.id} value={calendar.id}>{calendar.provider === "office_calendar" ? t("calendarOffice") : calendar.name ?? calendar.provider ?? common("nameUnavailable")}</SelectItem>)}</SelectContent></Select>
                {calendars.isLoading ? <p role="status" className="text-xs text-muted-foreground">{common("table.loading")}</p> : null}
                {calendars.isError ? <div role="alert" className="flex items-center gap-2 text-xs text-destructive"><p>{t("calendarLoadFailed")}</p><Button type="button" variant="link" size="xs" className="h-auto p-0 text-destructive" onClick={() => void calendars.refetch()}>{common("retry")}</Button></div> : null}
                {calendars.isSuccess && hostCalendars.length === 0 ? <p className="text-xs text-muted-foreground">{t("calendarNone")}</p> : null}
                {calendars.hasNextPage ? <Button type="button" variant="link" size="xs" className="h-auto w-fit p-0" disabled={calendars.isFetchingNextPage} onClick={() => void calendars.fetchNextPage()}>{calendars.isFetchingNextPage ? common("table.loading") : common("table.loadMore")}</Button> : null}
              </div>
            </> : null}
            <div className="grid gap-3 sm:grid-cols-2">
              <NumberField id="meeting-type-duration" label={t("fieldDuration")} min={5} max={480} value={form.duration_minutes} onChange={(value) => setNumber("duration_minutes", value)} />
              <NumberField id="meeting-type-before" label={t("fieldBufferBefore")} min={0} max={480} value={form.buffer_before_minutes} onChange={(value) => setNumber("buffer_before_minutes", value)} />
              <NumberField id="meeting-type-after" label={t("fieldBufferAfter")} min={0} max={480} value={form.buffer_after_minutes} onChange={(value) => setNumber("buffer_after_minutes", value)} />
              <NumberField id="meeting-type-notice" label={t("fieldMinNotice")} min={0} max={525600} value={form.minimum_notice_minutes} onChange={(value) => setNumber("minimum_notice_minutes", value)} />
              <NumberField id="meeting-type-horizon" label={t("fieldHorizon")} min={1} max={365} value={form.booking_horizon_days} onChange={(value) => setNumber("booking_horizon_days", value)} />
            </div>
            {editing ? <div className="grid gap-1"><Label htmlFor="meeting-type-status">{t("fieldStatus")}</Label><Select value={String(form.status ?? "active")} onValueChange={(value) => setForm((state) => ({ ...state, status: value as "active" | "disabled" }))}><SelectTrigger id="meeting-type-status"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="active">{t("statusActive")}</SelectItem><SelectItem value="disabled">{t("statusDisabled")}</SelectItem></SelectContent></Select></div> : null}
            {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
            <DialogFooter><Button type="button" variant="outline" onClick={() => setDialogOpen(false)}>{common("cancel")}</Button><Button type="submit" disabled={pending || !form.name.trim() || (!editing && (!currentPrincipalId || !selectedCalendarId))}>{pending ? t("saving") : t("save")}</Button></DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function NumberField({ id, label, value, min, max, onChange }: { id: string; label: string; value: number; min: number; max: number; onChange: (value: string) => void }) {
  return <div className="grid gap-1"><Label htmlFor={id}>{label}</Label><Input id={id} type="number" min={min} max={max} value={value} onChange={(event) => onChange(event.target.value)} /></div>;
}

function AvailabilityEditor({ hosts, t, locale }: { hosts: PrincipalRef[]; t: ReturnType<typeof useTranslations>; locale: string }) {
  const common = useTranslations("common");
  const client = useQueryClient();
  const [hostId, setHostId] = useState("");
  const [rulesDraft, setRulesDraft] = useState<Array<{ weekday: number; start_local: string; end_local: string; timezone: string }> | null>(null);
  const [exceptionsDraft, setExceptionsDraft] = useState<Array<{ starts_at: string; ends_at: string; kind: "blocked" | "available"; reason?: string }> | null>(null);
  const [timezoneOverride, setTimezoneOverride] = useState<string | null>(null);
  const [ruleDay, setRuleDay] = useState("1");
  const [ruleStart, setRuleStart] = useState("09:00");
  const [ruleEnd, setRuleEnd] = useState("17:00");
  const [exceptionStart, setExceptionStart] = useState("");
  const [exceptionEnd, setExceptionEnd] = useState("");
  const [exceptionKind, setExceptionKind] = useState<"blocked" | "available">("blocked");
  const [exceptionReason, setExceptionReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const availability = useQuery({
    queryKey: ["meetings", "availability", hostId],
    enabled: Boolean(hostId),
    queryFn: () => getHostAvailability(hostId),
  });
  const savedRules = availability.data?.data.rules.map(({ weekday, start_local, end_local, timezone: tz }) => ({ weekday, start_local: start_local.slice(0, 5), end_local: end_local.slice(0, 5), timezone: tz })) ?? [];
  const savedExceptions = availability.data?.data.exceptions.map(({ starts_at, ends_at, kind, reason }) => ({ starts_at, ends_at, kind: kind as "blocked" | "available", reason: reason ?? undefined })) ?? [];
  const rules = rulesDraft ?? savedRules;
  const exceptions = exceptionsDraft ?? savedExceptions;
  const timezone = timezoneOverride ?? rules[0]?.timezone ?? detectTimezone();

  function addRule() {
    if (!timezone.trim() || ruleEnd <= ruleStart) {
      setError(t("invalidRule"));
      return;
    }
    setError(null);
    setRulesDraft((values) => [...(values ?? rules), { weekday: Number(ruleDay), start_local: ruleStart, end_local: ruleEnd, timezone: timezone.trim() }].sort((a, b) => a.weekday - b.weekday || a.start_local.localeCompare(b.start_local)));
  }

  function addException() {
    if (!exceptionStart || !exceptionEnd || new Date(exceptionEnd).getTime() <= new Date(exceptionStart).getTime()) {
      setError(t("invalidException"));
      return;
    }
    setError(null);
    setExceptionsDraft((values) => [...(values ?? exceptions), { starts_at: new Date(exceptionStart).toISOString(), ends_at: new Date(exceptionEnd).toISOString(), kind: exceptionKind, reason: exceptionReason.trim() || undefined }]);
    setExceptionStart("");
    setExceptionEnd("");
    setExceptionReason("");
  }

  async function save() {
    if (!hostId || !availability.data) return;
    setSaving(true);
    setError(null);
    setNotice(null);
    try {
      await putHostAvailability(hostId, { rules, exceptions }, { etag: readEtag(availability.data) ?? availability.data.data.version, idempotencyKey: idempotencyKey() });
      setNotice(t("saved"));
      setRulesDraft(null);
      setExceptionsDraft(null);
      setTimezoneOverride(null);
      await client.invalidateQueries({ queryKey: ["meetings", "availability", hostId] });
    } catch (cause) {
      const apiError = cause instanceof ApiError ? cause : null;
      setError(apiError?.status === 412 || apiError?.status === 428 ? t("conflict") : apiError?.problem.detail ?? t("errors.generic"));
    } finally {
      setSaving(false);
    }
  }

  return <div className="flex flex-col gap-4 rounded-2xl border border-border bg-card p-4">
    <div className="grid gap-3 sm:grid-cols-2">
      <div className="grid gap-1"><Label htmlFor="availability-host">{t("host")}</Label><Select value={hostId} onValueChange={(value) => { setHostId(value); setRulesDraft(null); setExceptionsDraft(null); setTimezoneOverride(null); setError(null); }}><SelectTrigger id="availability-host"><SelectValue placeholder={t("host")} /></SelectTrigger><SelectContent>{hosts.map((host) => <SelectItem key={host.id} value={host.id}>{principalDisplayName(host) ?? common("nameUnavailable")}</SelectItem>)}</SelectContent></Select></div>
      <div className="grid gap-1"><Label htmlFor="availability-timezone">{t("timezone")}</Label><Input id="availability-timezone" value={timezone} onChange={(event) => setTimezoneOverride(event.target.value)} /></div>
    </div>
    {availability.isPending && hostId ? <Skeleton className="h-36 w-full rounded-xl" /> : null}
    {availability.isError ? <ErrorState error={availability.error} onRetry={() => void availability.refetch()} /> : null}
    {hostId && availability.data ? <>
      <section className="grid gap-3">
        <div><h2 className="text-sm font-semibold">{t("rules")}</h2><p className="text-xs text-muted-foreground">{t("rulesNote")}</p></div>
        {rules.length === 0 ? <p className="text-sm text-muted-foreground">{t("noRule")}</p> : rules.map((rule, index) => <div key={`${rule.weekday}-${rule.start_local}-${index}`} className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border px-3 py-2 text-sm"><span>{t(`weekdays.${rule.weekday}` as never)} · {rule.start_local}–{rule.end_local} · {rule.timezone}</span><Button variant="ghost" size="xs" onClick={() => setRulesDraft((values) => (values ?? rules).filter((_, i) => i !== index))}>{t("removeRule")}</Button></div>)}
        <div className="grid gap-2 rounded-xl bg-muted/40 p-3 sm:grid-cols-4">
          <div className="grid gap-1"><Label htmlFor="availability-weekday">{t("weekday")}</Label><Select value={ruleDay} onValueChange={setRuleDay}><SelectTrigger id="availability-weekday"><SelectValue /></SelectTrigger><SelectContent>{[1,2,3,4,5,6,7].map((day) => <SelectItem key={day} value={String(day)}>{t(`weekdays.${day}` as never)}</SelectItem>)}</SelectContent></Select></div>
          <div className="grid gap-1"><Label htmlFor="availability-start">{t("ruleFrom")}</Label><Input id="availability-start" type="time" value={ruleStart} onChange={(event) => setRuleStart(event.target.value)} /></div>
          <div className="grid gap-1"><Label htmlFor="availability-end">{t("ruleTo")}</Label><Input id="availability-end" type="time" value={ruleEnd} onChange={(event) => setRuleEnd(event.target.value)} /></div>
          <div className="flex items-end"><Button className="w-full" variant="outline" size="sm" onClick={addRule}>{t("addRule")}</Button></div>
        </div>
      </section>
      <section className="grid gap-3">
        <div><h2 className="text-sm font-semibold">{t("exceptions")}</h2><p className="text-xs text-muted-foreground">{t("exceptionsNote")}</p></div>
        {exceptions.length === 0 ? <p className="text-sm text-muted-foreground">{t("exceptionsEmpty")}</p> : exceptions.map((exception, index) => <div key={`${exception.starts_at}-${index}`} className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border px-3 py-2 text-sm"><span><StatusBadge status={exception.kind} /> {formatDateTime(exception.starts_at, { tz: timezone, locale })} – {formatDateTime(exception.ends_at, { tz: timezone, locale })}{exception.reason ? ` · ${exception.reason}` : ""}</span><Button variant="ghost" size="xs" onClick={() => setExceptionsDraft((values) => (values ?? exceptions).filter((_, i) => i !== index))}>{t("removeException")}</Button></div>)}
        <div className="grid gap-2 rounded-xl bg-muted/40 p-3 sm:grid-cols-2">
          <div className="grid gap-1"><Label htmlFor="exception-kind">{t("exceptionKind")}</Label><Select value={exceptionKind} onValueChange={(value) => setExceptionKind(value as "blocked" | "available")}><SelectTrigger id="exception-kind"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="blocked">{t("kindBlocked")}</SelectItem><SelectItem value="available">{t("kindAvailable")}</SelectItem></SelectContent></Select></div>
          <div className="grid gap-1"><Label htmlFor="exception-reason">{t("exceptionReason")}</Label><Input id="exception-reason" value={exceptionReason} onChange={(event) => setExceptionReason(event.target.value)} /></div>
          <div className="grid gap-1"><Label htmlFor="exception-start">{t("exceptionStart")}</Label><Input id="exception-start" type="datetime-local" value={exceptionStart} onChange={(event) => setExceptionStart(event.target.value)} /></div>
          <div className="grid gap-1"><Label htmlFor="exception-end">{t("exceptionEnd")}</Label><Input id="exception-end" type="datetime-local" value={exceptionEnd} onChange={(event) => setExceptionEnd(event.target.value)} /></div>
          <Button variant="outline" size="sm" onClick={addException}>{t("addException")}</Button>
          <p className="self-center text-xs text-muted-foreground">{t("exceptionInTz")}</p>
        </div>
      </section>
      {error ? <Alert variant="destructive"><AlertDescription>{error}</AlertDescription></Alert> : null}
      {notice ? <Alert variant="success"><AlertDescription>{notice}</AlertDescription></Alert> : null}
      <div className="flex justify-end gap-2"><Button variant="outline" onClick={() => { setRulesDraft(null); setExceptionsDraft(null); setTimezoneOverride(null); void availability.refetch(); }}>{t("reload")}</Button><Button disabled={saving} onClick={() => void save()}>{saving ? common("loading") : t("save")}</Button></div>
    </> : null}
  </div>;
}
