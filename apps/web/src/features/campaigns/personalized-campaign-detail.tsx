"use client";

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, GripVertical, Plus } from "lucide-react";
import { useLocale, useTranslations } from "next-intl";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useEffect, useMemo, useRef, useState } from "react";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { CampaignBoardAddTargetDialog } from "@/features/campaigns/campaign-board-add-target-dialog";
import { CampaignTargetDetailDialog } from "@/features/campaigns/campaign-target-detail-dialog";
import { getCampaign } from "@/lib/api/campaigns";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  getSalesCampaign,
  queueSalesCampaignTarget,
  updateSalesCampaignTarget,
  type SalesCampaignTarget,
  type SalesCampaignWorkflowStage,
} from "@/lib/api/sales-campaigns";
import { formatDateTime } from "@/features/campaigns/format";

const BOARD_STAGES = [
  "research",
  "draft",
  "review",
  "queued",
  "accepted",
  "attention",
] as const;

const POINTER_DRAG_THRESHOLD_PX = 7;

interface ActiveBoardPointerDrag {
  pointerId: number;
  targetId: string;
  startX: number;
  startY: number;
  activated: boolean;
  captureTarget: HTMLElement;
}

type BoardStage = (typeof BOARD_STAGES)[number];

function isBoardStage(value: string): value is BoardStage {
  return (BOARD_STAGES as readonly string[]).includes(value);
}

function targetName(target: SalesCampaignTarget, unavailable: string) {
  return target.company_name?.trim() || unavailable;
}

function approvedCurrentSnapshot(target: SalesCampaignTarget) {
  const draft = target.current_draft;
  const outbound = target.outbound;
  return Boolean(
    draft &&
      draft.status === "frozen" &&
      draft.outbound_message_id &&
      outbound &&
      outbound.outbound_message_id === draft.outbound_message_id &&
      outbound.payload_hash &&
      outbound.approval_status === "approved" &&
      outbound.approved_at &&
      (!outbound.approval_expires_at ||
        Date.parse(outbound.approval_expires_at) > Date.now())
  );
}

const EMPTY_TARGETS: SalesCampaignTarget[] = [];

