"use client";

import Link from "next/link";
import { useEffect, useMemo, useState } from "react";
import { useLocale, useTranslations } from "next-intl";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";

import { DataTable, NO_SELECTION, type ColumnDef, type RowSelection } from "@/components/patterns/data-table";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { CompanyFormDialog } from "@/features/companies/company-form-dialog";
import { MergeRequestDialog } from "@/features/companies/merge-request-dialog";
import { principalName, useMe, useHumanPrincipals } from "@/features/companies/owner-select";
import { companyListParams, companiesPath, requestCrmExport, type Company, type CompanyCreated, type CompanyDuplicateCandidate, type LifecycleStage } from "@/lib/api/companies";
import { idempotencyKey } from "@/lib/api/client";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { formatDateTime } from "@/features/companies/utils";

const ALL = "__all__";

export function CompaniesScreen() {
  const t = useTranslations("companies");
  const tc = useTranslations("common");
  const locale = useLocale();
  const queryClient = useQueryClient();
  const me = useMe();
  const principals = useHumanPrincipals();
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [stage, setStage] = useState(ALL);
  const [owner, setOwner] = useState(ALL);
  const [sort, setSort] = useState("created_desc");
  const [archivedOnly, setArchivedOnly] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [mergeOpen, setMergeOpen] = useState(false);
  const [selection, setSelection] = useState<RowSelection>(NO_SELECTION);
  const [duplicates, setDuplicates] = useState<CompanyDuplicateCandidate[]>([]);
  const [exporting, setExporting] = useState(false);
  const [selectedError, setSelectedError] = useState<string | null>(null);

  useEffect(() => {
    const timer = setTimeout(() => setDebounced(query.trim()), 250);
    return () => clearTimeout(timer);
  }, [query]);

  const params = useMemo(() => companyListParams({
    q: debounced,
    lifecycle_stage: stage === ALL ? undefined : stage,
    owner_principal_id: owner === ALL ? undefined : owner,
    archived: archivedOnly ? "true" : undefined,
  }), [debounced, stage, owner, archivedOnly]);
  const list = useCursorList<Company>({
    queryKey: ["companies", "list", params, sort],
    path: companiesPath(),
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
  const selectedRows = selection.kind === "rows"
    ? rows.filter((row) => selection.ids.has(row.id))
    : [];
  const human = me.data?.kind === "human";

  const columns: ColumnDef<Company>[] = [
    {
      key: "name",
      header: t("list.columns.name"),
      cell: (row) => <Link className="font-medium text-foreground underline-offset-4 hover:underline" href={`/app/companies/${encodeURIComponent(row.id)}`}>{row.name}</Link>,
    },
    { key: "domain", header: t("list.columns.domain"), cell: (row) => row.domain ?? "—" },
    { key: "country", header: t("list.columns.country"), cell: (row) => row.country_code ?? "—" },
    { key: "industry", header: t("list.columns.industry"), cell: (row) => row.industry ?? "—" },
    { key: "stage", header: t("list.columns.stage"), cell: (row) => <StatusBadge status={row.lifecycle_stage} /> },
    { key: "owner", header: t("list.columns.owner"), cell: (row) => row.owner_display_name?.trim() || principalName(principals.data?.find((p) => p.id === row.owner_principal_id), row.owner_principal_id) || (row.owner_principal_id ? tc("nameUnavailable") : t("list.unassigned")) },
    { key: "activity", header: t("list.columns.lastActivity"), cell: (row) => row.last_activity_summary ? <span title={formatDateTime(row.last_activity_at, locale)}>{row.last_activity_summary}</span> : "—" },
    { key: "created", header: t("list.columns.created"), cell: (row) => formatDateTime(row.created_at, locale) },
  ];

  async function exportCurrentFilter() {
    setExporting(true);
    try {
      await requestCrmExport({
        fields: ["company.name", "company.domain", "company.industry", "company.country_code", "company.website_url", "company.lifecycle_stage"],
        filters: {
          ...(debounced ? { q: debounced } : {}),
          ...(stage !== ALL ? { lifecycle_stage: stage } : {}),
        },
      }, { idempotencyKey: idempotencyKey() });
      toast.success(t("list.exportQueued"));
    } catch {
      toast.error(t("list.exportError"));
    } finally {
      setExporting(false);
    }
  }

  function openMerge() {
    setSelectedError(null);
    if (selection.kind !== "rows" || selectedRows.length !== 2) {
      setSelectedError(t("list.mergeNeedTwo"));
      return;
    }
    setMergeOpen(true);
  }

  function resetFilters() {
    setSelection(NO_SELECTION);
  }

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader
        title={t("title")}
        description={t("description")}
        actions={<>
          {human ? <Button variant="outline" size="sm" onClick={() => void exportCurrentFilter()} disabled={exporting || owner !== ALL || archivedOnly} title={owner !== ALL || archivedOnly ? t("list.exportFilterUnsupported") : undefined}>{exporting ? tc("loading") : t("list.exportCsv")}</Button> : null}
          {human ? <Button variant="outline" size="sm" onClick={openMerge}>{t("list.mergeRequest")}</Button> : null}
          {human ? <Button size="sm" onClick={() => setCreateOpen(true)}>{t("list.newCompany")}</Button> : null}
        </>}
      />
      {duplicates.length > 0 ? (
        <Alert variant="warning">
          <AlertDescription className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span>{t("duplicates.description")}</span>
            {duplicates.map((duplicate) => <Link key={duplicate.id} className="underline underline-offset-4" href={`/app/companies/${encodeURIComponent(duplicate.id)}`}>{duplicate.name}</Link>)}
            <Button variant="ghost" size="xs" onClick={() => setDuplicates([])}>{tc("close")}</Button>
          </AlertDescription>
        </Alert>
      ) : null}
      {selectedError ? <p role="alert" className="text-sm text-destructive">{selectedError}</p> : null}

      <div className="grid gap-3 rounded-2xl border border-border bg-card p-4 md:grid-cols-2 xl:grid-cols-5">
        <Input aria-label={t("list.searchPlaceholder")} placeholder={t("list.searchPlaceholder")} value={query} onChange={(e) => { setQuery(e.target.value); resetFilters(); }} />
        <Select value={stage} onValueChange={(value) => { setStage(value); resetFilters(); }}>
          <SelectTrigger aria-label={t("list.stageFilter")}><SelectValue placeholder={t("list.stageFilter")} /></SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL}>{t("list.allStages")}</SelectItem>
            {(["prospect", "engaged", "qualified", "customer", "inactive"] as LifecycleStage[]).map((value) => <SelectItem key={value} value={value}>{t(`stages.${value}`)}</SelectItem>)}
          </SelectContent>
        </Select>
        <div className="grid gap-1">
          <span className="text-xs text-muted-foreground">{t("list.ownerFilter")}</span>
          <Select value={owner} onValueChange={(value) => { setOwner(value); resetFilters(); }}>
            <SelectTrigger aria-label={t("list.ownerFilter")}><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value={ALL}>{t("list.allOwners")}</SelectItem>
              {(principals.data ?? []).map((person) => <SelectItem key={person.id} value={person.id}>{principalName(person, person.id) ?? tc("nameUnavailable")}</SelectItem>)}
            </SelectContent>
          </Select>
        </div>
        <Select value={sort} onValueChange={(value) => { setSort(value); resetFilters(); }}>
          <SelectTrigger aria-label={t("list.sortLabel")}><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="created_desc">{t("list.sortCreatedDesc")}</SelectItem>
            <SelectItem value="updated_desc">{t("list.sortUpdatedDesc")}</SelectItem>
            <SelectItem value="name_asc">{t("list.sortNameAsc")}</SelectItem>
          </SelectContent>
        </Select>
        <label className="flex items-center gap-2 text-sm">
          <Checkbox checked={archivedOnly} onCheckedChange={(checked) => { setArchivedOnly(checked === true); resetFilters(); }} />
          {t("list.archivedOnly")}
        </label>
      </div>

      <DataTable
        caption={t("list.columns.name")}
        columns={columns}
        rows={rows}
        rowKey={(row) => row.id}
        loading={list.isPending}
        error={list.error}
        onRetry={() => void list.refetch()}
        selectable
        selection={selection}
        onSelectionChange={setSelection}
        page={lastPage?.page}
        onLoadMore={() => void list.fetchNextPage()}
        loadingMore={list.isFetchingNextPage}
      />

      <CompanyFormDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        onSaved={(result, mode) => {
          if (mode === "created") setDuplicates((result as CompanyCreated).duplicate_candidates ?? []);
          void queryClient.invalidateQueries({ queryKey: ["companies"] });
        }}
      />
      <MergeRequestDialog
        open={mergeOpen}
        onOpenChange={setMergeOpen}
        pair={selectedRows.length === 2 ? [selectedRows[0]!, selectedRows[1]!] : undefined}
        pickerLabels={{ placeholder: t("list.searchPlaceholder"), none: t("list.unassigned"), empty: t("list.emptyFiltered") }}
        onRequested={() => {
          setSelection(NO_SELECTION);
          void queryClient.invalidateQueries({ queryKey: ["companies"] });
        }}
      />
    </div>
  );
}
