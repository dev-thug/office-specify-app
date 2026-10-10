"use client";

import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslations } from "next-intl";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  getMarketingSuppressionSyncStatus,
  type MarketingSuppressionSyncState,
} from "@/lib/api/marketing";
import { useIsAdmin } from "@/features/settings/use-me";

function normalizedSyncStatus(status: string): MarketingSuppressionSyncState {
  switch (status) {
    case "pending":
    case "in_progress":
    case "completed":
    case "unknown":
    case "rejected":
    case "not_connected":
      return status;
    default:
      return "unknown";
  }
}

/** A read-only Office status check; it never submits or retries a provider mutation. */
export function SuppressionSyncStatus({ suppressionId }: { suppressionId: string }) {
  const t = useTranslations("compliance.suppressions.syncStatus");
  const isAdmin = useIsAdmin();
  const [requested, setRequested] = useState(false);
  const query = useQuery({
    queryKey: ["compliance", "suppression-sync-status", suppressionId],
    enabled: isAdmin && requested,
    queryFn: () => getMarketingSuppressionSyncStatus(suppressionId),
    retry: false,
    refetchOnWindowFocus: false,
  });

  if (!isAdmin) return null;

  const readStatus = () => {
    if (requested) {
      void query.refetch();
    } else {
      setRequested(true);
    }
  };

  return (
    <div className="mt-2 flex flex-col items-end gap-2">
      <Button
        type="button"
        variant="outline"
        size="xs"
        disabled={query.isFetching}
        onClick={readStatus}
      >
        {requested ? t("refresh") : t("check")}
      </Button>

      {requested && query.isFetching ? (
        <p role="status" className="text-2xs text-muted-foreground">{t("checking")}</p>
      ) : null}
      {requested && query.isError ? (
        <p role="alert" className="text-2xs text-destructive">{t("loadFailed")}</p>
      ) : null}
      {requested && query.isSuccess ? (
        query.data.data.length === 0 ? (
          <p role="status" className="text-2xs text-muted-foreground">{t("notConnected")}</p>
        ) : (
          <ul className="grid gap-2 text-left">
            {query.data.data.map((record) => {
              const status = normalizedSyncStatus(record.status);
              return (
                <li key={record.connection_id} className="grid gap-1 rounded-md border border-border px-2.5 py-2">
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <span className="text-2xs font-medium">
                      {t("connectionTitle", { connection: record.connection_name })}
                    </span>
                    <Badge variant="secondary">{t(`status.${status}`)}</Badge>
                  </div>
                  {status === "unknown" ? (
                    <p className="text-2xs text-muted-foreground">{t("unknownDescription")}</p>
                  ) : null}
                  {record.provider_code ? (
                    <p className="text-2xs text-muted-foreground">
                      {t("providerCode", { code: record.provider_code })}
                    </p>
                  ) : null}
                  {record.updated_at ? (
                    <p className="text-2xs text-muted-foreground">
                      {t("updatedAt", { time: record.updated_at })}
                    </p>
                  ) : null}
                </li>
              );
            })}
          </ul>
        )
      ) : null}
    </div>
  );
}
