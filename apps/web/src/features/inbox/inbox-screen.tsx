"use client";

/**
 * 공동 Inbox 목록 화면 (UI05).
 * 메일함·미배정·내 담당·상태 필터 + 검색 + cursor 페이지네이션.
 * 메일함 재연결 필요 시 최상단 차단 배너를 띄운다.
 */
import { Search } from "lucide-react";
import { useTranslations } from "next-intl";
import Link from "next/link";
import { useEffect, useMemo, useState } from "react";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { PageHeader } from "@/components/patterns/page-header";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  THREAD_STATUSES,
  useEmailThreads,
  useMailAccounts,
  usePrincipals,
  type EmailThreadSummary,
  type ThreadStatus,
} from "@/lib/api/inbox";
import type { ApiError } from "@/lib/api/problem";
import {
  isThreadUnread,
  needsReconnect,
  reconnectAccounts,
} from "./thread-state";
import { ReconnectBanner } from "./reconnect-banner";
import { ThreadRow } from "./thread-row";

type AssigneeFilter = "all" | "unassigned" | "me";
type StatusFilter = "all" | ThreadStatus;

const ALL = "all";

/** 간단한 디바운스 — 검색어 입력이 멈춘 뒤에만 목록 쿼리를 바꾼다. */
function useDebouncedValue<T>(value: T, delayMs: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), delayMs);
    return () => clearTimeout(timer);
  }, [value, delayMs]);
  return debounced;
}

