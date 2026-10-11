"use client";

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, GripVertical, Plus, Loader2, LockKeyhole } from "lucide-react";
import { useLocale, useTranslations } from "next-intl";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";

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
import { freezeMessageDraft, getMessageDraft } from "@/lib/api/sending";
import { readEtag } from "@/lib/api/mutate";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  getSalesCampaign,
  getSalesCampaignSender,
  refreshSalesCampaignSender,
  queueSalesCampaignTarget,
  updateSalesCampaignTarget,
  type SalesCampaignTarget,
  type SalesCampaignResponse,
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
  clientX: number;
  clientY: number;
  offsetX: number;
  offsetY: number;
  width: number;
  height: number;
}

type BoardStage = (typeof BOARD_STAGES)[number];

function isBoardStage(value: string): value is BoardStage {
  return (BOARD_STAGES as readonly string[]).includes(value);
}

function targetName(target: SalesCampaignTarget, unavailable: string) {
  return target.company_name?.trim() || unavailable;
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
  const boardRef = useRef<HTMLDivElement>(null);
  const previewRef = useRef<HTMLDivElement>(null);
  const [dragVisual, setDragVisual] = useState<{width: number; height: number; x: number; y: number} | null>(null);
  const suppressClickUntil = useRef(0);
  const [optimisticStages, setOptimisticStages] = useState<Record<string, BoardStage>>({});
  const [moveAnnouncement, setMoveAnnouncement] = useState("");
  const [batchTimes, setBatchTimes] = useState<Record<string, string>>({});
  const pendingTargetIdsRef = useRef(new Set<string>());
  const [pendingTargetIds, setPendingTargetIds] = useState<ReadonlySet<string>>(() => new Set());
  const operationKeys = useRef(new Map<string, string>());
  const stageIntents = useRef(new Map<string, BoardStage>());

  const detail = useQuery({
    queryKey: ["sales-campaigns", "detail", campaignId],
    queryFn: async () => (await getSalesCampaign(campaignId)).data,
    refetchInterval: draggingTargetId || pendingTargetIds.size ? false : 2_000,
    refetchOnWindowFocus: !draggingTargetId && pendingTargetIds.size === 0,
    retry: false,
  });
  // 기본 발신자(별칭 등)가 바뀌면 아직 발송 전인 캠페인도 곧바로 따라가도록 맞춘다.
  const defaultSender = useQuery({
    queryKey: ["sales-campaigns", "sender"],
    queryFn: async () => (await getSalesCampaignSender()).data,
    refetchInterval: 5_000,
    retry: false,
  });
  const syncedSenderRef = useRef<string | null>(null);
  const campaignSmtpProfileId = detail.data?.campaign.smtp_profile_id;
  const defaultSmtpProfileId = defaultSender.data?.sender?.ready
    ? defaultSender.data.sender.smtp_profile_id
    : undefined;
  useEffect(() => {
    if (!campaignSmtpProfileId || !defaultSmtpProfileId) return;
    if (campaignSmtpProfileId === defaultSmtpProfileId) return;
    const key = `${campaignId}:${defaultSmtpProfileId}`;
    if (syncedSenderRef.current === key) return;
    syncedSenderRef.current = key;
    void refreshSalesCampaignSender(campaignId)
      .then(() => queryClient.invalidateQueries({ queryKey: ["sales-campaigns"] }))
      .catch(() => undefined); // 이미 발송·예약된 캠페인은 그대로 둔다.
  }, [campaignId, campaignSmtpProfileId, defaultSmtpProfileId, queryClient]);
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
    // Saving a draft is what moves a card to Draft; the server rejects a stage
    // change to Draft/Review while the card has no draft.
    if ((stage === "draft" || stage === "review") && !target.current_draft) {
      setSelectedTargetId(target.id);
      setQueueNotice(t("draftRequiredFirst"));
      return;
    }
    // Stage edits may be superseded while saving; delivery operations remain locked.
    if (pendingTargetIdsRef.current.has(target.id) && !stageIntents.current.has(target.id)) return;
    stageIntents.current.set(target.id, stage);
    setOptimisticStages(current => ({...current, [target.id]: stage}));
    if (!beginTargetAction(target.id)) return;
    const queryKey = ["sales-campaigns", "detail", campaignId];
    let scope: string | undefined;
    try {
      // An already-running poll must not overwrite the PATCH's newer target.
      await queryClient.cancelQueries({queryKey});
      let confirmed = queryClient.getQueryData<SalesCampaignResponse>(queryKey)?.targets.find(item => item.id === target.id) ?? target;
      while (stageIntents.current.has(target.id)) {
        const destination = stageIntents.current.get(target.id)!;
        if (confirmed.stage === destination) {
          stageIntents.current.delete(target.id);
          break;
        }
        scope = `stage:${campaignId}:${target.id}:${confirmed.version}:${destination}`;
        const response = await updateSalesCampaignTarget(
          campaignId,
          target.id,
          {stage: destination as SalesCampaignWorkflowStage, expected_target_version: confirmed.version},
          {idempotencyKey: getOperationKey(scope)}
        );
        settleOperation(scope);
        scope = undefined;
        confirmed = response.data;
        queryClient.setQueryData<SalesCampaignResponse>(queryKey, current => current ? {
          ...current,
          targets: current.targets.map(item => item.id === target.id ? confirmed : item),
        } : current);
        if (stageIntents.current.get(target.id) === destination) stageIntents.current.delete(target.id);
      }
      setMoveAnnouncement(t("moved", {company: target.company_name ?? "", stage: t(`columns.${confirmed.stage}` as never)}));
    } catch (error) {
      if (scope) settleOperation(scope, error);
      stageIntents.current.delete(target.id);
      if (
        error instanceof ApiError &&
        error.problem.violations.some(violation => violation.code === "DRAFT_REQUIRED")
      ) {
        setSelectedTargetId(target.id);
        setQueueNotice(t("draftRequiredFirst"));
      } else {
        setActionError(t("moveError"));
      }
      await refresh();
    } finally {
      stageIntents.current.delete(target.id);
      setOptimisticStages(current => {
        const next = {...current};
        delete next[target.id];
        return next;
      });
      endTargetAction(target.id);
    }
  }

  /**
   * 카드를 "발송 대기"로 보낸다. 별도의 동결·승인 단계 없이, 초안이 아직 확정되지 않았다면
   * 캠페인 발신 채널로 자동 확정한 뒤 바로 예약한다.
   */
  async function queueTarget(target: SalesCampaignTarget) {
    if (!campaign) return;
    if (!target.current_draft) {
      setSelectedTargetId(target.id);
      setQueueNotice(t("draftRequiredFirst"));
      return;
    }
    if (!beginTargetAction(target.id)) return;
    setActionError(null);
    setQueueNotice(null);
    let scope: string | undefined;
    try {
      let current = target;
      if (target.current_draft.status !== "frozen" || !target.outbound) {
        const draftId = target.current_draft.draft_id;
        const draft = await getMessageDraft(draftId);
        scope = `freeze:${campaignId}:${draftId}:${draft.data.version}`;
        await freezeMessageDraft(
          draftId,
          {
            delivery_channel_id: campaign.delivery_channel_id,
            tracking_requested: false,
            ...(campaign.meeting_type_id ? {meeting_type_id: campaign.meeting_type_id} : {}),
          },
          {
            etag: readEtag(draft) ?? draft.data.version,
            idempotencyKey: getOperationKey(scope),
          }
        );
        settleOperation(scope);
        scope = undefined;
        const fresh = (await getSalesCampaign(campaignId)).data;
        queryClient.setQueryData<SalesCampaignResponse>(
          ["sales-campaigns", "detail", campaignId],
          fresh
        );
        current = fresh.targets.find(item => item.id === target.id) ?? target;
      }
      const outbound = current.outbound;
      if (!outbound) throw new Error("outbound snapshot missing");
      scope = `queue:${campaignId}:${current.id}:${current.version}:${outbound.payload_hash}`;
      const response = await queueSalesCampaignTarget(
        campaignId,
        current.id,
        {
          outbound_message_id: outbound.outbound_message_id,
          expected_payload_hash: outbound.payload_hash,
          expected_target_version: current.version,
        },
        { idempotencyKey: getOperationKey(scope) }
      );
      settleOperation(scope);
      scope = undefined;
      setBatchTimes((previous) => ({
        ...previous,
        [target.id]: response.data.batch_at,
      }));
      await queryClient.invalidateQueries({
        queryKey: ["sales-campaigns", "detail", campaignId],
      });
    } catch (error) {
      if (scope) settleOperation(scope, error);
      setActionError(t("queueError"));
      await refresh();
    } finally {
      endTargetAction(target.id);
    }
  }

  const boardStageAtPointer = useCallback((clientX: number, clientY: number): BoardStage | null => {
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
  }, [campaignId]);

  const clearPointerDrag = useCallback((pointerId?: number) => {
    const active = activePointerDragRef.current;
    if (!active || (pointerId !== undefined && pointerId !== active.pointerId)) return;

    if (active.activated) suppressClickUntil.current = Date.now() + 350;
    activePointerDragRef.current = null;
    setDraggingTargetId(null);
    setDragVisual(null);
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
  }, []);

  function handlePointerDown(
    event: React.PointerEvent<HTMLElement>,
    target: SalesCampaignTarget
  ) {
    const stage = isBoardStage(target.stage) ? target.stage : "attention";
    if (
      activePointerDragRef.current ||
      event.button !== 0 ||
      !(BOARD_STAGES.slice(0, 3) as readonly string[]).includes(stage) ||
      (pendingTargetIdsRef.current.has(target.id) && !stageIntents.current.has(target.id))
    ) {
      return;
    }

    event.preventDefault();
    const captureTarget = event.currentTarget;
    const bounds = captureTarget.closest("article")!.getBoundingClientRect();
    activePointerDragRef.current = {
      pointerId: event.pointerId,
      targetId: target.id,
      startX: event.clientX,
      startY: event.clientY,
      activated: false,
      captureTarget,
      clientX: event.clientX,
      clientY: event.clientY,
      offsetX: event.clientX - bounds.left,
      offsetY: event.clientY - bounds.top,
      width: bounds.width,
      height: bounds.height,
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
    active.clientX = event.clientX;
    active.clientY = event.clientY;
    const deltaX = event.clientX - active.startX;
    const deltaY = event.clientY - active.startY;
    if (!active.activated) {
      if (deltaX * deltaX + deltaY * deltaY < POINTER_DRAG_THRESHOLD_PX ** 2) return;
      active.activated = true;
      setDragVisual({width: active.width, height: active.height, x: event.clientX - active.offsetX, y: event.clientY - active.offsetY});
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
      setDragVisual({width: active.width, height: active.height, x: event.clientX - active.offsetX, y: event.clientY - active.offsetY});
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
      (stageIntents.current.get(dragged.id) ?? dragged.stage) === destination ||
      (pendingTargetIdsRef.current.has(dragged.id) && !stageIntents.current.has(dragged.id))
    ) {
      return;
    }
    void moveTarget(dragged, destination);
  }

  function handlePointerCancel(event: React.PointerEvent) {
    clearPointerDrag(event.pointerId);
  }

  useEffect(() => {
    const cancel = () => clearPointerDrag();
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && activePointerDragRef.current) {
        event.preventDefault();
        clearPointerDrag();
      }
    };
    window.addEventListener("keydown", escape);
    window.addEventListener("blur", cancel);
    document.addEventListener("visibilitychange", cancel);
    return () => {
      window.removeEventListener("keydown", escape);
      window.removeEventListener("blur", cancel);
      document.removeEventListener("visibilitychange", cancel);
      const active = activePointerDragRef.current;
      activePointerDragRef.current = null;
      if (active?.captureTarget.hasPointerCapture?.(active.pointerId)) {
        active.captureTarget.releasePointerCapture(active.pointerId);
      }
    };
  }, [clearPointerDrag]);

  useEffect(() => {
    if (!draggingTargetId) return;
    let frame = 0;
    let previousTime = 0;
    const previousCursor = document.body.style.cursor;
    const previousSelect = document.body.style.userSelect;
    document.body.style.cursor = "grabbing";
    document.body.style.userSelect = "none";
    const tick = (time: number) => {
      const active = activePointerDragRef.current;
      const board = boardRef.current;
      if (!active || !board) return;
      if (previewRef.current) {
        previewRef.current.style.transform = `translate3d(${active.clientX - active.offsetX}px, ${active.clientY - active.offsetY}px, 0)`;
      }
      const bounds = board.getBoundingClientRect();
      const step = Math.min((time - previousTime) / 16.67 || 1, 2);
      previousTime = time;
      const edgeSpeed = (point: number, start: number, end: number) => {
        const edge = Math.min(64, (end - start) / 3);
        if (edge <= 0) return 0;
        if (point < start + edge) return -Math.min(1, (start + edge - point) / edge) * 14 * step;
        if (point > end - edge) return Math.min(1, (point - end + edge) / edge) * 14 * step;
        return 0;
      };
      if (active.clientX >= bounds.left - 24 && active.clientX <= bounds.right + 24 &&
          active.clientY >= bounds.top - 24 && active.clientY <= bounds.bottom + 24) {
        board.scrollLeft += edgeSpeed(active.clientX, bounds.left, bounds.right);
        board.scrollTop += edgeSpeed(active.clientY, bounds.top, bounds.bottom);
      }
      setPointerDropStage(boardStageAtPointer(active.clientX, active.clientY));
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => {
      cancelAnimationFrame(frame);
      document.body.style.cursor = previousCursor;
      document.body.style.userSelect = previousSelect;
    };
  }, [draggingTargetId, boardStageAtPointer]);

  const draggedTarget = targets.find(target => target.id === draggingTargetId);
  const draggedStage = draggedTarget ? optimisticStages[draggedTarget.id] ?? draggedTarget.stage : null;

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
        ref={boardRef}
        onClickCapture={event => {
          if (Date.now() < suppressClickUntil.current) {
            event.preventDefault();
            event.stopPropagation();
          }
        }}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onPointerCancel={handlePointerCancel}
        onLostPointerCapture={handlePointerCancel}
        className="min-h-80 min-w-0 max-w-full max-h-[calc(100dvh-19rem)] overflow-auto overscroll-contain rounded-xl pb-4 outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <div className="grid min-w-[1680px] grid-cols-6 items-stretch gap-4">
        {BOARD_STAGES.map((stage) => {
          const laneTargets = targets.filter((target) => {
            const effectiveStage = optimisticStages[target.id] ?? (isBoardStage(target.stage) ? target.stage : "attention");
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
              className={`flex min-h-80 min-w-0 flex-col gap-3 rounded-xl border border-transparent bg-muted/30 p-2 transition-colors motion-reduce:transition-none ${pointerDropStage === stage && draggedStage !== stage ? "bg-accent/60 ring-2 ring-primary/30 ring-inset" : ""}`}
            >
              <div className="sticky top-0 z-10 flex items-center justify-between gap-2 rounded-lg bg-background/95 px-2 py-2">
                <h2 className="flex items-center gap-2 text-sm font-medium">
                  <span aria-hidden className={`size-2 rounded-full ${stage === "accepted" ? "bg-emerald-500" : stage === "attention" ? "bg-amber-500" : stage === "review" ? "bg-violet-400" : stage === "draft" ? "bg-blue-400" : "bg-muted-foreground/40"}`} />
                  {stageLabel}
                  {(stage === "accepted" || stage === "attention") ? <LockKeyhole aria-hidden className="size-3 text-muted-foreground" /> : null}
                </h2>
                <Badge variant="secondary">{laneTargets.length}</Badge>
              </div>
              <div className="flex min-h-56 flex-1 flex-col gap-2">
                {draggedTarget && pointerDropStage === stage && draggedStage !== stage ? (
                  <div data-testid="board-drop-placeholder" aria-hidden
                    style={{minHeight: dragVisual?.height ?? 120}}
                    className="flex items-center justify-center rounded-lg border-2 border-dashed border-primary/30 bg-primary/5 px-4 text-center text-xs text-muted-foreground">
                    {t(stage === "queued" ? "dropToQueue" : "dropHere", {stage: stageLabel})}
                  </div>
                ) : null}
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
                {laneTargets.length === 0 && !(draggedTarget && pointerDropStage === stage) ? (
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

      <p role="status" aria-live="polite" className="sr-only">{moveAnnouncement}</p>
      {draggedTarget && dragVisual && createPortal(
        <div ref={previewRef} data-testid="board-drag-preview" aria-hidden
          style={{
            position: "fixed", top: 0, left: 0, width: dragVisual.width,
            transform: `translate3d(${dragVisual.x}px, ${dragVisual.y}px, 0)`,
            zIndex: 100, pointerEvents: "none",
          }}>
          <div className="rounded-xl border border-primary/20 bg-card p-4 shadow-2xl ring-1 ring-black/5">
            <p className="line-clamp-2 text-sm font-semibold">{draggedTarget.company_name}</p>
            <p className="mt-1 truncate text-xs text-muted-foreground">{draggedTarget.contact_name}</p>
            <p className="mt-3 line-clamp-3 text-sm">{draggedTarget.current_draft?.subject}</p>
            <p className="mt-4 text-xs text-muted-foreground">{pointerDropStage ? t("dropHere", {stage: t(`columns.${pointerDropStage}` as never)}) : t("dragCancel")}</p>
          </div>
        </div>, document.body
      )}

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
          onQueue={() => queueTarget(selectedTarget)}
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
      className={`group flex min-w-0 flex-col gap-3 rounded-lg border border-border/70 bg-card p-3 shadow-xs transition-[box-shadow,opacity] motion-reduce:transition-none hover:border-border hover:shadow-md ${isDragging ? "opacity-50" : ""}`}
    >
      <div className="flex min-w-0 items-start gap-1">
      <button
        type="button"
        onClick={onOpen}
        onPointerDown={event => {
          if (isEditable && event.pointerType === "mouse") onPointerDown(event);
        }}
        aria-label={t("openTarget", { company })}
        className="min-w-0 flex-1 cursor-grab text-left outline-none active:cursor-grabbing focus-visible:rounded-lg focus-visible:ring-[3px] focus-visible:ring-ring/50"
      >
        <span className="block line-clamp-2 text-sm font-semibold leading-5">{company}</span>
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
      <div className="flex items-center justify-between gap-2">
        {outboundStatus ? <StatusBadge status={outboundStatus} /> : <span />}
        {busy ? <Loader2 aria-label={t("saving")} className="size-3.5 animate-spin text-muted-foreground motion-reduce:animate-none" /> : null}
      </div>
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
