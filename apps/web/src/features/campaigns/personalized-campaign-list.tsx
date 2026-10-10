"use client";

import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { ArrowUpRight, Plus } from "lucide-react";
import { useTranslations } from "next-intl";
import Link from "next/link";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  getSalesCampaignSender,
  listSalesCampaigns,
  type SalesCampaignSummary,
} from "@/lib/api/sales-campaigns";

export function PersonalizedCampaignList() {
  const t = useTranslations("campaigns.personalized.list");

  const senderQuery = useQuery({
    queryKey: ["sales-campaigns", "sender"],
    queryFn: async () => (await getSalesCampaignSender()).data,
    retry: false,
  });
  const campaignsQuery = useInfiniteQuery({
    queryKey: ["sales-campaigns", "list"],
    initialPageParam: null as string | null,
    queryFn: ({ pageParam }) =>
      listSalesCampaigns({ limit: 50, cursor: pageParam ?? undefined }),
    getNextPageParam: (lastPage) =>
      lastPage.page?.has_more ? (lastPage.page.next_cursor ?? undefined) : undefined,
    refetchInterval: 2_000,
    retry: false,
  });

  const sender = senderQuery.data?.sender;
  const rows: SalesCampaignSummary[] =
    campaignsQuery.data?.pages.flatMap((page) => page.data) ?? [];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader
        title={t("title")}
        description={t("description")}
        actions={
          <div className="flex flex-wrap gap-2">
            <Button asChild variant="outline" size="sm">
              <Link href="/app/approvals">{t("approvals")}</Link>
            </Button>
            <Button asChild variant="outline" size="sm">
              <Link href="/app/campaigns/bulk">{t("bulkCampaigns")}</Link>
            </Button>
            <Button asChild size="sm">
              <Link href="/app/campaigns/new">
                <Plus aria-hidden />
                {t("newCampaign")}
              </Link>
            </Button>
          </div>
        }
      />

      {senderQuery.isError || (sender && !sender.ready) ? (
        <Alert variant="warning">
          <AlertDescription className="flex flex-wrap items-center justify-between gap-2">
            <span>{t("senderUnavailable")}</span>
            <Button asChild variant="link" size="xs">
              <Link href="/app/settings/mail-accounts">{t("openSettings")}</Link>
            </Button>
          </AlertDescription>
        </Alert>
      ) : sender ? (
        <div className="flex flex-wrap items-center justify-between gap-2 rounded-2xl border border-border bg-app-panel-muted px-4 py-3">
          <div className="min-w-0">
            <p className="text-xs text-muted-foreground">{t("defaultSender")}</p>
            <p className="truncate text-sm font-medium">
              {sender.profile_name} · {sender.from_name} &lt;{sender.from_address}&gt;
            </p>
          </div>
          <StatusBadge status={sender.status} />
        </div>
      ) : senderQuery.isPending ? (
        <Skeleton className="h-16 w-full rounded-2xl" />
      ) : null}

      {campaignsQuery.isPending ? (
        <div className="grid gap-3 sm:grid-cols-2">
          <Skeleton className="h-28 rounded-2xl" />
          <Skeleton className="h-28 rounded-2xl" />
        </div>
      ) : campaignsQuery.isError ? (
        <ErrorState error={campaignsQuery.error} onRetry={() => campaignsQuery.refetch()} />
      ) : rows.length === 0 ? (
        <EmptyState
          title={t("emptyTitle")}
          description={t("emptyDescription")}
          action={
            <Button asChild size="sm">
              <Link href="/app/campaigns/new">
                <Plus aria-hidden />
                {t("newCampaign")}
              </Link>
            </Button>
          }
        />
      ) : (
        <section aria-label={t("campaignList")} className="flex flex-col gap-3">
          <div className="grid gap-3 sm:grid-cols-2">
            {rows.map((campaign) => (
              <Link
                key={campaign.id}
                href={`/app/campaigns/${encodeURIComponent(campaign.id)}`}
                className="group flex min-w-0 items-start justify-between gap-4 rounded-2xl border border-border bg-card px-4 py-4 outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
              >
                <span className="min-w-0">
                  <span className="block truncate text-sm font-semibold text-foreground">
                    {campaign.name}
                  </span>
                  {campaign.objective ? (
                    <span className="mt-1 block line-clamp-2 text-sm text-muted-foreground">
                      {campaign.objective}
                    </span>
                  ) : null}
                  <span className="mt-3 block truncate text-2xs text-muted-foreground">
                    {campaign.from_name} &lt;{campaign.from_address}&gt;
                  </span>
                </span>
                <span className="flex shrink-0 items-center gap-2">
                  <StatusBadge status={campaign.sender_status} />
                  <ArrowUpRight
                    aria-hidden
                    className="size-4 text-muted-foreground transition-transform group-hover:translate-x-0.5 group-hover:-translate-y-0.5"
                  />
                </span>
              </Link>
            ))}
          </div>
          {campaignsQuery.hasNextPage ? (
            <div className="flex justify-center">
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={campaignsQuery.isFetchingNextPage}
                onClick={() => void campaignsQuery.fetchNextPage()}
              >
                {campaignsQuery.isFetchingNextPage ? t("loadingMore") : t("loadMore")}
              </Button>
            </div>
          ) : null}
        </section>
      )}
    </div>
  );
}