export function InboxScreen() {
  const t = useTranslations("inbox");
  const tDrafts = useTranslations("sending.history");
  const tc = useTranslations("common.table");
  const ts = useTranslations("status");

  const [mailbox, setMailbox] = useState<string>(ALL);
  const [assignee, setAssignee] = useState<AssigneeFilter>("all");
  const [status, setStatus] = useState<StatusFilter>("all");
  const [searchInput, setSearchInput] = useState("");
  const q = useDebouncedValue(searchInput.trim(), 300);

  const principalsQuery = usePrincipals();
  const principalNames = useMemo(() => {
    const names = new Map<string, string>();
    for (const principal of principalsQuery.data?.data ?? []) {
      const name = principal.name?.trim() || principal.display_name?.trim();
      if (name) names.set(principal.id, name);
    }
    return names;
  }, [principalsQuery.data]);
  const accountsQuery = useMailAccounts();
  const accounts = useMemo(
    () => accountsQuery.data?.data ?? [],
    [accountsQuery.data]
  );
  const reauth = reconnectAccounts(accounts);

  const threadsQuery = useEmailThreads({
    mail_account_id: mailbox === ALL ? undefined : mailbox,
    assigned: assignee === "all" ? undefined : assignee,
    status: status === "all" ? undefined : status,
    q: q === "" ? undefined : q,
  });

  const threads = useMemo(
    () => threadsQuery.data?.pages.flatMap((page) => page.data) ?? [],
    [threadsQuery.data]
  );
  const lastPage = threadsQuery.data?.pages.at(-1)?.page;

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-4 py-6 sm:px-6">
      <PageHeader
        title={t("title")}
        description={t("description")}
        actions={
          <Button asChild type="button" variant="outline" size="sm">
            <Link href="/app/inbox/drafts">{tDrafts("open")}</Link>
          </Button>
        }
      />

      {reauth.length > 0 ? (
        <ReconnectBanner accounts={reauth} blocking />
      ) : null}

      {/* 필터 바 — 메일함 / 담당 / 상태 / 검색 (UI05) */}
      <div className="flex flex-wrap items-end gap-3">
        <div className="grid gap-1.5">
          <Label id="inbox-mailbox-filter-label" className="text-xs">
            {t("filters.mailbox")}
          </Label>
          <Select value={mailbox} onValueChange={setMailbox}>
            <SelectTrigger
              aria-labelledby="inbox-mailbox-filter-label"
              className="w-52"
            >
              <SelectValue placeholder={t("filters.allMailboxes")} />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ALL}>{t("filters.allMailboxes")}</SelectItem>
              {accounts.map((account) => (
                <SelectItem
                  key={account.id}
                  value={account.id}
                  textValue={account.name || account.primary_address}
                >
                  <span className="flex items-center gap-2">
                    <span className="truncate">
                      {account.name || account.primary_address}
                    </span>
                    {needsReconnect(account) ? (
                      <Badge variant="amber-subtle">
                        {t("reconnect.syncStopped")}
                      </Badge>
                    ) : null}
                  </span>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <div className="grid gap-1.5">
          <Label id="inbox-assignee-filter-label" className="text-xs">
            {t("filters.assignee")}
          </Label>
          <Tabs
            value={assignee}
            onValueChange={(v) => setAssignee(v as AssigneeFilter)}
          >
            <TabsList aria-labelledby="inbox-assignee-filter-label">
              <TabsTrigger value="all">{t("filters.assigneeAll")}</TabsTrigger>
              <TabsTrigger value="unassigned">
                {t("filters.assigneeUnassigned")}
              </TabsTrigger>
              <TabsTrigger value="me">{t("filters.assigneeMine")}</TabsTrigger>
            </TabsList>
          </Tabs>
        </div>

        <div className="grid gap-1.5">
          <Label id="inbox-status-filter-label" className="text-xs">
            {t("filters.status")}
          </Label>
          <Select
            value={status}
            onValueChange={(v) => setStatus(v as StatusFilter)}
          >
            <SelectTrigger
              aria-labelledby="inbox-status-filter-label"
              className="w-36"
            >
              <SelectValue placeholder={t("filters.statusAll")} />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ALL}>{t("filters.statusAll")}</SelectItem>
              {THREAD_STATUSES.map((s) => (
                <SelectItem key={s} value={s}>
                  {ts(s)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <div className="grid min-w-48 flex-1 gap-1.5">
          <Label htmlFor="inbox-search" className="text-xs">
            {t("filters.searchLabel")}
          </Label>
          <div className="relative">
            <Search
              aria-hidden
              className="absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
            />
            <Input
              id="inbox-search"
              value={searchInput}
              onChange={(e) => setSearchInput(e.target.value)}
              placeholder={t("filters.searchPlaceholder")}
              className="pl-9"
            />
          </div>
        </div>
      </div>

      <ThreadList
        threads={threads}
        principalNames={principalNames}
        loading={threadsQuery.isPending}
        error={threadsQuery.error}
        onRetry={() => threadsQuery.refetch()}
        hasMore={lastPage?.has_more ?? false}
        loadingMore={threadsQuery.isFetchingNextPage}
        onLoadMore={() => threadsQuery.fetchNextPage()}
        loadMoreLabel={tc("loadMore")}
        loadingLabel={tc("loading")}
      />
    </div>
  );
}

interface ThreadListProps {
  threads: EmailThreadSummary[];
  principalNames: Map<string, string>;
  loading: boolean;
  error: Error | ApiError | null;
  onRetry: () => void;
  hasMore: boolean;
  loadingMore: boolean;
  onLoadMore: () => void;
  loadMoreLabel: string;
  loadingLabel: string;
}

function ThreadList({
  threads,
  principalNames,
  loading,
  error,
  onRetry,
  hasMore,
  loadingMore,
  onLoadMore,
  loadMoreLabel,
  loadingLabel,
}: ThreadListProps) {
  const t = useTranslations("inbox");

  if (error) {
    return <ErrorState error={error} onRetry={onRetry} />;
  }

  return (
    <div className="flex flex-col gap-3">
      <ul
        aria-label={t("list.caption")}
        className="flex flex-col gap-2"
      >
        {loading
          ? Array.from({ length: 5 }).map((_, i) => (
              <li key={`skeleton-${i}`}>
                <Skeleton className="h-20 w-full rounded-2xl" />
              </li>
            ))
          : threads.map((thread) => (
              <li key={thread.id}>
                <ThreadRow thread={thread} unread={isThreadUnread(thread)} principalNames={principalNames} />
              </li>
            ))}
      </ul>

      {!loading && threads.length === 0 ? (
        <EmptyState
          title={t("list.empty")}
          description={t("list.emptyDescription")}
        />
      ) : null}

      {hasMore ? (
        <div className="flex justify-center">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={loadingMore}
            onClick={onLoadMore}
          >
            {loadingMore ? loadingLabel : loadMoreLabel}
          </Button>
        </div>
      ) : null}
    </div>
  );
}
