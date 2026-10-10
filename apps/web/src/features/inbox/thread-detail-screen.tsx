"use client";

/**
 * 대화 상세 화면 (UI05, §9.3).
 * - 메시지 본문은 서버 정제 HTML 만 sandboxed iframe 으로 렌더한다.
 * - 20초 폴링으로 새 답장·다른 구성원 회신을 감지해 "새 내용 보기" 프롬프트를
 *   띄운다 — baseline 이후 도착분은 확인 전까지 숨긴다 (collision detection).
 * - 배정·상태 변경은 If-Match; 412 는 ConflictDiff 로 비교·재적용한다.
 * - 회신은 초안 작성 후 동결·검토·승인 또는 확인된 제출 흐름을 따른다.
 *   accepted ≠ delivered.
 */
import {
  ArrowLeft,
  Ban,
  CheckCircle2,
  PanelRight,
  PauseCircle,
} from "lucide-react";
import Link from "next/link";
import { useTranslations } from "next-intl";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";

import { ConflictDiff, type ConflictChoice } from "@/components/patterns/conflict-diff";
import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button, buttonVariants } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { getMe } from "@/lib/api/auth";
import { idempotencyKey } from "@/lib/api/client";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import {
  patchThreadStatus,
  putAssignment,
  putReadState,
  useEmailThread,
  useMailAccounts,
  usePrincipals,
  type EmailMessage,
  type MailAccount,
  type ThreadStatus,
} from "@/lib/api/inbox";
import { ApiError } from "@/lib/api/problem";
import { readEtag } from "@/lib/api/mutate";
import { cn } from "@/lib/utils";
import { AssignMenu } from "./assign-menu";
import { ClassificationReviewDialog } from "./classification-review-dialog";
import { CrmPanel } from "./crm-panel";
import { MessageCard } from "./message-card";
import { ReconnectBanner } from "./reconnect-banner";
import { ReplyComposer } from "./reply-composer";
import {
  canAction,
  canComposeInThread,
  computeBaseline,
  detectCollision,
  inboxErrorKey,
  inferredReplyTo,
  needsReconnect,
  nextPendingStatus,
  nextResolveStatus,
  replyBlockReason,
  suppressionCandidates,
  suppressionHref,
  visibleMessages,
  type CollisionKind,
  type ThreadBaseline,
} from "./thread-state";

/** 폴링 주기 — 충돌 감지·동기화 지연 표시용 (UI05 "다른 직원 회신·새 답장 감지"). */
const POLL_MS = 20_000;

const COLLISION_KEY: Record<CollisionKind, string> = {
  new_inbound: "collision.newInbound",
  teammate_reply: "collision.teammateReply",
  changed: "collision.changed",
};

