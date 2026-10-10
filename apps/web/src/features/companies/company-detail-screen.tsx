"use client";

import Link from "next/link";
import { useRef, useState } from "react";
import { useLocale, useTranslations } from "next-intl";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";

import { ActivityFeed } from "@/features/companies/activity-feed";
import { CompanyFormDialog } from "@/features/companies/company-form-dialog";
import { principalName, useHumanPrincipals } from "@/features/companies/owner-select";
import { ContactFormDialog } from "@/features/contacts/contact-form-dialog";
import { MergeRequestDialog } from "@/features/companies/merge-request-dialog";
import { NotesPanel } from "@/features/companies/notes-panel";
import { TasksPanel } from "@/features/companies/tasks-panel";
import { ResearchEvidencePanel } from "@/features/research";
import { activityGroup, formatDateTime } from "@/features/companies/utils";
import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { idempotencyKey } from "@/lib/api/client";
import { archiveCompany, companyActivitiesPath, getCompany, allowsAction, ACTION_ADD_CONTACT, ACTION_ADD_NOTE, ACTION_ADD_TASK, ACTION_ARCHIVE, ACTION_MERGE, ACTION_UPDATE, type CompanyActivity } from "@/lib/api/companies";
import { readEtag } from "@/lib/api/mutate";
import { type Contact, contactListParams, contactsPath } from "@/lib/api/contacts";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { ApiError } from "@/lib/api/problem";

