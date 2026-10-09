"use client";

import { useTranslations } from "next-intl";
import { useMemo, useState, type ReactNode } from "react";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import type { ApiError } from "@/lib/api/problem";

export interface ColumnDef<T> {
  key: string;
  header: ReactNode;
  cell: (row: T, index: number) => ReactNode;
  className?: string;
}

/** "선택한 행"과 "현재 필터 전체"를 명확히 구분하는 선택 모델 (§7.2). */
export type RowSelection =
  | { kind: "none" }
  | { kind: "rows"; ids: ReadonlySet<string> }
  | { kind: "allFiltered"; excludedIds?: ReadonlySet<string> };

export const NO_SELECTION: RowSelection = { kind: "none" };

export interface PageInfo {
  next_cursor: string | null;
  has_more: boolean;
}

export interface DataTableProps<T> {
  columns: ColumnDef<T>[];
  rows: T[];
  rowKey: (row: T, index: number) => string;
  /** 커서 페이지네이션 — has_more 면 "더 보기" 버튼을 보인다. */
  page?: PageInfo;
  onLoadMore?: (nextCursor: string) => void;
  loadingMore?: boolean;
  loading?: boolean;
  error?: ApiError | Error | null;
  onRetry?: () => void;
  selectable?: boolean;
  selection?: RowSelection;
  onSelectionChange?: (selection: RowSelection) => void;
  /** "현재 필터 전체" 개수 — 선택 툴바에 명시적으로 표시한다. */
  filteredTotal?: number;
  emptyState?: ReactNode;
  caption?: string;
  className?: string;
}

function selectionCount(selection: RowSelection, filteredTotal?: number): number {
  switch (selection.kind) {
    case "rows":
      return selection.ids.size;
    case "allFiltered":
      return (filteredTotal ?? 0) - (selection.excludedIds?.size ?? 0);
    default:
      return 0;
  }
}

export function DataTable<T>({
  columns,
  rows,
  rowKey,
  page,
  onLoadMore,
  loadingMore,
  loading,
  error,
  onRetry,
  selectable = false,
  selection = NO_SELECTION,
  onSelectionChange,
  filteredTotal,
  emptyState,
  caption,
  className,
}: DataTableProps<T>) {
  const t = useTranslations("common.table");
  const [internalSelection, setInternalSelection] = useState<RowSelection>(NO_SELECTION);
  const sel = onSelectionChange ? selection : internalSelection;
  const setSel = onSelectionChange ?? setInternalSelection;

  const allVisibleSelected = useMemo(
    () =>
      rows.length > 0 &&
      (sel.kind === "allFiltered" ||
        (sel.kind === "rows" && rows.every((r, i) => sel.ids.has(rowKey(r, i))))),
    [rows, sel, rowKey]
  );

  const selectedCount = selectionCount(sel, filteredTotal);

  const toggleRow = (key: string, checked: boolean) => {
    if (sel.kind === "allFiltered") {
      const excluded = new Set(sel.excludedIds ?? []);
      if (checked) excluded.delete(key);
      else excluded.add(key);
      setSel({ kind: "allFiltered", excludedIds: excluded });
      return;
    }
    const ids = new Set(sel.kind === "rows" ? sel.ids : []);
    if (checked) ids.add(key);
    else ids.delete(key);
    setSel(ids.size ? { kind: "rows", ids } : NO_SELECTION);
  };

  const isRowSelected = (key: string): boolean => {
    if (sel.kind === "allFiltered") return !(sel.excludedIds?.has(key) ?? false);
    return sel.kind === "rows" && sel.ids.has(key);
  };

  const toggleVisible = (checked: boolean) => {
    if (!checked) {
      setSel(NO_SELECTION);
      return;
    }
    setSel({ kind: "rows", ids: new Set(rows.map((r, i) => rowKey(r, i))) });
  };

  return (
    <div data-slot="data-table" className={cn("flex flex-col gap-3", className)}>
      {selectable && selectedCount > 0 ? (
        <div
          role="status"
          className="flex items-center gap-2 rounded-lg border border-border bg-muted px-3 py-2 text-sm"
        >
          <span className="font-medium">
            {sel.kind === "allFiltered"
              ? t("allFilteredSelected", {
                  count: selectedCount,
                })
              : t("rowsSelected", { count: selectedCount })}
          </span>
          {sel.kind === "rows" &&
          filteredTotal !== undefined &&
          filteredTotal > selectedCount ? (
            <Button
              type="button"
              variant="link"
              size="xs"
              onClick={() => setSel({ kind: "allFiltered" })}
            >
              {t("selectAllFiltered", { count: filteredTotal })}
            </Button>
          ) : null}
          <Button
            type="button"
            variant="ghost"
            size="xs"
            className="ml-auto"
            onClick={() => setSel(NO_SELECTION)}
          >
            {t("clearSelection")}
          </Button>
        </div>
      ) : null}

      <div className="overflow-x-auto rounded-2xl border border-border">
        <table className="w-full text-sm">
          {caption ? <caption className="sr-only">{caption}</caption> : null}
          <thead>
            <tr className="border-b border-border bg-app-panel-muted text-left">
              {selectable ? (
                <th scope="col" className="w-10 px-3 py-2">
                  <Checkbox
                    aria-label={t("selectVisibleRows")}
                    checked={allVisibleSelected}
                    onCheckedChange={(v) => toggleVisible(v === true)}
                  />
                </th>
              ) : null}
              {columns.map((col) => (
                <th
                  key={col.key}
                  scope="col"
                  className={cn(
                    "px-3 py-2 text-xs font-medium text-muted-foreground",
                    col.className
                  )}
                >
                  {col.header}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {loading
              ? Array.from({ length: 5 }).map((_, i) => (
                  <tr key={`skeleton-${i}`} className="border-b border-border/60 last:border-b-0">
                    {selectable ? (
                      <td className="px-3 py-2.5">
                        <Skeleton className="size-4 rounded-xs" />
                      </td>
                    ) : null}
                    {columns.map((col) => (
                      <td key={col.key} className="px-3 py-2.5">
                        <Skeleton className="h-4 w-3/4" />
                      </td>
                    ))}
                  </tr>
                ))
              : rows.map((row, i) => {
                  const key = rowKey(row, i);
                  const selected = isRowSelected(key);
                  return (
                    <tr
                      key={key}
                      data-state={selected ? "selected" : undefined}
                      className="border-b border-border/60 transition-colors last:border-b-0 hover:bg-muted/60 data-[state=selected]:bg-accent/60"
                    >
                      {selectable ? (
                        <td className="px-3 py-2.5">
                          <Checkbox
                            aria-label={t("selectRow")}
                            checked={selected}
                            onCheckedChange={(v) => toggleRow(key, v === true)}
                          />
                        </td>
                      ) : null}
                      {columns.map((col) => (
                        <td key={col.key} className={cn("px-3 py-2.5", col.className)}>
                          {col.cell(row, i)}
                        </td>
                      ))}
                    </tr>
                  );
                })}
          </tbody>
        </table>

        {!loading && !error && rows.length === 0 ? (
          <div className="p-3">{emptyState ?? <EmptyState title={t("empty")} />}</div>
        ) : null}
        {error ? (
          <div className="p-3">
            <ErrorState error={error} onRetry={onRetry} />
          </div>
        ) : null}
      </div>

      {page?.has_more ? (
        <div className="flex justify-center">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={loadingMore || !page.next_cursor}
            onClick={() => page.next_cursor && onLoadMore?.(page.next_cursor)}
          >
            {loadingMore ? t("loading") : t("loadMore")}
          </Button>
        </div>
      ) : null}
    </div>
  );
}