export function PersonalizedCampaignDetail({ campaignId }: { campaignId: string }) {
  const t = useTranslations("campaigns.personalized.board");
  const locale = useLocale();
  const router = useRouter();
  const queryClient = useQueryClient();
  const [addOpen, setAddOpen] = useState(false);
  const [selectedTargetId, setSelectedTargetId] = useState<string | null>(null);
  const [queueNotice, setQueueNotice] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const activePointerDragRef = useRef<ActiveBoardPointerDrag | null>(null);
  const [draggingTargetId, setDraggingTargetId] = useState<string | null>(null);
  const [pointerDropStage, setPointerDropStage] = useState<BoardStage | null>(null);
  const [batchTimes, setBatchTimes] = useState<Record<string, string>>({});
  const pendingTargetIdsRef = useRef(new Set<string>());
  const [pendingTargetIds, setPendingTargetIds] = useState<ReadonlySet<string>>(() => new Set());
  const operationKeys = useRef(new Map<string, string>());

  const detail = useQuery({
    queryKey: ["sales-campaigns", "detail", campaignId],
    queryFn: async () => (await getSalesCampaign(campaignId)).data,
    refetchInterval: 2_000,
    retry: false,
  });
  const isLegacyNotFound =
    detail.isError &&
    detail.error instanceof ApiError &&
    detail.error.status === 404;
  const legacy = useQuery({
    queryKey: ["campaigns", "legacy-redirect", campaignId],
    enabled: isLegacyNotFound,
    queryFn: async () => (await getCampaign(campaignId)).data,
    retry: false,
  });

  useEffect(() => {
    if (legacy.data?.id) {
      router.replace(`/app/campaigns/bulk/${encodeURIComponent(campaignId)}`);
    }
  }, [campaignId, legacy.data?.id, router]);

  const campaign = detail.data?.campaign;
  const targets = detail.data?.targets ?? EMPTY_TARGETS;
  const selectedTarget = targets.find((target) => target.id === selectedTargetId) ?? null;
  const nextBatchAt = useMemo(() => {
    const values = targets
      .filter((target) =>
        target.outbound?.status === "queued" ||
        target.outbound?.status === "claimed" ||
        target.outbound?.status === "submitting" ||
        (Boolean(batchTimes[target.id]) &&
          target.stage !== "accepted" &&
          target.stage !== "attention")
      )
      .map((target) => target.outbound?.not_before ?? batchTimes[target.id])
      .filter((value): value is string => Boolean(value))
      .sort((a, b) => Date.parse(a) - Date.parse(b));
    return values[0] ?? null;
  }, [batchTimes, targets]);

  function beginTargetAction(targetId: string) {
    if (pendingTargetIdsRef.current.has(targetId)) return false;
    const next = new Set(pendingTargetIdsRef.current);
    next.add(targetId);
    pendingTargetIdsRef.current = next;
    setPendingTargetIds(next);
    return true;
  }

  function endTargetAction(targetId: string) {
    const next = new Set(pendingTargetIdsRef.current);
    next.delete(targetId);
    pendingTargetIdsRef.current = next;
    setPendingTargetIds(next);
  }

  function getOperationKey(scope: string) {
    let key = operationKeys.current.get(scope);
    if (!key) {
      key = idempotencyKey();
      operationKeys.current.set(scope, key);
    }
    return key;
  }

  function settleOperation(scope: string, error?: unknown) {
    if (
      !error ||
      (error instanceof ApiError &&
        error.problem.retryable !== true &&
        error.status < 500)
    ) {
      operationKeys.current.delete(scope);
    }
  }

  async function refresh() {
    await detail.refetch();
  }

  async function moveTarget(
    target: SalesCampaignTarget,
    stage: BoardStage
  ) {
    setActionError(null);
    setQueueNotice(null);
    if (stage === "queued") {
      await queueTarget(target);
      return;
    }
    if (!(BOARD_STAGES.slice(0, 3) as readonly string[]).includes(stage)) return;
    if (target.stage === "review" && target.current_draft?.status === "frozen") {
      setSelectedTargetId(target.id);
      setQueueNotice(t("frozenReviewCopyRequired"));
      return;
    }
    if (!beginTargetAction(target.id)) return;
    const scope = `stage:${campaignId}:${target.id}:${target.version}:${stage}`;
    try {
      await updateSalesCampaignTarget(
        campaignId,
        target.id,
        {
          stage: stage as SalesCampaignWorkflowStage,
          expected_target_version: target.version,
        },
        { idempotencyKey: getOperationKey(scope) }
      );
      settleOperation(scope);
      await queryClient.invalidateQueries({
        queryKey: ["sales-campaigns", "detail", campaignId],
      });
    } catch (error) {
      settleOperation(scope, error);
      setActionError(t("moveError"));
      await refresh();
    } finally {
      endTargetAction(target.id);
    }
  }

  async function queueTarget(target: SalesCampaignTarget) {
    if (!approvedCurrentSnapshot(target)) {
      setSelectedTargetId(target.id);
      setQueueNotice(
        target.current_draft?.status === "frozen"
          ? t("queueApprovalNeeded")
          : t("queueMissingSnapshot")
      );
      return;
    }
    if (!beginTargetAction(target.id)) return;
    const outbound = target.outbound!;
    const scope = `queue:${campaignId}:${target.id}:${target.version}:${outbound.payload_hash}`;
    setActionError(null);
    setQueueNotice(null);
    try {
      const response = await queueSalesCampaignTarget(
        campaignId,
        target.id,
        {
          outbound_message_id: outbound.outbound_message_id,
          expected_payload_hash: outbound.payload_hash,
          expected_target_version: target.version,
        },
        { idempotencyKey: getOperationKey(scope) }
      );
      settleOperation(scope);
      setBatchTimes((current) => ({
        ...current,
        [target.id]: response.data.batch_at,
      }));
      await queryClient.invalidateQueries({
        queryKey: ["sales-campaigns", "detail", campaignId],
      });
    } catch (error) {
      settleOperation(scope, error);
      setActionError(t("queueError"));
      await refresh();
    } finally {
      endTargetAction(target.id);
    }
  }

function boardStageAtPointer(clientX: number, clientY: number): BoardStage | null {
    const element = document.elementFromPoint(clientX, clientY);
    const lane = element?.closest(
      "[data-sales-campaign-id][data-sales-campaign-stage]"
    ) as HTMLElement | null;
    if (!lane || lane.dataset.salesCampaignId !== campaignId) return null;
    const stage = lane.dataset.salesCampaignStage ?? "";
    if (
      !isBoardStage(stage) ||
      !(BOARD_STAGES.slice(0, 4) as readonly string[]).includes(stage)
    ) {
      return null;
    }
    return stage;
  }

  function clearPointerDrag(pointerId?: number) {
    const active = activePointerDragRef.current;
    if (!active || (pointerId !== undefined && pointerId !== active.pointerId)) return;

    activePointerDragRef.current = null;
    setDraggingTargetId(null);
    setPointerDropStage(null);
    try {
      if (
        typeof active.captureTarget.hasPointerCapture === "function" &&
        active.captureTarget.hasPointerCapture(active.pointerId)
      ) {
        active.captureTarget.releasePointerCapture(active.pointerId);
      }
    } catch {
      // Pointer capture may already have been released by the browser.
    }
  }

  function handlePointerDown(
    event: React.PointerEvent<HTMLElement>,
    target: SalesCampaignTarget
  ) {
    const stage = isBoardStage(target.stage) ? target.stage : "attention";
    if (
      activePointerDragRef.current ||
      event.button !== 0 ||
      !(BOARD_STAGES.slice(0, 3) as readonly string[]).includes(stage) ||
      pendingTargetIdsRef.current.has(target.id)
    ) {
      return;
    }

    event.preventDefault();
    const captureTarget = event.currentTarget;
    activePointerDragRef.current = {
      pointerId: event.pointerId,
      targetId: target.id,
      startX: event.clientX,
      startY: event.clientY,
      activated: false,
      captureTarget,
    };
    try {
      if (typeof captureTarget.setPointerCapture === "function") {
        captureTarget.setPointerCapture(event.pointerId);
      }
    } catch {
      // Movement can still be handled while the pointer remains over the board.
    }
  }

  function handlePointerMove(event: React.PointerEvent) {
    const active = activePointerDragRef.current;
    if (!active || event.pointerId !== active.pointerId) return;

    event.preventDefault();
    const deltaX = event.clientX - active.startX;
    const deltaY = event.clientY - active.startY;
    if (!active.activated) {
      if (deltaX * deltaX + deltaY * deltaY < POINTER_DRAG_THRESHOLD_PX ** 2) return;
      active.activated = true;
      setDraggingTargetId(active.targetId);
    }
    setPointerDropStage(boardStageAtPointer(event.clientX, event.clientY));
  }

  function handlePointerUp(event: React.PointerEvent) {
    const active = activePointerDragRef.current;
    if (!active || event.pointerId !== active.pointerId) return;

    const deltaX = event.clientX - active.startX;
    const deltaY = event.clientY - active.startY;
    if (
      !active.activated &&
      deltaX * deltaX + deltaY * deltaY >= POINTER_DRAG_THRESHOLD_PX ** 2
    ) {
      active.activated = true;
      setDraggingTargetId(active.targetId);
    }
    const destination = active.activated
      ? boardStageAtPointer(event.clientX, event.clientY)
      : null;
    clearPointerDrag(event.pointerId);
    if (!destination) return;

    const dragged = targets.find((target) => target.id === active.targetId);
    if (
      !dragged ||
      dragged.stage === destination ||
      pendingTargetIdsRef.current.has(dragged.id)
    ) {
      return;
    }
    void moveTarget(dragged, destination);
  }

  function handlePointerCancel(event: React.PointerEvent) {
    clearPointerDrag(event.pointerId);
  }

  if (detail.isPending || (isLegacyNotFound && legacy.isPending)) {
    return (
      <div className="mx-auto flex w-full max-w-7xl flex-col gap-5 px-6 py-6">
        <Skeleton className="h-8 w-64" />
        <Skeleton className="h-20 w-full rounded-2xl" />
        <Skeleton className="h-96 w-full rounded-2xl" />
      </div>
    );
  }

  if (detail.isError && !legacy.data) {
    return (
      <div className="mx-auto w-full max-w-6xl px-6 py-6">
        {isLegacyNotFound && legacy.isError ? (
          <EmptyState
            title={t("notFoundTitle")}
            description={t("notFoundDescription")}
            action={
              <Button asChild variant="outline" size="sm">
                <Link href="/app/campaigns">{t("back")}</Link>
              </Button>
            }
          />
        ) : (
          <ErrorState
            error={detail.error}
            onRetry={() => void Promise.all([detail.refetch(), legacy.refetch()])}
          />
        )}
      </div>
    );
  }

  if (!campaign) {
    return (
      <div className="mx-auto w-full max-w-6xl px-6 py-6">
        <EmptyState title={t("notFoundTitle")} description={t("notFoundDescription")} />
      </div>
    );
  }

  return (
    <div className="mx-auto flex w-full max-w-[1600px] flex-col gap-5 px-6 py-6">
      <Link
        href="/app/campaigns"
        className="inline-flex w-fit items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft aria-hidden className="size-4" />
        {t("back")}
      </Link>
      <PageHeader
        title={campaign.name}
        description={campaign.objective ?? undefined}
        actions={
          <div className="flex flex-wrap items-center gap-2">
            <StatusBadge status={campaign.sender_status} />
            <Button type="button" size="sm" onClick={() => setAddOpen(true)}>
              <Plus aria-hidden />
              {t("addTarget")}
            </Button>
          </div>
        }
      />

      <div className="flex flex-wrap items-center justify-between gap-3 rounded-2xl border border-border bg-app-panel-muted px-4 py-3">
        <div className="min-w-0">
          <p className="text-xs text-muted-foreground">{t("defaultSender")}</p>
          <p className="truncate text-sm font-medium">
            {campaign.from_name} &lt;{campaign.from_address}&gt;
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-3 text-sm text-muted-foreground">
          <span>{t("targetCount", { count: targets.length })}</span>
          {nextBatchAt ? (
            <span>
              {t("nextBatch")}: {formatDateTime(nextBatchAt, locale)}
            </span>
          ) : null}
          <span className="text-2xs">{t("refreshing")}</span>
        </div>
      </div>

      {actionError ? (
        <Alert variant="destructive">
          <AlertDescription>{actionError}</AlertDescription>
        </Alert>
      ) : null}
      <p className="text-2xs text-muted-foreground">{t("dragHint")}</p>
      <div
        role="region"
        aria-label={t("boardLabel")}
        tabIndex={0}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onPointerCancel={handlePointerCancel}
        onLostPointerCapture={handlePointerCancel}
        className="min-w-0 max-w-full overflow-x-auto pb-2"
      >
        <div className="grid min-w-[1380px] grid-cols-6 gap-3">
        {BOARD_STAGES.map((stage) => {
          const laneTargets = targets.filter((target) => {
            const effectiveStage = isBoardStage(target.stage) ? target.stage : "attention";
            return effectiveStage === stage;
          });
          const stageLabel = t(`columns.${stage}` as never);
          return (
            <section
              key={stage}
              role="region"
              aria-label={stageLabel}
              data-sales-campaign-id={campaignId}
              data-sales-campaign-stage={stage}
              className={`flex min-w-0 flex-col gap-2 rounded-2xl border border-border bg-app-panel-muted p-2 ${pointerDropStage === stage ? "ring-2 ring-primary" : ""}`}
            >
              <div className="flex items-center justify-between gap-2 px-1 py-1">
                <h2 className="text-sm font-semibold">{stageLabel}</h2>
                <Badge variant="secondary">{laneTargets.length}</Badge>
              </div>
              <div className="flex min-h-20 flex-col gap-2">
                {laneTargets.map((target) => (
                  <CampaignTargetCard
                    key={target.id}
                    target={target}
                    busy={pendingTargetIds.has(target.id)}
                    stage={stage}
                    isDragging={draggingTargetId === target.id}
                    batchAt={target.outbound?.not_before ?? batchTimes[target.id] ?? null}
                    onOpen={() => {
                      setQueueNotice(null);
                      setSelectedTargetId(target.id);
                    }}
                    onMove={(next) => void moveTarget(target, next)}
                    onPointerDown={(event) => handlePointerDown(event, target)}
                  />
                ))}
                {laneTargets.length === 0 ? (
                  <p className="px-2 py-4 text-center text-2xs text-muted-foreground">
                    {t("emptyLane")}
                  </p>
                ) : null}
              </div>
            </section>
          );
        })}
        </div>
      </div>

      {targets.length === 0 ? (
        <EmptyState
          title={t("emptyTitle")}
          description={t("emptyDescription")}
          action={
            <Button type="button" size="sm" onClick={() => setAddOpen(true)}>
              <Plus aria-hidden />
              {t("addTarget")}
            </Button>
          }
        />
      ) : null}

      <CampaignBoardAddTargetDialog
        campaignId={campaign.id}
        open={addOpen}
        onOpenChange={setAddOpen}
        onAdded={() => void refresh()}
      />
      {selectedTarget ? (
        <CampaignTargetDetailDialog
          key={selectedTarget.id}
          campaign={campaign}
          target={selectedTarget}
          queueNotice={queueNotice}
          open
          onOpenChange={(open) => {
            if (!open) {
              setSelectedTargetId(null);
              setQueueNotice(null);
            }
          }}
          onChanged={() => refresh()}
        />
      ) : null}
    </div>
  );
}