export function CompanyDetailScreen({ companyId }: { companyId: string }) {
  const t = useTranslations("companies");
  const tc = useTranslations("common");
  const locale = useLocale();
  const queryClient = useQueryClient();
  const principals = useHumanPrincipals();
  const etag = useRef<string | undefined>(undefined);
  const [activeTab, setActiveTab] = useState("overview");
  const [editOpen, setEditOpen] = useState(false);
  const [addContactOpen, setAddContactOpen] = useState(false);
  const [mergeOpen, setMergeOpen] = useState(false);
  const [archiveOpen, setArchiveOpen] = useState(false);
  const [archiveReason, setArchiveReason] = useState("");
  const [archiveError, setArchiveError] = useState<string | null>(null);
  const [archiving, setArchiving] = useState(false);
  const archiveIdem = useRef(idempotencyKey());

  const detail = useQuery({
    queryKey: ["companies", "detail", companyId],
    queryFn: async () => {
      const result = await getCompany(companyId);
      etag.current = readEtag(result) ?? String(result.data.version);
      return result.data;
    },
    retry: false,
  });
  const company = detail.data;
  const activitiesPath = companyActivitiesPath(companyId);
  const contacts = useCursorList<Contact>({
    queryKey: ["contacts", "company", companyId],
    path: contactsPath(),
    params: contactListParams({ company_id: companyId }),
    enabled: Boolean(company),
  });
  const contactRows = contacts.data?.pages.flatMap((page) => page.data ?? []) ?? [];

  async function archive() {
    if (!company) return;
    setArchiving(true);
    setArchiveError(null);
    try {
      await archiveCompany(company.id, { reason: archiveReason.trim() || undefined }, {
        etag: etag.current ?? company.version,
        idempotencyKey: archiveIdem.current,
      });
      archiveIdem.current = idempotencyKey();
      setArchiveOpen(false);
      toast.success(t("detail.archived"));
      await queryClient.invalidateQueries({ queryKey: ["companies"] });
      await detail.refetch();
    } catch (cause) {
      const apiError = cause instanceof ApiError ? cause : null;
      setArchiveError(apiError?.problem.detail ?? t("detail.archiveError"));
      if (apiError && apiError.problem.retryable !== true && apiError.status < 500) archiveIdem.current = idempotencyKey();
    } finally {
      setArchiving(false);
    }
  }

  if (detail.isPending) {
    return <div className="mx-auto flex w-full max-w-6xl flex-col gap-4 px-6 py-6" aria-busy="true"><Skeleton className="h-8 w-72" /><Skeleton className="h-28 w-full rounded-2xl" /><Skeleton className="h-72 w-full rounded-2xl" /></div>;
  }
  if (detail.isError) return <div className="mx-auto max-w-6xl px-6 py-6"><ErrorState error={detail.error} onRetry={() => void detail.refetch()} /></div>;
  if (!company) return <div className="mx-auto max-w-6xl px-6 py-6"><EmptyState title={t("detail.notFound")} /></div>;

  const can = (...actions: string[]) => allowsAction(company.allowed_actions, ...actions);
  const tabKeys = ["overview", "contacts", "activity", "emails", "meetings", "research", "notes", "tasks"] as const;
  const activityFilter = (group: string) => (item: CompanyActivity) => activityGroup(item) === group;

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader title={company.name} description={company.description ?? company.domain ?? undefined} actions={<>
        {can(...ACTION_UPDATE) ? <Button variant="outline" size="sm" onClick={() => setEditOpen(true)}>{t("detail.edit")}</Button> : null}
        {can(...ACTION_MERGE) ? <Button variant="outline" size="sm" onClick={() => setMergeOpen(true)}>{t("detail.mergeRequest")}</Button> : null}
        {can(...ACTION_ARCHIVE) && !company.archived_at ? <Button variant="destructive" size="sm" onClick={() => setArchiveOpen(true)}>{t("detail.archive")}</Button> : null}
      </>} />
      {company.archived_at ? <Alert variant="warning"><AlertDescription>{t("detail.archivedBanner")}</AlertDescription></Alert> : null}
      <Tabs value={activeTab} onValueChange={setActiveTab}>
        <TabsList className="flex h-auto flex-wrap">
          {tabKeys.map((key) => <TabsTrigger key={key} value={key}>{t(`tabs.${key}`)}</TabsTrigger>)}
        </TabsList>
        <TabsContent value="overview" className="pt-4">
          <div className="grid gap-4 lg:grid-cols-[minmax(0,2fr)_minmax(16rem,1fr)]">
            <Card>
              <CardHeader><CardTitle>{company.name}</CardTitle></CardHeader>
              <CardContent>
                <dl className="grid gap-x-4 gap-y-3 text-sm sm:grid-cols-2">
                  <Field label={t("detail.fields.domain")} value={company.domain} />
                  <Field label={t("detail.fields.website")} value={company.website_url} />
                  <Field label={t("detail.fields.industry")} value={company.industry} />
                  <Field label={t("detail.fields.country")} value={company.country_code} />
                  <div className="grid gap-1"><dt className="text-xs text-muted-foreground">{t("detail.fields.stage")}</dt><dd><StatusBadge status={company.lifecycle_stage} /></dd></div>
                  <Field label={t("detail.fields.owner")} value={company.owner_display_name?.trim() || principalName(principals.data?.find((p) => p.id === company.owner_principal_id), company.owner_principal_id) || (company.owner_principal_id ? tc("nameUnavailable") : t("detail.unassigned"))} />
                  <Field label={t("detail.fields.createdAt")} value={formatDateTime(company.created_at, locale)} />
                  <Field label={t("detail.fields.updatedAt")} value={formatDateTime(company.updated_at, locale)} />
                </dl>
              </CardContent>
            </Card>
            <Card>
              <CardHeader><CardTitle>{t("activity.title")}</CardTitle></CardHeader>
              <CardContent><ActivityFeed path={activitiesPath} ns="companies" emptyTitle={t("activity.emptyTitle")} emptyDescription={t("activity.emptyDescription")} queryKeySuffix="overview" /></CardContent>
            </Card>
          </div>
        </TabsContent>
        <TabsContent value="contacts" className="pt-4">
          <div className="flex items-center justify-between gap-3">
            <h2 className="text-sm font-semibold">{t("tabs.contacts")}</h2>
            {can(...ACTION_ADD_CONTACT) ? <Button size="sm" onClick={() => setAddContactOpen(true)}>{t("contactsTab.addContact")}</Button> : null}
          </div>
          {contacts.isPending ? <div className="mt-3 grid gap-2">{[1, 2, 3].map((n) => <Skeleton key={n} className="h-14 w-full rounded-lg" />)}</div> : contacts.isError ? <div className="mt-3"><ErrorState error={contacts.error} onRetry={() => void contacts.refetch()} /></div> : contactRows.length === 0 ? <div className="mt-3"><EmptyState title={t("contactsTab.emptyTitle")} description={t("contactsTab.emptyDescription")} /></div> : (
            <ul className="mt-3 grid gap-2 sm:grid-cols-2">
              {contactRows.map((contact) => <li key={contact.id} className="rounded-2xl border border-border bg-card px-4 py-3">
                <Link className="font-medium underline-offset-4 hover:underline" href={`/app/contacts/${encodeURIComponent(contact.id)}`}>{contact.name}</Link>
                <p className="mt-1 text-sm text-muted-foreground">{contact.job_title ?? "—"}{contact.department ? ` · ${contact.department}` : ""}</p>
                <div className="mt-2"><StatusBadge status={contact.status} /></div>
              </li>)}
            </ul>
          )}
        </TabsContent>
        <TabsContent value="activity" className="pt-4"><ActivityFeed path={activitiesPath} ns="companies" emptyTitle={t("activity.emptyTitle")} emptyDescription={t("activity.permittedOnly")} queryKeySuffix="all" /></TabsContent>
        <TabsContent value="emails" className="pt-4"><ActivityFeed path={activitiesPath} ns="companies" filter={activityFilter("mail")} emptyTitle={t("activity.emptyMailTitle")} emptyDescription={t("activity.emptyMailDescription")} queryKeySuffix="emails" /></TabsContent>
        <TabsContent value="meetings" className="pt-4"><ActivityFeed path={activitiesPath} ns="companies" filter={activityFilter("booking")} emptyTitle={t("activity.emptyMeetingTitle")} emptyDescription={t("activity.emptyMeetingDescription")} queryKeySuffix="meetings" /></TabsContent>
        <TabsContent value="research" className="pt-4"><ResearchEvidencePanel companyId={companyId} /></TabsContent>
        <TabsContent value="notes" className="pt-4"><NotesPanel ns="companies" companyId={companyId} canWrite={can(...ACTION_ADD_NOTE)} invalidateKeys={[["activities", activitiesPath, "notes"]]} feed={<ActivityFeed path={activitiesPath} ns="companies" filter={activityFilter("note")} emptyTitle={t("notes.emptyTitle")} emptyDescription={t("notes.emptyDescription")} queryKeySuffix="notes" />} /></TabsContent>
        <TabsContent value="tasks" className="pt-4"><TasksPanel ns="companies" companyId={companyId} canWrite={can(...ACTION_ADD_TASK)} /></TabsContent>
      </Tabs>
      <CompanyFormDialog open={editOpen} onOpenChange={setEditOpen} company={company} etag={company.version} onSaved={() => { void queryClient.invalidateQueries({ queryKey: ["companies"] }); void detail.refetch(); }} />
      <ContactFormDialog open={addContactOpen} onOpenChange={setAddContactOpen} defaultCompany={company} onSaved={() => { void queryClient.invalidateQueries({ queryKey: ["contacts"] }); void contacts.refetch(); }} />
      <MergeRequestDialog open={mergeOpen} onOpenChange={setMergeOpen} fixedTarget={company} pickerLabels={{ placeholder: t("list.searchPlaceholder"), none: t("list.unassigned"), empty: t("list.emptyFiltered") }} />
      <Dialog open={archiveOpen} onOpenChange={setArchiveOpen}>
        <DialogContent>
          <DialogHeader><DialogTitle>{t("detail.archiveTitle")}</DialogTitle><DialogDescription>{t("detail.archiveDescription")}</DialogDescription></DialogHeader>
          <div className="grid gap-1.5"><Label htmlFor="company-archive-reason">{t("detail.archiveReasonLabel")}</Label><Input id="company-archive-reason" value={archiveReason} onChange={(event) => setArchiveReason(event.target.value)} placeholder={t("detail.archiveReasonPlaceholder")} maxLength={1000} /></div>
          {archiveError ? <p role="alert" className="text-sm text-destructive">{archiveError}</p> : null}
          <DialogFooter><Button variant="outline" onClick={() => setArchiveOpen(false)}>{tc("cancel")}</Button><Button variant="destructive" disabled={archiving} onClick={() => void archive()}>{archiving ? tc("loading") : t("detail.archiveConfirm")}</Button></DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function Field({ label, value }: { label: string; value: string | null | undefined }) {
  return <div className="grid gap-1"><dt className="text-xs text-muted-foreground">{label}</dt><dd className="break-words">{value || "—"}</dd></div>;
}