export function ThreadDetailScreen({ threadId }: { threadId: string }) {
  const t = useTranslations("inbox");
  const tc = useTranslations("common");
  const ts = useTranslations("status");
  const queryClient = useQueryClient();

  const detailQuery = useEmailThread(threadId, { refetchIntervalMs: POLL_MS });
  const accountsQuery = useMailAccounts();
  const principalsQuery = usePrincipals();

  const detail = detailQuery.data?.data ?? null;

  // ── 최신 ETag — If-Match 에 쓴다 (GET 응답 ETag 또는 version 필드). ──
  const etagRef = useRef<string | number | undefined>(undefined);
  useEffect(() => {
    if (detailQuery.data) {
      etagRef.current =
        readEtag(detailQuery.data) ?? detailQuery.data.data.thread.version;
    }
  }, [detailQuery.data]);

  // ── baseline: 사용자가 마지막으로 확인한 스냅샷 → 충돌 감지 기준. ──
  const [baseline, setBaseline] = useState<ThreadBaseline | null>(null);
  useEffect(() => {
    if (detail && baseline === null) {
      setBaseline(computeBaseline(detail));
    }
  }, [detail, baseline]);

  const collision = useMemo(
    () => (baseline && detail ? detectCollision(baseline, detail) : null),
    [baseline, detail]
  );

  const acknowledgeCollision = useCallback(() => {
    if (detail) setBaseline(computeBaseline(detail));
  }, [detail]);

  // ── 본인 읽음 위치 갱신 — 확인한 범위까지 단조 증가 PUT (If-Match 없음). ──
  const lastReadMarkedRef = useRef<string | null>(null);
  useEffect(() => {
    if (!detail) return;
    const messages = visibleMessages(detail.messages, baseline);
    const last = messages.at(-1);
    if (!last || lastReadMarkedRef.current === last.id) return;
    lastReadMarkedRef.current = last.id;
    // 읽음 갱신은 최선 노력 — 실패해도 다음 확인 시 다시 시도한다.
    putReadState(threadId, { last_read_message_id: last.id }).catch(
      () => undefined
    );
  }, [detail, baseline, threadId]);

  // ── 메일함 재연결 — 스레드의 메일함 상태를 목록/상세 어디서든 찾는다. ──
  const mailboxNeedsReconnect = useMemo(() => {
    if (detail?.mail_account && needsReconnect(detail.mail_account)) {
      return true;
    }
    const account = (accountsQuery.data?.data ?? []).find(
      (a) => a.id === detail?.thread.mail_account_id
    );
    return needsReconnect(account);
  }, [detail, accountsQuery.data]);

  // ── 내 principal — "나에게 배정"용. ──
  const meQuery = useQuery({
    queryKey: ["inbox", "me"],
    staleTime: 60_000,
    queryFn: () => getMe(),
  });
  const meId = meQuery.data?.data.principal_id;

  // ── 변경 뮤테이션 — 논리 작업별 멱등 키 + If-Match. ──
  const assignKeyRef = useRef<string | undefined>(undefined);
  const statusKeyRef = useRef<string | undefined>(undefined);
  const [mutating, setMutating] = useState(false);
  const [conflict, setConflict] = useState<{
    base: string;
    server: string;
    mine: string;
    mineValue: ThreadStatus;
  } | null>(null);
  const [conflictChoice, setConflictChoice] = useState<ConflictChoice>("server");

  const showError = useCallback(
    (error: unknown) => {
      if (error instanceof ApiError) {
        const key = inboxErrorKey(error.problem);
        toast.error(
          key ? t(`errors.${key}`) : (error.problem.detail ?? error.message)
        );
        return error.problem.retryable !== true && error.status < 500;
      }
      toast.error(t("errors.title"));
      return false;
    },
    [t]
  );

  async function refresh() {
    const res = await detailQuery.refetch();
    if (res.data) setBaseline(computeBaseline(res.data.data));
    // 목록 쿼리도 무효화 — 배정·상태 변경이 돌아간 목록에 반영되게 한다.
    await queryClient.invalidateQueries({ queryKey: ["inbox", "threads"] });
  }

  async function applyStatus(next: ThreadStatus) {
    if (!detail) return;
    statusKeyRef.current ??= idempotencyKey();
    setMutating(true);
    try {
      await patchThreadStatus(
        threadId,
        { status: next },
        { etag: etagRef.current, idempotencyKey: statusKeyRef.current }
      );
      statusKeyRef.current = undefined;
      toast.success(t("detail.statusSuccess"));
      await refresh();
      setConflict(null);
    } catch (error) {
      if (error instanceof ApiError && error.status === 412) {
        // 412 — 최신 서버 값과 비교해 사용자가 선택하게 한다 (§7.2).
        const fresh = await detailQuery.refetch();
        const server = fresh.data?.data.thread.status ?? detail.thread.status;
        setConflictChoice("server");
        setConflict({
          base: detail.thread.status,
          server,
          mine: next,
          mineValue: next,
        });
      } else {
        const definitive = showError(error);
        if (definitive) statusKeyRef.current = undefined;
      }
    } finally {
      setMutating(false);
    }
  }

  async function applyAssignment(principalId: string | null) {
    if (!detail) return;
    assignKeyRef.current ??= idempotencyKey();
    setMutating(true);
    try {
      await putAssignment(
        threadId,
        { assigned_principal_id: principalId },
        { etag: etagRef.current, idempotencyKey: assignKeyRef.current }
      );
      assignKeyRef.current = undefined;
      toast.success(t("detail.assignSuccess"));
      await refresh();
    } catch (error) {
      if (error instanceof ApiError && (error.status === 412 || error.status === 428)) {
        // 배정 충돌 — 최신 상태를 다시 보여준다 (ConflictDiff 는 상태 패치에 집중).
        await refresh();
        toast.error(t("errors.versionMismatch"));
      } else {
        const definitive = showError(error);
        if (definitive) assignKeyRef.current = undefined;
      }
    } finally {
      setMutating(false);
    }
  }

  // ── 분류 검토 대화상자. ──
  const [reviewMessage, setReviewMessage] = useState<EmailMessage | null>(null);

  const allowed = detail?.allowed_actions;
  const mailboxAllowedActions = (accountsQuery.data?.data ?? []).find(
    (account) => account.id === detail?.thread.mail_account_id
  )?.allowed_actions;
  const messages = useMemo(
    () => (detail ? visibleMessages(detail.messages, baseline) : []),
    [detail, baseline]
  );
  const hiddenCount =
    detail && baseline
      ? (detail.messages ?? []).length - messages.length
      : 0;
  const replyTo = useMemo(() => inferredReplyTo(messages), [messages]);
  const suppressionTargets = useMemo(
    () => (detail ? suppressionCandidates(detail.messages) : []),
    [detail]
  );

  const canAssign = canAction(allowed, "assign");
  const canUpdateStatus = canAction(allowed, "update_status");
  const canCompose = canComposeInThread(allowed, mailboxAllowedActions);
  const canReview = canAction(allowed, "review_classification");

  if (detailQuery.isPending) {
    return (
      <div className="mx-auto flex w-full max-w-6xl flex-col gap-4 px-4 py-6 sm:px-6">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-24 w-full rounded-2xl" />
        <Skeleton className="h-24 w-full rounded-2xl" />
        <Skeleton className="h-24 w-full rounded-2xl" />
      </div>
    );
  }

  if (detailQuery.error || !detail) {
    return (
      <div className="mx-auto flex w-full max-w-6xl flex-col gap-4 px-4 py-6 sm:px-6">
        <BackLink />
        <ErrorState
          error={detailQuery.error}
          onRetry={() => detailQuery.refetch()}
        />
      </div>
    );
  }

  const thread = detail.thread;
  const principals = principalsQuery.data?.data ?? [];

  // 배너에 표시할 재연결 대상 메일함 — 목록 결과 우선, 없으면 상세 응답의 상태로.
  const reconnectBannerAccounts: MailAccount[] = (() => {
    const account = (accountsQuery.data?.data ?? []).find(
      (a) => a.id === thread.mail_account_id
    );
    if (account && needsReconnect(account)) return [account];
    if (detail.mail_account && needsReconnect(detail.mail_account)) {
      return [
        {
          id: detail.mail_account.id,
          name: detail.mail_account.name ?? detail.mail_account.primary_address ?? "",
          primary_address: detail.mail_account.primary_address ?? "",
          status: detail.mail_account.status ?? "reauth_required",
        },
      ];
    }
    return [];
  })();

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-4 px-4 py-6 sm:px-6">
      <BackLink />

      <ReconnectBanner accounts={reconnectBannerAccounts} blocking />

      {/* 충돌 감지 — 새 답장/다른 구성원 회신/상태 변경 시 재확인 요구. */}
      {collision ? (
        <Alert variant="warning">
          <AlertTitle>{t("errors.title")}</AlertTitle>
          <AlertDescription>
            <p>{t(COLLISION_KEY[collision])}</p>
            {hiddenCount > 0 ? (
              <p className="text-xs">
                {t("collision.hiddenMessages", { count: hiddenCount })}
              </p>
            ) : null}
            <Button
              type="button"
              variant="outline"
              size="xs"
              onClick={acknowledgeCollision}
            >
              {t("collision.showNew")}
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}

      {/* 헤더 — 제목·상태·담당자·행동 */}
      <header className="flex flex-wrap items-center gap-2">
        <h1 className="min-w-0 flex-1 text-xl font-semibold leading-tight">
          {thread.subject ?? t("list.noSubject")}
        </h1>
        <StatusBadge status={thread.status} />
        <span className="text-xs text-muted-foreground">
          {t("detail.assignee")}:{" "}
          {thread.assigned_principal_name?.trim() ||
            (thread.assigned_principal_id
              ? (principals.find((principal) => principal.id === thread.assigned_principal_id)?.name?.trim() ||
                principals.find((principal) => principal.id === thread.assigned_principal_id)?.display_name?.trim() ||
                tc("nameUnavailable"))
              : t("list.unassigned"))}
        </span>
      </header>

      <div className="flex flex-wrap items-center gap-2">
        <AssignMenu
          principals={principals}
          meId={meId}
          currentAssigneeId={thread.assigned_principal_id}
          disabled={!canAssign}
          pending={mutating}
          onAssign={applyAssignment}
        />
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={!canUpdateStatus || mutating}
          onClick={() => applyStatus(nextPendingStatus(thread.status))}
        >
          <PauseCircle aria-hidden />
          {thread.status === "pending"
            ? t("detail.pendingOff")
            : t("detail.pending")}
        </Button>
        <Button
          type="button"
          variant={thread.status === "resolved" ? "outline" : "default"}
          size="sm"
          disabled={!canUpdateStatus || mutating}
          onClick={() => applyStatus(nextResolveStatus(thread.status))}
        >
          <CheckCircle2 aria-hidden />
          {thread.status === "resolved"
            ? t("detail.reopen")
            : t("detail.resolve")}
        </Button>

        {suppressionTargets.length > 0 ? (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button type="button" variant="outline" size="sm">
                <Ban aria-hidden />
                {t("detail.suppress")}
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuLabel>{t("detail.suppress")}</DropdownMenuLabel>
              {suppressionTargets.map((address) => (
                <DropdownMenuItem key={address} asChild>
                  <Link href={suppressionHref(address)}>{address}</Link>
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
        ) : null}

        {/* 모바일 CRM 패널 — 시트로 연다. */}
        <Sheet>
          <SheetTrigger asChild>
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="ml-auto xl:hidden"
            >
              <PanelRight aria-hidden />
              {t("detail.crmOpen")}
            </Button>
          </SheetTrigger>
          <SheetContent side="right" className="w-80 overflow-y-auto">
            <SheetHeader>
              <SheetTitle>{t("detail.crm")}</SheetTitle>
            </SheetHeader>
            <CrmPanel crm={detail.crm} className="border-0 p-0" />
          </SheetContent>
        </Sheet>
      </div>

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_20rem]">
        <div className="flex min-w-0 flex-col gap-3">
          {messages.length === 0 ? (
            <EmptyState
              title={t("empty.title")}
              description={t("empty.description")}
            />
          ) : (
            <ol className="flex flex-col gap-3">
              {messages.map((message) => (
                <li key={message.id}>
                  <MessageCard
                    message={message}
                    canReviewClassification={canReview}
                    onReview={setReviewMessage}
                  />
                </li>
              ))}
            </ol>
          )}

          <ReplyComposer
            threadId={threadId}
            mailAccountId={thread.mail_account_id}
            expectedLastInboundId={thread.last_inbound_message_id}
            inferredTo={replyTo?.address ?? null}
            blockReason={replyBlockReason({
              mailboxNeedsReconnect,
              collision,
              canCompose,
              hasInbound: messages.some((m) => m.direction === "inbound"),
            })}
            onAcknowledgeCollision={acknowledgeCollision}
            onCreated={() => detailQuery.refetch()}
            onSendingChanged={() => detailQuery.refetch()}
            onReplyError={(error) => {
              // 작성 중 새 답장 도착 — 최신 내용 확인을 요구한다 (§9.3).
              if (error.code === "REPLY_CONTEXT_CHANGED") {
                detailQuery.refetch();
              }
              if (error.code === "REPLY_ALREADY_SUBMITTED") {
                void detailQuery.refetch();
              }
            }}
          />
        </div>

        <CrmPanel crm={detail.crm} className="hidden self-start xl:block" />
      </div>

      <ClassificationReviewDialog
        message={reviewMessage}
        onOpenChange={(open) => {
          if (!open) setReviewMessage(null);
        }}
        onReviewed={() => detailQuery.refetch()}
      />

      {/* 412 — base/서버/내 변경 3-way 비교 (§7.2). */}
      <Dialog
        open={conflict !== null}
        onOpenChange={(open) => {
          if (!open) setConflict(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("conflict.title")}</DialogTitle>
            <DialogDescription>
              {tc("conflict.description")}
            </DialogDescription>
          </DialogHeader>
          {conflict ? (
            <ConflictDiff
              fields={[
                {
                  key: "status",
                  label: t("conflict.statusField"),
                  base: ts(conflict.base),
                  server: ts(conflict.server),
                  mine: ts(conflict.mine),
                },
              ]}
              choices={{ status: conflictChoice }}
              onChoiceChange={(_, choice) => setConflictChoice(choice)}
            />
          ) : null}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => setConflict(null)}
            >
              {t("conflict.useServer")}
            </Button>
            <Button
              type="button"
              disabled={conflictChoice !== "mine" || mutating}
              onClick={() => conflict && applyStatus(conflict.mineValue)}
            >
              {t("conflict.applyMine")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );

  function BackLink() {
    return (
      <Link
        href="/app/inbox"
        className={cn(
          buttonVariants({ variant: "ghost", size: "xs" }),
          "self-start text-muted-foreground"
        )}
      >
        <ArrowLeft aria-hidden />
        {t("detail.backToList")}
      </Link>
    );
  }
}
