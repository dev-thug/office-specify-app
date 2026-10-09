"use client";

/**
 * 업무 패널 — GET/POST /tasks, PATCH /tasks/{id} (If-Match, B.2/B.1.4).
 * 회사 상세(company_id)와 담당자 상세(contact_id)가 같은 패널을 쓴다.
 */
import { useTranslations, useLocale } from "next-intl";
import { useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { CalendarClock, CheckCircle2 } from "lucide-react";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { OwnerSelect } from "@/features/companies/owner-select";
import { formatDate, isOverdue } from "@/features/companies/utils";
import {
  createTask,
  patchTask,
  tasksPath,
  type TaskItem,
} from "@/lib/api/companies";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import { useCursorList } from "@/lib/api/use-cursor-list";

interface TasksPanelProps {
  ns: "companies" | "contacts";
  companyId?: string;
  contactId?: string;
  canWrite?: boolean;
}

export function TasksPanel({
  ns,
  companyId,
  contactId,
  canWrite = true,
}: TasksPanelProps) {
  const t = useTranslations(`${ns}.tasks`);
  const tc = useTranslations("common");
  const locale = useLocale();
  const queryClient = useQueryClient();
  const path = tasksPath({ companyId, contactId });
  const list = useCursorList<TaskItem>({
    queryKey: ["tasks", { companyId, contactId }],
    path,
  });
  const [title, setTitle] = useState("");
  const [dueAt, setDueAt] = useState("");
  const [ownerId, setOwnerId] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const idemRef = useRef<string>(idempotencyKey());

  const rows = (list.data?.pages ?? []).flatMap((p) => p.data ?? []);
  const lastPage = list.data?.pages?.[list.data.pages.length - 1];

  async function submit() {
    setPending(true);
    setError(null);
    try {
      await createTask(
        {
          title: title.trim(),
          company_id: companyId,
          contact_id: contactId,
          owner_principal_id: ownerId ?? undefined,
          due_at: dueAt ? new Date(`${dueAt}T00:00:00Z`).toISOString() : undefined,
        },
        { idempotencyKey: idemRef.current }
      );
      idemRef.current = idempotencyKey();
      setTitle("");
      setDueAt("");
      setOwnerId(null);
      await queryClient.invalidateQueries({ queryKey: ["tasks"] });
    } catch (err) {
      setError(
        err instanceof ApiError ? (err.problem.detail ?? t("error")) : t("error")
      );
      if (
        err instanceof ApiError &&
        err.problem.retryable !== true &&
        err.status < 500
      ) {
        idemRef.current = idempotencyKey();
      }
    } finally {
      setPending(false);
    }
  }

  async function setStatus(task: TaskItem, status: "done" | "open") {
    try {
      await patchTask(
        task.id,
        { status },
        { etag: task.version, idempotencyKey: idempotencyKey() }
      );
      await queryClient.invalidateQueries({ queryKey: ["tasks"] });
    } catch (err) {
      // 412면 목록을 다시 읽어 최신 버전을 맞춘다 — 숨은 덮어쓰기를 하지 않는다.
      if (err instanceof ApiError && (err.status === 412 || err.status === 428)) {
        await queryClient.invalidateQueries({ queryKey: ["tasks"] });
      }
      setError(t("updateError"));
    }
  }

  return (
    <div className="flex flex-col gap-4">
      {canWrite ? (
        <form
          className="flex flex-col gap-2 rounded-2xl border border-border p-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (title.trim()) void submit();
          }}
        >
          <Label htmlFor={`task-title-${companyId ?? contactId ?? "x"}`}>
            {t("addTitle")}
          </Label>
          <Input
            id={`task-title-${companyId ?? contactId ?? "x"}`}
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder={t("titlePlaceholder")}
            maxLength={300}
          />
          <div className="flex flex-wrap items-end gap-3">
            <div className="grid gap-1.5">
              <Label htmlFor={`task-due-${companyId ?? contactId ?? "x"}`}>
                {t("dueLabel")}
              </Label>
              <Input
                id={`task-due-${companyId ?? contactId ?? "x"}`}
                type="date"
                value={dueAt}
                onChange={(e) => setDueAt(e.target.value)}
              />
            </div>
            <div className="grid min-w-44 gap-1.5">
              <Label>{t("ownerLabel")}</Label>
              <OwnerSelect
                value={ownerId}
                onChange={setOwnerId}
                unassignedLabel={t("ownerLabel")}
              />
            </div>
            <Button type="submit" size="sm" disabled={pending || !title.trim()}>
              {pending ? t("submitting") : t("submit")}
            </Button>
          </div>
          {error ? (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          ) : null}
        </form>
      ) : null}

      {list.isPending ? (
        <div className="flex flex-col gap-2" aria-busy="true">
          {Array.from({ length: 3 }).map((_, i) => (
            <Skeleton key={i} className="h-12 w-full rounded-lg" />
          ))}
        </div>
      ) : list.isError ? (
        <ErrorState
          error={list.error instanceof ApiError ? list.error : null}
          onRetry={() => list.refetch()}
        />
      ) : rows.length === 0 ? (
        <EmptyState title={t("emptyTitle")} description={t("emptyDescription")} />
      ) : (
        <ul className="flex flex-col gap-2">
          {rows.map((task) => (
            <li
              key={task.id}
              className="flex items-center gap-3 rounded-lg border border-border px-3 py-2.5"
            >
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-2">
                  <span
                    className={
                      task.status === "done"
                        ? "text-ui text-muted-foreground line-through"
                        : "text-ui font-medium"
                    }
                  >
                    {task.title}
                  </span>
                  <StatusBadge status={task.status} />
                  {isOverdue(task) ? (
                    <StatusBadge status="risky" label={t("overdue")} />
                  ) : null}
                </div>
                <div className="mt-0.5 flex flex-wrap items-center gap-3 text-2xs text-muted-foreground">
                  {task.due_at ? (
                    <span className="inline-flex items-center gap-1">
                      <CalendarClock className="size-3" aria-hidden />
                      {formatDate(task.due_at, locale)}
                    </span>
                  ) : null}
                  {task.owner_display_name ? (
                    <span>{task.owner_display_name}</span>
                  ) : null}
                </div>
              </div>
              {task.status === "open" ? (
                <Button
                  type="button"
                  variant="outline"
                  size="xs"
                  onClick={() => void setStatus(task, "done")}
                >
                  <CheckCircle2 className="size-3.5" aria-hidden />
                  {t("markDone")}
                </Button>
              ) : (
                <Button
                  type="button"
                  variant="ghost"
                  size="xs"
                  onClick={() => void setStatus(task, "open")}
                >
                  {t("reopen")}
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
      {lastPage?.page?.has_more ? (
        <div className="flex justify-center">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={list.isFetchingNextPage}
            onClick={() => list.fetchNextPage()}
          >
            {list.isFetchingNextPage
              ? tc("table.loading")
              : tc("table.loadMore")}
          </Button>
        </div>
      ) : null}
    </div>
  );
}
