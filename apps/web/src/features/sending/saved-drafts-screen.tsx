"use client";

import { useQuery } from "@tanstack/react-query";
import { useLocale, useTranslations } from "next-intl";
import Link from "next/link";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { ErrorState } from "@/components/patterns/error-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Button } from "@/components/ui/button";
import { getMessageDraft, MESSAGE_DRAFTS_PATH, type MessageDraftListItem } from "@/lib/api/sending";
import { useMailAccounts } from "@/lib/api/inbox";
import { useCursorList } from "@/lib/api/use-cursor-list";

import { DraftSendingWorkflow } from "./draft-sending-workflow";

function formatUpdatedAt(value: string, locale: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

export function SavedDraftsScreen() {
  const t = useTranslations("sending.history");
  const tw = useTranslations("sending.workflow");
  const locale = useLocale();
  const drafts = useCursorList<MessageDraftListItem>({
    queryKey: ["sending", "draft-history"],
    path: MESSAGE_DRAFTS_PATH,
    limit: 50,
  });
  const accounts = useMailAccounts();
  const mailboxNames = new Map(
    (accounts.data?.data ?? []).map((account) => [
      account.id,
      account.name || account.primary_address,
    ])
  );
  const rows = drafts.data?.pages.flatMap((page) => page.data) ?? [];
  const lastPage = drafts.data?.pages.at(-1)?.page;

  const purposeLabel = (purpose: string) =>
    tw.has(`purposes.${purpose}`) ? tw(`purposes.${purpose}`) : purpose;

  const columns: ColumnDef<MessageDraftListItem>[] = [
    {
      key: "subject",
      header: t("subject"),
      cell: (draft) => (
        <Link
          href={`/app/inbox/drafts/${encodeURIComponent(draft.draft_id)}`}
          className="font-medium text-foreground underline-offset-4 hover:underline"
        >
          {draft.subject || t("untitled")}
        </Link>
      ),
    },
    {
      key: "mailbox",
      header: t("mailbox"),
      cell: (draft) => (
        <span>{mailboxNames.get(draft.mail_account_id) ?? t("mailboxUnavailable")}</span>
      ),
    },
    {
      key: "purpose",
      header: t("purpose"),
      cell: (draft) => <span>{purposeLabel(draft.purpose)}</span>,
    },
    {
      key: "status",
      header: t("status"),
      cell: (draft) => <StatusBadge status={draft.status} />,
    },
    {
      key: "updated",
      header: t("updated"),
      className: "w-48",
      cell: (draft) => (
        <span className="text-muted-foreground">
          {formatUpdatedAt(draft.updated_at, locale)}
        </span>
      ),
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-4 py-6 sm:px-6">
      <PageHeader
        title={t("title")}
        description={t("description")}
        actions={
          <Button asChild type="button" variant="outline" size="sm">
            <Link href="/app/inbox">{t("backToInbox")}</Link>
          </Button>
        }
      />
      <DataTable
        columns={columns}
        rows={rows}
        rowKey={(draft) => draft.draft_id}
        page={lastPage}
        onLoadMore={() => void drafts.fetchNextPage()}
        loadingMore={drafts.isFetchingNextPage}
        loading={drafts.isLoading}
        error={drafts.error ?? accounts.error}
        onRetry={() => {
          void drafts.refetch();
          void accounts.refetch();
        }}
        emptyState={
          <div className="py-8 text-center">
            <p className="text-sm font-medium">{t("emptyTitle")}</p>
            <p className="mt-1 text-sm text-muted-foreground">
              {t("emptyDescription")}
            </p>
          </div>
        }
      />
    </div>
  );
}

export function SavedDraftDetailScreen({ draftId }: { draftId: string }) {
  const t = useTranslations("sending.history");
  const query = useQuery({
    queryKey: ["sending", "draft-history-detail", draftId],
    queryFn: () => getMessageDraft(draftId),
    retry: false,
  });
  const draft = query.data?.data;

  if (query.isPending) {
    return (
      <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-4 py-6 sm:px-6">
        <PageHeader title={t("detailTitle")} description={t("detailDescription")} />
        <div className="h-36 animate-pulse rounded-2xl border border-border bg-muted/30" />
      </div>
    );
  }

  if (query.error || !draft) {
    return (
      <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-4 py-6 sm:px-6">
        <PageHeader title={t("detailTitle")} description={t("detailDescription")} />
        <ErrorState error={query.error} onRetry={() => void query.refetch()} />
      </div>
    );
  }

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-4 py-6 sm:px-6">
      <PageHeader
        title={t("detailTitle")}
        description={t("detailDescription")}
        actions={
          <Button asChild type="button" variant="outline" size="sm">
            <Link href="/app/inbox/drafts">{t("backToDrafts")}</Link>
          </Button>
        }
      />
      <DraftSendingWorkflow
        key={draft.draft_id}
        draftId={draft.draft_id}
        mailAccountId={draft.mail_account_id}
        onChanged={() => query.refetch()}
      />
    </div>
  );
}
