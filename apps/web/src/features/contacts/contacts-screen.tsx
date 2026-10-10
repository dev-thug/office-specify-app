"use client";

import Link from "next/link";
import { useEffect, useMemo, useState } from "react";
import { useLocale, useTranslations } from "next-intl";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { CompanyPicker } from "@/features/companies/company-picker";
import { principalName, useMe, useHumanPrincipals } from "@/features/companies/owner-select";
import { ContactFormDialog } from "@/features/contacts/contact-form-dialog";
import { formatDateTime } from "@/features/companies/utils";
import { requestCrmExport } from "@/lib/api/companies";
import { idempotencyKey } from "@/lib/api/client";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { contactListParams, contactsPath, type Contact, type ContactStatus } from "@/lib/api/contacts";
import type { Company } from "@/lib/api/companies";

const ALL = "__all__";

export function ContactsScreen() {
  const t = useTranslations("contacts");
  const tc = useTranslations("common");
  const statusT = useTranslations("status");
  const locale = useLocale();
  const me = useMe();
  const principals = useHumanPrincipals();
  const queryClient = useQueryClient();
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [status, setStatus] = useState(ALL);
  const [owner, setOwner] = useState(ALL);
  const [company, setCompany] = useState<Company | null>(null);
  const [sort, setSort] = useState("created_desc");
  const [createOpen, setCreateOpen] = useState(false);
  const [exporting, setExporting] = useState(false);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(query.trim()), 250);
    return () => clearTimeout(timer);
  }, [query]);

  const params = useMemo(() => contactListParams({
    q: debounced,
    status: status === ALL ? undefined : status,
    owner_principal_id: owner === ALL ? undefined : owner,
    company_id: company?.id,
  }), [debounced, status, owner, company?.id]);
  const list = useCursorList<Contact>({
    queryKey: ["contacts", "list", params, sort],
    path: contactsPath(),
    params,
  });
  const rows = useMemo(() => {
    const items = list.data?.pages.flatMap((page) => page.data ?? []) ?? [];
    return [...items].sort((a, b) => sort === "name_asc"
      ? a.name.localeCompare(b.name)
      : sort === "updated_desc"
        ? (b.updated_at ?? "").localeCompare(a.updated_at ?? "")
        : (b.created_at ?? "").localeCompare(a.created_at ?? ""));
  }, [list.data, sort]);
  const lastPage = list.data?.pages.at(-1);
  const columns: ColumnDef<Contact>[] = [
    { key: "name", header: t("list.columns.name"), cell: (row) => <Link className="font-medium underline-offset-4 hover:underline" href={`/app/contacts/${encodeURIComponent(row.id)}`}>{row.name}</Link> },
    { key: "company", header: t("list.columns.company"), cell: (row) => row.company_name ?? (row.company_id ? <Link className="underline-offset-4 hover:underline" href={`/app/companies/${encodeURIComponent(row.company_id)}`}>{t("list.linkedCompany")}</Link> : t("list.noCompany")) },
    { key: "title", header: t("list.columns.jobTitle"), cell: (row) => row.job_title ?? "—" },
    { key: "status", header: t("list.columns.status"), cell: (row) => <StatusBadge status={row.status} /> },
    { key: "email", header: t("list.columns.primaryEmail"), cell: (row) => row.emails?.find((email) => email.is_primary)?.address ?? "—" },
    { key: "owner", header: t("list.columns.owner"), cell: (row) => row.owner_display_name?.trim() || principalName(principals.data?.find((p) => p.id === row.owner_principal_id), row.owner_principal_id) || (row.owner_principal_id ? tc("nameUnavailable") : t("detail.unassigned")) },
    { key: "created", header: t("list.columns.created"), cell: (row) => formatDateTime(row.created_at, locale) },
  ];

  async function exportCurrentFilter() {
    setExporting(true);
    try {
      await requestCrmExport({
        fields: ["company.name", "contact.name", "contact.job_title", "contact.department", "contact.status", "contact.email", "contact.email_label"],
        filters: {
          ...(query.trim() ? { q: query.trim() } : {}),
          ...(company?.id ? { company_ids: [company.id] } : {}),
        },
      }, { idempotencyKey: idempotencyKey() });
      toast.success(t("list.exportQueued"));
    } catch {
      toast.error(t("list.exportError"));
    } finally {
      setExporting(false);
    }
  }

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader title={t("title")} description={t("description")} actions={<>
        {me.data?.kind === "human" ? <Button variant="outline" size="sm" disabled={exporting || status !== ALL || owner !== ALL} title={status !== ALL || owner !== ALL ? t("list.exportFilterUnsupported") : undefined} onClick={() => void exportCurrentFilter()}>{exporting ? tc("loading") : t("list.exportCsv")}</Button> : null}
        {me.data?.kind === "human" ? <Button size="sm" onClick={() => setCreateOpen(true)}>{t("list.newContact")}</Button> : null}
      </>} />
      <div className="grid gap-3 rounded-2xl border border-border bg-card p-4 md:grid-cols-2 xl:grid-cols-5">
        <Input aria-label={t("list.searchPlaceholder")} placeholder={t("list.searchPlaceholder")} value={query} onChange={(event) => setQuery(event.target.value)} />
        <Select value={status} onValueChange={setStatus}>
          <SelectTrigger aria-label={t("list.statusFilter")}><SelectValue /></SelectTrigger>
          <SelectContent><SelectItem value={ALL}>{t("list.allStatuses")}</SelectItem>{(["new", "active", "departed", "inactive"] as ContactStatus[]).map((value) => <SelectItem key={value} value={value}>{statusT(value)}</SelectItem>)}</SelectContent>
        </Select>
        <Select value={owner} onValueChange={setOwner}>
          <SelectTrigger aria-label={t("list.ownerFilter")}><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL}>{t("list.allOwners")}</SelectItem>
            {(principals.data ?? []).map((person) => <SelectItem key={person.id} value={person.id}>{principalName(person, person.id) ?? tc("nameUnavailable")}</SelectItem>)}
          </SelectContent>
        </Select>
        <div className="grid gap-1">
          <span className="text-xs text-muted-foreground">{t("list.companyFilter")}</span>
          <CompanyPicker value={company} onChange={setCompany} placeholder={t("form.companyPlaceholder")} noneLabel={t("list.allCompanies")} emptyLabel={t("form.companySearchEmpty")} />
        </div>
        <Select value={sort} onValueChange={setSort}>
          <SelectTrigger aria-label={t("list.sortLabel")}><SelectValue /></SelectTrigger>
          <SelectContent><SelectItem value="created_desc">{t("list.sortCreatedDesc")}</SelectItem><SelectItem value="updated_desc">{t("list.sortUpdatedDesc")}</SelectItem><SelectItem value="name_asc">{t("list.sortNameAsc")}</SelectItem></SelectContent>
        </Select>
      </div>
      <DataTable
        caption={t("list.columns.name")}
        columns={columns}
        rows={rows}
        rowKey={(row) => row.id}
        loading={list.isPending}
        error={list.error}
        onRetry={() => void list.refetch()}
        page={lastPage?.page}
        onLoadMore={() => void list.fetchNextPage()}
        loadingMore={list.isFetchingNextPage}
      />
      <ContactFormDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        onSaved={() => { void queryClient.invalidateQueries({ queryKey: ["contacts"] }); }}
      />
    </div>
  );
}
