"use client";

import { useLocale, useTranslations } from "next-intl";
import Link from "next/link";
import { useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeftIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  TriangleAlertIcon,
} from "lucide-react";
import { toast } from "sonner";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import {
  cancelLaunch,
  executeCampaignLaunch,
  getCampaign,
  getCampaignLaunch,
  normalizeLaunch,
  pauseLaunch,
  requestLaunchApproval,
  type CampaignLaunch,
  type CampaignLaunchDetail,
  type LaunchMessageSnapshot,
} from "@/lib/api/campaigns";
import { useDeliveryChannels, useMailAccounts } from "@/lib/api/settings";
import { MEETING_TYPES_LIST_LIMIT, PRINCIPALS_PATH, principalDisplayName, type PrincipalRef } from "@/lib/api/meetings";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { readEtag } from "@/lib/api/mutate";
import { ApiError } from "@/lib/api/problem";

import { useApiErrorMessage } from "./api-error-message";
import { formatDateTime, shortHash } from "./format";
import {
  campaignActions,
  channelBlocked,
  latestRevision,
  launchActions,
} from "./rules";
import { useIdempotencyKey } from "./use-idempotency-key";

export function CampaignDetail({ campaignId }: { campaignId: string }) {
  const t = useTranslations("campaigns.detail");
  const tCommon = useTranslations("common");
  const tList = useTranslations("campaigns.list");
  const tGate = useTranslations("campaigns.channelGate");
  const tReason = useTranslations("campaigns.reasonCodes");
  const locale = useLocale();
  const errorMessage = useApiErrorMessage("campaigns");

  const etagRef = useRef<string | undefined>(undefined);
  const detail = useQuery({
    queryKey: ["campaigns", "detail", campaignId],
    queryFn: async () => {
      const res = await getCampaign(campaignId);
      etagRef.current = readEtag(res) ?? undefined;
      return res.data;
    },
    retry: false,
  });
  const deliveryChannels = useDeliveryChannels();
  const mailAccountsQuery = useMailAccounts();
  const principalsQuery = useCursorList<PrincipalRef>({
    queryKey: ["meetings", "principals"],
    path: PRINCIPALS_PATH,
    limit: MEETING_TYPES_LIST_LIMIT,
  });

  const campaign = detail.data;
  const mailAccounts = mailAccountsQuery.data?.pages.flatMap((page) => page.data) ?? [];
  const principals = principalsQuery.data?.pages.flatMap((page) => page.data) ?? [];
  const mailbox = mailAccounts.find((account) => account.id === campaign?.mail_account_id);
  const mailboxLabel = mailbox
    ? [mailbox.name?.trim(), mailbox.primary_address?.trim()].filter(Boolean).join(" · ")
    : "";
  const ownerLabel = principalDisplayName(
    principals.find((principal) => principal.id === campaign?.owner_principal_id)
  );
  const revision = useMemo(
    () =>
      campaign?.revision ??
      latestRevision(campaign?.revisions) ??
      null,
    [campaign]
  );
  const launches = useMemo(
    () =>
      campaign?.launches ??
      (campaign?.launch ? [campaign.launch] : []),
    [campaign]
  );
  const channels = deliveryChannels.data?.pages.flatMap((page) => page.data) ?? [];
  const smtpChannelIds = new Set(
    channels
      .filter(
        (channel) =>
          channel.purpose === "marketing" &&
          channel.capabilities?.transport === "smtp"
      )
      .map((channel) => channel.id)
  );
  const gateBlocked = channelBlocked(campaign);
  const actions = campaignActions(campaign, {
    allowedActions: campaign?.allowed_actions,
  });

  const [bannerError, setBannerError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  const [expandedLaunch, setExpandedLaunch] = useState<string | null>(null);

  function refetch() {
    setConflict(false);
    setBannerError(null);
    void detail.refetch();
  }

  function handleError(err: unknown) {
    if (
      err instanceof ApiError &&
      (err.code === "VERSION_MISMATCH" || err.status === 412)
    ) {
      setConflict(true);
      return;
    }
    setBannerError(errorMessage(err));
  }

  if (detail.isLoading) {
    return (
      <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
        <Skeleton className="h-8 w-64" />
        <Skeleton className="h-40 w-full rounded-2xl" />
        <Skeleton className="h-64 w-full rounded-2xl" />
      </div>
    );
  }

  if (detail.isError) {
    return (
      <div className="mx-auto w-full max-w-6xl px-6 py-6">
        <ErrorState error={detail.error} onRetry={refetch} />
      </div>
    );
  }

  if (!campaign) {
    return (
      <div className="mx-auto w-full max-w-6xl px-6 py-6">
        <EmptyState
          title={t("notFoundTitle")}
          description={t("notFoundDescription")}
          action={
            <Button asChild variant="outline" size="sm">
              <Link href="/app/campaigns/bulk">{t("back")}</Link>
            </Button>
          }
        />
      </div>
    );
  }

  const rules = revision?.schedule_rules ?? {};

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <div>
        <Link
          href="/app/campaigns/bulk"
          className="inline-flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeftIcon className="size-4" aria-hidden />
          {t("back")}
        </Link>
      </div>

      <PageHeader
        title={campaign.name}
        actions={
          <div className="flex items-center gap-2">
            <StatusBadge status={campaign.status} />
            {actions.editDraft.visible || actions.newRevision.visible ? (
              <Button asChild size="sm" variant="outline">
                <Link
                  href={`/app/campaigns/bulk/new?source=${campaign.id}&mode=revision`}
                >
                  {actions.editDraft.visible ? t("editDraft") : t("newRevision")}
                </Link>
              </Button>
            ) : null}
            {actions.clone.visible ? (
              <Button asChild size="sm" variant="outline">
                <Link href={`/app/campaigns/bulk/new?source=${campaign.id}&mode=clone`}>
                  {t("clone")}
                </Link>
              </Button>
            ) : null}
          </div>
        }
      />

      {/* 공급자 게이트 — 최상단 차단 배너 (§7.3 UI07). approve는 이를 우회하지 못한다. */}
      {gateBlocked ? (
        <Alert variant="destructive">
          <TriangleAlertIcon />
          <AlertTitle>{tGate("title")}</AlertTitle>
          <AlertDescription>
            <p>{tGate("description")}</p>
            {(campaign.blocking_reasons ?? []).length > 0 ? (
              <p className="mt-1.5 flex flex-wrap items-center gap-1.5">
                <span className="text-2xs">{tGate("reasons")}:</span>
                {campaign.blocking_reasons!.map((r) => (
                  <Badge key={r} variant="rose-subtle">
                    {tReason.has(r) ? tReason(r) : r}
                  </Badge>
                ))}
              </p>
            ) : null}
          </AlertDescription>
        </Alert>
      ) : null}

      {conflict ? (
        <Alert variant="warning">
          <TriangleAlertIcon />
          <AlertTitle>{t("conflictTitle")}</AlertTitle>
          <AlertDescription className="flex items-center gap-2">
            {t("conflictDescription")}
            <Button
              type="button"
              variant="outline"
              size="xs"
              onClick={refetch}
            >
              {t("refresh")}
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}
      {bannerError ? (
        <Alert variant="destructive">
          <TriangleAlertIcon />
          <AlertDescription>{bannerError}</AlertDescription>
        </Alert>
      ) : null}

      {/* 기본 정보 */}
      <section className="rounded-2xl border border-border bg-card px-4 py-4">
        <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm sm:grid-cols-[auto_1fr_auto_1fr]">
          {campaign.objective ? (
            <>
              <dt className="text-muted-foreground">{t("objective")}</dt>
              <dd>{campaign.objective}</dd>
            </>
          ) : null}
          <dt className="text-muted-foreground">{t("mailbox")}</dt>
          <dd>{mailboxLabel || tCommon("nameUnavailable")}</dd>
          {campaign.owner_principal_id ? (
            <>
              <dt className="text-muted-foreground">{t("owner")}</dt>
              <dd>{ownerLabel ?? tCommon("nameUnavailable")}</dd>
            </>
          ) : null}
          <dt className="text-muted-foreground">{tList("colCreated")}</dt>
          <dd>{formatDateTime(campaign.created_at, locale)}</dd>
        </dl>
      </section>

      {/* revision·시간 규칙 */}
      {revision ? (
        <section className="flex flex-col gap-3 rounded-2xl border border-border bg-card px-4 py-4">
          <div className="flex items-center gap-2">
            <h2 className="text-sm font-medium">
              {t("revision", { no: revision.revision_no })}
            </h2>
            <StatusBadge status={revision.status} />
            {revision.status === "frozen" ? (
              <span className="text-2xs text-muted-foreground">
                {t("versionFrozen")}
              </span>
            ) : null}
          </div>
          <div className="flex flex-wrap items-center gap-1.5 text-2xs">
            <Badge variant="secondary">{revision.timezone}</Badge>
            {rules.window_start && rules.window_end ? (
              <Badge variant="secondary">
                {t("scheduleWindow", {
                  start: String(rules.window_start),
                  end: String(rules.window_end),
                })}
              </Badge>
            ) : null}
            {rules.internal_daily_cap != null ? (
              <Badge variant="secondary">
                {t("scheduleCap", { count: Number(rules.internal_daily_cap) })}
              </Badge>
            ) : null}
            {rules.minimum_gap_seconds != null ? (
              <Badge variant="secondary">
                {t("scheduleGap", {
                  seconds: Number(rules.minimum_gap_seconds),
                })}
              </Badge>
            ) : null}
            {rules.launch_not_before ? (
              <Badge variant="secondary">
                {t("scheduleNotBefore", {
                  time: formatDateTime(String(rules.launch_not_before), locale),
                })}
              </Badge>
            ) : null}
            {rules.launch_expires_at ? (
              <Badge variant="secondary">
                {t("scheduleExpires", {
                  time: formatDateTime(String(rules.launch_expires_at), locale),
                })}
              </Badge>
            ) : null}
            {revision.stop_on_reply ? (
              <Badge variant="info-subtle">{t("stopOnReply")}</Badge>
            ) : null}
            {revision.stop_on_booking ? (
              <Badge variant="info-subtle">{t("stopOnBooking")}</Badge>
            ) : null}
          </div>
          {(revision.steps ?? []).length > 0 ? (
            <ul className="flex flex-col gap-1.5">
              {(revision.steps ?? [])
                .slice()
                .sort((a, b) => a.step_order - b.step_order)
                .map((s) => (
                  <li
                    key={s.id ?? s.step_order}
                    className="flex flex-wrap items-center gap-2 rounded-lg border border-border px-3 py-2 text-sm"
                  >
                    <span className="font-medium">
                      {t("stepN", { order: s.step_order })}
                    </span>
                    <span className="text-2xs text-muted-foreground">
                      {t("savedTemplateVersion")}
                    </span>
                    <span className="text-2xs text-muted-foreground">
                      {s.delay_hours > 0
                        ? t("delayHoursValue", { hours: s.delay_hours })
                        : t("delayNone")}
                    </span>
                  </li>
                ))}
            </ul>
          ) : null}
        </section>
      ) : null}

      {/* 실행 현황 */}
      {campaign.stats && Object.keys(campaign.stats).length > 0 ? (
        <section className="rounded-2xl border border-border bg-card px-4 py-4">
          <h2 className="text-sm font-medium">{t("stats")}</h2>
          <dl className="mt-2 grid grid-cols-2 gap-2 text-sm sm:grid-cols-4">
            {Object.entries(campaign.stats).map(([k, v]) => (
              <div key={k} className="rounded-lg bg-app-panel-muted px-3 py-2">
                <dt className="text-2xs text-muted-foreground">{k}</dt>
                <dd className="font-medium">{v ?? "—"}</dd>
              </div>
            ))}
          </dl>
        </section>
      ) : null}

      {/* 실행 묶음 목록 */}
      <section className="flex flex-col gap-3 rounded-2xl border border-border bg-card px-4 py-4">
        <h2 className="text-sm font-medium">{t("launches")}</h2>
        {launches.length === 0 ? (
            <EmptyState compact title={t("noLaunches")} />
          ) : (
            <ul className="flex flex-col gap-2">
            {launches.map((l) => {
              const launchRevision =
                campaign.revisions?.find((candidate) => candidate.id === l.revision_id) ??
                revision;
              const deliveryChannelSupported = smtpChannelIds.has(
                l.delivery_channel_id ?? launchRevision?.delivery_channel_id ?? ""
              );
              return (
                <LaunchRow
                  key={l.id}
                  launch={l}
                  campaignId={campaign.id}
                  deliveryChannelSupported={deliveryChannelSupported}
                  gateBlocked={gateBlocked || !deliveryChannelSupported}
                  expanded={expandedLaunch === l.id}
                  onToggle={() =>
                    setExpandedLaunch((cur) => (cur === l.id ? null : l.id))
                  }
                  onError={handleError}
                  onChanged={refetch}
                />
              );
            })}
          </ul>
        )}
      </section>
    </div>
  );
}

function LaunchRow({
  launch,
  campaignId,
  deliveryChannelSupported,
  gateBlocked,
  expanded,
  onToggle,
  onError,
  onChanged,
}: {
  launch: CampaignLaunch;
  campaignId: string;
  deliveryChannelSupported: boolean;
  gateBlocked: boolean;
  expanded: boolean;
  onToggle: () => void;
  onError: (err: unknown) => void;
  onChanged: () => void;
}) {
  const t = useTranslations("campaigns.detail");
  const tGate = useTranslations("campaigns.channelGate");
  const tCommon = useTranslations("common");
  const locale = useLocale();
  const qc = useQueryClient();
  const errorMessage = useApiErrorMessage("campaigns");
  const approvalIdem = useIdempotencyKey();
  const pauseIdem = useIdempotencyKey();
  const cancelIdem = useIdempotencyKey();

  const [dialog, setDialog] = useState<"approve" | "pause" | "cancel" | null>(
    null
  );
  const [reason, setReason] = useState("");
  const [pending, setPending] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);

  const actions = launchActions(launch, {
    gateBlocked,
    allowedActions: deliveryChannelSupported ? launch.allowed_actions : [],
  });

  async function submit() {
    if (!dialog) return;
    setPending(true);
    setLocalError(null);
    try {
      if (dialog === "approve") {
        // 해시가 목록에 없으면 상세에서 가져와 검증한다 — 잘못된 스냅샷 승인 요청 방지.
        let manifestHash = launch.manifest_hash;
        if (!manifestHash) {
          const full = (await getCampaignLaunch(launch.id)).data;
          manifestHash = full.manifest_hash ?? undefined;
        }
        if (!manifestHash) throw new Error("manifest_hash missing");
        await requestLaunchApproval(
          launch.id,
          {
            expected_manifest_hash: manifestHash,
            reason: reason.trim() || undefined,
          },
          { idempotencyKey: approvalIdem.key() }
        );
        approvalIdem.settle();
        toast.success(t("approvalRequested"));
      } else if (dialog === "pause") {
        await pauseLaunch(
          launch.id,
          { reason: reason.trim() || undefined },
          { etag: launch.version, idempotencyKey: pauseIdem.key() }
        );
        pauseIdem.settle();
      } else {
        await cancelLaunch(
          launch.id,
          { reason: reason.trim() || undefined },
          { etag: launch.version, idempotencyKey: cancelIdem.key() }
        );
        cancelIdem.settle();
      }
      setDialog(null);
      setReason("");
      qc.invalidateQueries({ queryKey: ["campaigns", "detail", campaignId] });
      qc.invalidateQueries({ queryKey: ["approvals"] });
      onChanged();
    } catch (err) {
      if (dialog === "approve") approvalIdem.settle(err);
      else if (dialog === "pause") pauseIdem.settle(err);
      else cancelIdem.settle(err);
      setLocalError(errorMessage(err));
      onError(err);
    } finally {
      setPending(false);
    }
  }

  const dialogTitle =
    dialog === "approve"
      ? t("approvalRequestTitle")
      : dialog === "pause"
        ? t("pauseTitle")
        : t("cancelTitle");
  const dialogDescription =
    dialog === "approve"
      ? t("approvalRequestDescription")
      : dialog === "pause"
        ? undefined
        : t("cancelDescription");
  const reasonLabel =
    dialog === "approve"
      ? t("approvalReason")
      : dialog === "pause"
        ? t("pauseReason")
        : t("cancelReason");

  return (
    <li className="rounded-lg border border-border">
      <div className="flex flex-wrap items-center gap-2 px-3 py-2.5">
        <button
          type="button"
          aria-expanded={expanded}
          onClick={onToggle}
          className="flex min-w-0 flex-1 items-center gap-2 text-left text-sm"
        >
          {expanded ? (
            <ChevronDownIcon className="size-4 shrink-0" aria-hidden />
          ) : (
            <ChevronRightIcon className="size-4 shrink-0" aria-hidden />
          )}
          <span className="text-sm font-medium">{t("colLaunch")}</span>
          <StatusBadge status={launch.status} />
        </button>
        <span className="text-2xs text-muted-foreground">
          {t("colRecipients")}: {launch.recipients_count ?? "—"}
        </span>
        <span className="text-2xs text-muted-foreground">
          {t("colFrozen")}: {formatDateTime(launch.frozen_at, locale)}
        </span>
        <span className="text-2xs text-muted-foreground">
          {t("colExpires")}: {formatDateTime(launch.expires_at, locale)}
        </span>
        <span className="flex items-center gap-1.5">
          {actions.requestApproval.visible ? (
            <Button
              type="button"
              size="xs"
              disabled={actions.requestApproval.disabled}
              title={gateBlocked ? tGate("title") : undefined}
              onClick={() => setDialog("approve")}
            >
              {t("requestApproval")}
            </Button>
          ) : null}
          {actions.pause.visible ? (
            <Button
              type="button"
              size="xs"
              variant="outline"
              disabled={actions.pause.disabled}
              onClick={() => setDialog("pause")}
            >
              {t("pause")}
            </Button>
          ) : null}
          {actions.cancel.visible ? (
            <Button
              type="button"
              size="xs"
              variant="outline"
              disabled={actions.cancel.disabled}
              onClick={() => setDialog("cancel")}
            >
              {t("cancel")}
            </Button>
          ) : null}
        </span>
      </div>

      {!deliveryChannelSupported ? (
        <p role="status" className="px-3 pb-2.5 text-2xs text-muted-foreground">
          {t("retiredDeliveryChannel")}
        </p>
      ) : null}
      {launch.status === "building" ? (
        <p className="px-3 pb-2.5 text-2xs text-muted-foreground">
          {t("buildingLaunch")}
        </p>
      ) : null}
      {launch.status === "awaiting_approval" ? (
        <p className="px-3 pb-2.5 text-2xs text-muted-foreground">
          {t("approvalPending")}{" "}
          <Link
            href="/app/approvals"
            className="text-brand underline-offset-4 hover:underline"
          >
            {t("goToApprovals")}
          </Link>
        </p>
      ) : null}

      {expanded ? <LaunchDetailPanel launchId={launch.id} /> : null}

      <Dialog open={dialog !== null} onOpenChange={(o) => !o && setDialog(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{dialogTitle}</DialogTitle>
            {dialogDescription ? (
              <DialogDescription>{dialogDescription}</DialogDescription>
            ) : null}
          </DialogHeader>
          {dialog === "cancel" ? (
            <p className="text-2xs text-muted-foreground">{t("inFlightNote")}</p>
          ) : null}
          <div className="grid gap-1.5">
            <Label htmlFor={`launch-action-${launch.id}`}>{reasonLabel}</Label>
            <Textarea
              id={`launch-action-${launch.id}`}
              rows={3}
              value={reason}
              onChange={(e) => setReason(e.target.value)}
              placeholder={
                dialog === "approve" ? t("approvalReasonPlaceholder") : undefined
              }
            />
          </div>
          {localError ? (
            <p role="alert" className="text-sm text-destructive">
              {localError}
            </p>
          ) : null}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => setDialog(null)}
            >
              {tCommon("cancel")}
            </Button>
            <Button
              type="button"
              variant={dialog === "cancel" ? "destructive" : "default"}
              disabled={pending}
              onClick={submit}
            >
              {pending
                ? tCommon("loading")
                : dialog === "approve"
                  ? t("requestApproval")
                  : dialog === "pause"
                    ? t("pause")
                    : t("cancel")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </li>
  );
}

/** 실행 묶음 상세 — 고정된 발송 내용(messages)과 대상 실행 상태(enrollments). */
export function CampaignLaunchExecutionControl({
  launchId,
  onExecuted,
}: {
  launchId: string;
  onExecuted?: () => void | Promise<unknown>;
}) {
  const t = useTranslations("campaigns.detail.execution");
  const queryClient = useQueryClient();
  const executeKey = useIdempotencyKey();
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  const detail = useQuery({
    queryKey: ["campaigns", "launch-execution", launchId],
    queryFn: async () =>
      normalizeLaunch((await getCampaignLaunch(launchId)).data),
    retry: false,
    refetchInterval: (query) =>
      ["queued", "running"].includes(
        query.state.data?.execution_status ?? ""
      )
        ? 2_000
        : false,
  });

  const launch = detail.data;
  const executionStatus = launch?.execution_status ?? "not_ready";
  const canExecute = Boolean(
    launch?.status === "approved" &&
      executionStatus === "ready" &&
      launch.allowed_actions?.includes("execute_launch")
  );

  const execute = useMutation({
    mutationFn: () => {
      if (!launch?.manifest_hash || !canExecute) {
        throw new Error(t("notReady"));
      }
      return executeCampaignLaunch(
        launch.id,
        {
          expected_manifest_hash: launch.manifest_hash,
          expected_version: launch.version,
        },
        { idempotencyKey: executeKey.key() }
      );
    },
    onSuccess: async (result) => {
      executeKey.settle();
      setActionError(null);
      setConfirmOpen(false);
      queryClient.setQueryData<CampaignLaunchDetail>(
        ["campaigns", "launch-execution", launchId],
        (current) =>
          current
            ? {
                ...current,
                status: result.data.launch_status,
                execution_status: result.data.execution_status,
                activation_job_id: result.data.job_id,
                job_id: result.data.job_id,
                version: result.data.version,
                manifest_hash: result.data.manifest_hash,
              }
            : current
      );
      await Promise.all([
        detail.refetch(),
        queryClient.invalidateQueries({
          queryKey: ["campaigns", "launch-detail", launchId],
        }),
        queryClient.invalidateQueries({ queryKey: ["campaigns", "detail"] }),
      ]);
      await onExecuted?.();
    },
    onError: (error) => {
      executeKey.settle(error);
      setActionError(t("requestFailed"));
      void detail.refetch();
    },
  });

  const launchStatusLabel = (status: string) =>
    t.has(`launchStatuses.${status}`)
      ? t(`launchStatuses.${status}`)
      : status;
  const executionStatusLabel = (status: string) =>
    t.has(`statuses.${status}`) ? t(`statuses.${status}`) : status;

  return (
    <section
      aria-label={t("title")}
      className="grid gap-2 rounded-xl border border-border p-3"
    >
      <h3 className="text-sm font-medium">{t("title")}</h3>
      {detail.isPending ? (
        <p role="status" className="text-sm text-muted-foreground">
          {t("loading")}
        </p>
      ) : detail.isError || !launch ? (
        <p role="alert" className="text-sm text-destructive">
          {t("loadFailed")}
        </p>
      ) : (
        <>
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm sm:grid-cols-2">
            <dt className="text-muted-foreground">{t("launchStatusLabel")}</dt>
            <dd>{launchStatusLabel(launch.status)}</dd>
            <dt className="text-muted-foreground">{t("executionStatusLabel")}</dt>
            <dd>{executionStatusLabel(executionStatus)}</dd>
            {launch.activation_job_id ? (
              <>
                <dt className="text-muted-foreground">{t("jobId")}</dt>
                <dd className="break-all font-mono text-xs">
                  {launch.activation_job_id}
                </dd>
              </>
            ) : null}
          </dl>

          {executionStatus === "ready" && launch.status === "approved" ? (
            <p className="text-xs text-muted-foreground">
              {launch.allowed_actions?.includes("execute_launch")
                ? t("readyDescription")
                : t("permissionDenied")}
            </p>
          ) : executionStatus === "queued" ? (
            <Alert>
              <AlertDescription>
                {t("queuedDescription", {
                  job_id: launch.activation_job_id ?? t("jobPending"),
                })}
              </AlertDescription>
            </Alert>
          ) : executionStatus === "running" ? (
            <Alert>
              <AlertDescription>
                {t("runningDescription", {
                  job_id: launch.activation_job_id ?? t("jobPending"),
                })}
              </AlertDescription>
            </Alert>
          ) : executionStatus === "activated" ? (
            <Alert variant="success">
              <AlertDescription>{t("activatedDescription")}</AlertDescription>
            </Alert>
          ) : executionStatus === "failed" ? (
            <Alert variant="warning">
              <AlertDescription>{t("failedDescription")}</AlertDescription>
            </Alert>
          ) : executionStatus === "canceled" ? (
            <Alert variant="warning">
              <AlertDescription>{t("canceledDescription")}</AlertDescription>
            </Alert>
          ) : executionStatus === "not_ready" ? (
            <p className="text-xs text-muted-foreground">{t("notReady")}</p>
          ) : null}

          {canExecute ? (
            <Button
              type="button"
              size="sm"
              className="w-fit"
              disabled={execute.isPending || detail.isFetching}
              onClick={() => setConfirmOpen(true)}
            >
              {t("executeAction")}
            </Button>
          ) : null}

          {actionError ? (
            <p role="alert" className="text-sm text-destructive">
              {actionError}
            </p>
          ) : null}

          <Dialog open={confirmOpen} onOpenChange={setConfirmOpen}>
            <DialogContent>
              <DialogHeader>
                <DialogTitle>{t("dialogTitle")}</DialogTitle>
                <DialogDescription>
                  {t("dialogDescription", {
                    count:
                      launch.enrollments?.total ??
                      launch.recipients_count ??
                      0,
                  })}
                </DialogDescription>
              </DialogHeader>
              <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 rounded-lg border border-border bg-app-panel-muted px-3 py-2 text-sm">
                <dt className="text-muted-foreground">{t("recipients")}</dt>
                <dd>
                  {launch.enrollments?.total ??
                    launch.recipients_count ??
                    0}
                </dd>
                <dt className="text-muted-foreground">{t("manifestHash")}</dt>
                <dd className="break-all font-mono text-xs">
                  {launch.manifest_hash
                    ? shortHash(launch.manifest_hash, 24)
                    : "—"}
                </dd>
                <dt className="text-muted-foreground">{t("version")}</dt>
                <dd>{launch.version}</dd>
              </dl>
              {launch.messages.items[0] ? (
                <div className="grid gap-1 rounded-lg border border-border px-3 py-2">
                  <p className="text-sm font-medium">
                    {launch.messages.items[0].subject}
                  </p>
                  <p className="max-h-32 overflow-auto whitespace-pre-wrap text-xs text-muted-foreground">
                    {launch.messages.items[0].body_text}
                  </p>
                </div>
              ) : null}
              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  disabled={execute.isPending}
                  onClick={() => setConfirmOpen(false)}
                >
                  {t("cancel")}
                </Button>
                <Button
                  type="button"
                  disabled={execute.isPending || !canExecute}
                  onClick={() => execute.mutate()}
                >
                  {execute.isPending ? t("executing") : t("confirmExecute")}
                </Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        </>
      )}
    </section>
  );
}

function LaunchDetailPanel({ launchId }: { launchId: string }) {
  const t = useTranslations("campaigns.detail");
  const locale = useLocale();
  const [extraMessages, setExtraMessages] = useState<LaunchMessageSnapshot[]>([]);
  const [cursorOverride, setCursorOverride] = useState<string | null | undefined>(undefined);
  const [loadingMore, setLoadingMore] = useState(false);
  const [loadError, setLoadError] = useState<Error | null>(null);

  const detail = useQuery({
    queryKey: ["campaigns", "launch-detail", launchId],
    queryFn: async () => getCampaignLaunch(launchId, null),
  });

  const d = detail.data ? normalizeLaunch(detail.data.data) : undefined;
  const nextCursor = cursorOverride === undefined
    ? d?.messages.next_cursor ?? null
    : cursorOverride;
  const messages = [...(d?.messages.items ?? []), ...extraMessages];

  async function loadMore() {
    if (!nextCursor) return;
    setLoadingMore(true);
    setLoadError(null);
    try {
      const result = await getCampaignLaunch(launchId, nextCursor);
      const next = normalizeLaunch(result.data);
      setExtraMessages((current) => [...current, ...next.messages.items]);
      setCursorOverride(next.messages.next_cursor);
    } catch (cause) {
      setLoadError(cause instanceof Error ? cause : new Error(t("loadMoreError")));
    } finally {
      setLoadingMore(false);
    }
  }

  if (detail.isLoading) {
    return (
      <div className="flex flex-col gap-2 px-3 pb-3">
        <Skeleton className="h-16 w-full rounded-lg" />
      </div>
    );
  }
  if (detail.isError) {
    return (
      <div className="px-3 pb-3">
        <ErrorState error={detail.error} onRetry={() => detail.refetch()} />
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3 border-t border-border/60 px-3 py-3">
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <dt className="text-muted-foreground">{t("manifestHash")}</dt>
        <dd className="font-mono text-ui break-all">
          {d?.manifest_hash ? shortHash(d.manifest_hash, 24) : "—"}
        </dd>
        <dt className="text-muted-foreground">{t("frozenAt")}</dt>
        <dd>{formatDateTime(d?.frozen_at, locale)}</dd>
      </dl>

      <div className="grid gap-1.5">
        <h3 className="text-xs font-medium text-muted-foreground">
          {t("launchContents")}
        </h3>
        {messages.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            {t("launchContentsEmpty")}
          </p>
        ) : (
          <ul className="flex flex-col gap-1.5">
            {messages.map((m) => (
              <li
                key={m.outbound_message_id}
                className="rounded-lg border border-border px-3 py-2 text-sm"
              >
                <div className="flex flex-wrap items-center gap-2">
                  <Badge variant="secondary">
                    {t("stepN", { order: m.step_order })}
                  </Badge>
                  <span className="text-muted-foreground">
                    → {m.to_address_snapshot}
                  </span>
                  <StatusBadge status={m.status} />
                </div>
                <p className="mt-1 font-medium">{m.subject}</p>
                <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap rounded-lg bg-muted/50 p-3 text-xs">{m.body_text}</pre>
                {m.body_html ? <details className="mt-2"><summary className="cursor-pointer text-xs text-muted-foreground">{t("renderedHtml")}</summary><iframe title={t("renderedHtml")} sandbox="" srcDoc={m.body_html} className="mt-2 h-48 w-full rounded-lg border border-border bg-background" /></details> : null}
                <p className="mt-2 font-mono text-2xs text-muted-foreground">{t("payloadHash")}: {shortHash(m.payload_hash, 16)}</p>
              </li>
            ))}
          </ul>
        )}
      </div>

      {d?.enrollments ? (
        <div className="grid gap-1.5">
          <h3 className="text-xs font-medium text-muted-foreground">
            {t("enrollmentsTitle")}
          </h3>
          <dl className="grid grid-cols-2 gap-2 text-sm sm:grid-cols-3">
            {(["total", "pending", "active", "completed", "stopped", "suppressed"] as const).map((key) => (
              <div key={key} className="rounded-lg border border-border px-3 py-2">
                <dt className="text-xs text-muted-foreground">{t(`enrollmentCounts.${key}`)}</dt>
                <dd className="mt-1 font-medium tabular-nums">{d.enrollments[key]}</dd>
              </div>
            ))}
          </dl>
        </div>
      ) : null}

      {d &&
      (d.status === "approved" ||
        (d.execution_status && d.execution_status !== "not_ready")) ? (
        <CampaignLaunchExecutionControl
          launchId={launchId}
          onExecuted={() => detail.refetch()}
        />
      ) : null}

      {loadError ? <ErrorState error={loadError} onRetry={() => void loadMore()} /> : null}

      {nextCursor ? (
        <div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={loadingMore}
            onClick={() => void loadMore()}
          >
            {loadingMore ? t("loadingMore") : t("loadMore")}
          </Button>
        </div>
      ) : null}
    </div>
  );
}