function CampaignTargetCard({
  target,
  busy,
  stage,
  batchAt,
  onOpen,
  onMove,
  isDragging,
  onPointerDown,
}: {
  target: SalesCampaignTarget;
  busy: boolean;
  stage: BoardStage;
  batchAt: string | null;
  onOpen: () => void;
  onMove: (stage: BoardStage) => void;
  isDragging: boolean;
  onPointerDown: (event: React.PointerEvent<HTMLElement>) => void;
}) {
  const t = useTranslations("campaigns.personalized.board");
  const common = useTranslations("common");
  const locale = useLocale();
  const company = targetName(target, common("nameUnavailable"));
  const isEditable = (BOARD_STAGES.slice(0, 3) as readonly string[]).includes(stage);
  const nextStage = stage === "research" ? "draft" : stage === "draft" ? "review" : null;
  const previousStage = stage === "draft"
    ? "research"
    : stage === "review" && target.current_draft?.status !== "frozen"
      ? "draft"
      : null;
  const outboundStatus = target.outbound?.status ?? target.current_draft?.status;

  return (
    <article
      aria-label={company}
      draggable={false}
      aria-busy={busy}
      className={`flex min-w-0 flex-col gap-2 rounded-xl border border-border bg-card p-3 ${isDragging ? "opacity-50" : ""}`}
    >
      <div className="flex min-w-0 items-start gap-1">
      <button
        type="button"
        onClick={onOpen}
        aria-label={t("openTarget", { company })}
        className="min-w-0 flex-1 text-left outline-none focus-visible:rounded-lg focus-visible:ring-[3px] focus-visible:ring-ring/50"
      >
        <span className="block truncate text-sm font-semibold">{company}</span>
        <span className="mt-0.5 block truncate text-2xs text-muted-foreground">
          {target.contact_name ?? ""}
          {target.email_address ? ` · ${target.email_address}` : ""}
        </span>
        {target.current_draft?.subject ? (
          <span className="mt-2 block line-clamp-2 text-sm text-foreground">
            {target.current_draft.subject}
          </span>
        ) : null}
      </button>
      {isEditable ? (
        <span
          draggable={false}
          onPointerDown={onPointerDown}
          aria-hidden="true"
          title={t("dragTarget", { company })}
          className="mt-0.5 flex size-7 shrink-0 cursor-grab select-none touch-none items-center justify-center rounded text-muted-foreground hover:bg-accent active:cursor-grabbing"
        >
          <GripVertical className="size-4" />
        </span>
      ) : null}
      </div>
      {outboundStatus ? <StatusBadge status={outboundStatus} /> : null}
      {stage === "queued" ? (
        <p className="text-2xs text-muted-foreground">
          {batchAt ? `${t("batchAt")}: ${formatDateTime(batchAt, locale)}` : t("queuedWaiting")}
        </p>
      ) : stage === "accepted" || stage === "attention" ? (
        batchAt ? (
          <p className="text-2xs text-muted-foreground">
            {t("originalBatchAt")}: {formatDateTime(batchAt, locale)}
          </p>
        ) : null
      ) : null}
      {stage === "accepted" ? (
        <p className="text-2xs text-muted-foreground">{t("acceptedNote")}</p>
      ) : null}
      {stage === "attention" ? (
        <p className="text-2xs text-muted-foreground">{t("attentionNote")}</p>
      ) : null}
      {nextStage ? (
        <Button
          type="button"
          variant="outline"
          size="xs"
          onClick={() => onMove(nextStage)}
          disabled={busy}
          aria-label={t("moveToStage", {
            company,
            stage: t(`columns.${nextStage}` as never),
          })}
        >
          {t("moveForward", { stage: t(`columns.${nextStage}` as never) })}
        </Button>
      ) : null}
      {previousStage ? (
        <Button
          type="button"
          variant="outline"
          size="xs"
          onClick={() => onMove(previousStage)}
          disabled={busy}
          aria-label={t("moveBackToStage", {
            company,
            stage: t(`columns.${previousStage}` as never),
          })}
        >
          {t("moveBackward", { stage: t(`columns.${previousStage}` as never) })}
        </Button>
      ) : null}
      {stage === "review" ? (
        <Button
          type="button"
          variant="outline"
          size="xs"
          onClick={() => onMove("queued")}
          disabled={busy}
          aria-label={t("queueCompany", { company })}
        >
          {t("queue")}
        </Button>
      ) : null}
    </article>
  );
}
