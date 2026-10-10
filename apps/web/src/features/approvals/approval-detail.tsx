"use client";

import { useLocale, useTranslations } from "next-intl";
import Link from "next/link";
import { useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeftIcon,
  ShieldAlertIcon,
  TriangleAlertIcon,
} from "lucide-react";
import { toast } from "sonner";

import { ConfirmIrreversible } from "@/components/patterns/confirm-irreversible";
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
import { getMe } from "@/lib/api/auth";
import {
  getApproval,
  postApprovalDecision,
  revokeApproval,
  type ApprovalPolicyResult,
  type ApprovalRequestDetail,
  type ApprovalSnapshotMessage,
} from "@/lib/api/approvals";
import { readEtag } from "@/lib/api/mutate";
import { usePrincipals } from "@/lib/api/settings";
import { ApiError } from "@/lib/api/problem";

import { useApiErrorMessage } from "./api-error-message";
import { formatDateTime, shortHash } from "./format";
import {
  approvalActions,
  approvalExpired,
  approvalStateKey,
  isSelfRequest,
} from "./rules";
import { useIdempotencyKey } from "./use-idempotency-key";

export function ApprovalDetail({ approvalId }: { approvalId: string }) {
  const t = useTranslations("approvals.detail");
  const tKind = useTranslations("approvals.kind");
  const tc = useTranslations("common");
  const locale = useLocale();
  const principals = usePrincipals();
  const qc = useQueryClient();
  const errorMessage = useApiErrorMessage("approvals");

  const etagRef = useRef<string | undefined>(undefined);

  const detail = useQuery({
    queryKey: ["approvals", "detail", approvalId],
    queryFn: async () => {
      const res = await getApproval(approvalId);
      // If-Match 에 실을 버전 — ETag 헤더를 우선하고 없으면 version 필드.
      etagRef.current = readEtag(res) ?? undefined;
      return res.data;
    },
    retry: false,
  });

  const me = useQuery({
    queryKey: ["me"],
    queryFn: async () => (await getMe()).data,
    retry: false,
    staleTime: 60_000,
  });

  const req = detail.data;

  const etag = () => etagRef.current ?? req?.version;

  const self = isSelfRequest(req, me.data?.principal_id);
  const expired = approvalExpired(req);
  const requiresMailPreview = req?.action_type === "campaign_launch" || req?.action_type === "message_submit";
  const legacyProviderApproval = req?.retired_provider === true;
  const earlyPreview = req?.target?.preview ?? req?.snapshot ?? null;
  const previewMessages = earlyPreview?.messages ?? earlyPreview?.items ?? [];
  const reviewableMailContent = Boolean(
    earlyPreview?.body_text ||
      earlyPreview?.body_html ||
      previewMessages.some((message) => message.body_text || message.body_html)
  );
  const actions = useMemo(() => {
    const result = approvalActions(req, { mePrincipalId: me.data?.principal_id });
    // On direct navigation, hide decisions until the requester identity is known.
    // The server remains the authority, but the UI must not briefly offer self-approval.
    if (legacyProviderApproval) {
      return {
        ...result,
        approve: { visible: false, disabled: true },
        reject: { visible: false, disabled: true },
        revoke: { visible: false, disabled: true },
      };
    }
    if (!me.isSuccess && typeof req?.self_request !== "boolean") {
      return {
        ...result,
        approve: { visible: false, disabled: true },
        reject: { visible: false, disabled: true },
      };
    }
    if (requiresMailPreview && !reviewableMailContent) {
      return {
        ...result,
        approve: { visible: false, disabled: true },
        reject: { visible: false, disabled: true },
      };
    }
    return result;
  }, [req, me.data, me.isSuccess, requiresMailPreview, reviewableMailContent, legacyProviderApproval]);

  const [approveOpen, setApproveOpen] = useState(false);
  const [rejectOpen, setRejectOpen] = useState(false);
  const [revokeOpen, setRevokeOpen] = useState(false);
  const [bannerError, setBannerError] = useState<string | null>(null);
  const [conflict, setConflict] = useState<"version" | "snapshot" | null>(null);

  function refetch() {
    setConflict(null);
    setBannerError(null);
    void detail.refetch();
  }

  function handleMutationError(err: unknown) {
    if (err instanceof ApiError) {
      if (err.code === "VERSION_MISMATCH" || err.status === 412) {
        setConflict("version");
        return;
      }
      if (err.code === "SNAPSHOT_MISMATCH") {
        setConflict("snapshot");
        return;
      }
      if (err.code === "SELF_APPROVAL_FORBIDDEN") {
        setBannerError(t("selfApproval"));
        return;
      }
    }
    setBannerError(errorMessage(err));
  }

  if (detail.isLoading) {
    return (
      <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
        <Skeleton className="h-8 w-56" />
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

  if (!req) {
    return (
      <div className="mx-auto w-full max-w-6xl px-6 py-6">
        <EmptyState
          title={t("notFoundTitle")}
          description={t("notFoundDescription")}
          action={
            <Button asChild variant="outline" size="sm">
              <Link href="/app/approvals">{t("back")}</Link>
            </Button>
          }
        />
      </div>
    );
  }

  const snapshot = req.target?.preview ?? req.snapshot ?? null;
  const messages: ApprovalSnapshotMessage[] =
    snapshot?.messages ?? snapshot?.items ?? [];
  const policies = req.policy_results ?? [];
  const recipientsCount =
    req.recipients_count ?? snapshot?.recipients_count ?? snapshot?.enrollments?.total ?? messages.length;
  const sender = snapshot?.sender_address ?? snapshot?.sender ?? "—";
  const snapshotHash = req.snapshot_hash ?? req.target?.current_hash ?? req.manifest_hash ?? "";
  const stateKey = approvalStateKey(
    req.status === "pending" && expired ? "expired" : req.status
  );
  const requesterName =
    req.requester_display_name?.trim() ||
    req.requester?.display_name?.trim() ||
    req.requester?.name?.trim() ||
    principals.data?.pages
      .flatMap((page) => page.data)
      .find((principal) => principal.id === req.requested_by)?.name?.trim() ||
    principals.data?.pages
      .flatMap((page) => page.data)
      .find((principal) => principal.id === req.requested_by)?.display_name?.trim() ||
    tc("nameUnavailable");
  const targetId =
    req.target_id ??
    req.launch_id ??
    req.outbound_message_id ??
    req.booking_change_id ??
    req.suppression_id;

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <div>
        <Link
          href="/app/approvals"
          className="inline-flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeftIcon className="size-4" aria-hidden />
          {t("back")}
        </Link>
      </div>

      <PageHeader
        title={
          tKind.has(req.action_type) ? tKind(req.action_type) : req.action_type
        }
        actions={
          <div className="flex items-center gap-2">
            <StatusBadge status={expired && req.status === "pending" ? "expired" : req.status} />
            {actions.approve.visible ? (
              <Button
                type="button"
                size="sm"
                disabled={actions.approve.disabled}
                onClick={() => setApproveOpen(true)}
              >
                {t("approve")}
              </Button>
            ) : null}
            {actions.reject.visible ? (
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={actions.reject.disabled}
                onClick={() => setRejectOpen(true)}
              >
                {t("reject")}
              </Button>
            ) : null}
            {actions.revoke.visible ? (
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={actions.revoke.disabled}
                onClick={() => setRevokeOpen(true)}
              >
                {t("revoke")}
              </Button>
            ) : null}
          </div>
        }
      />

      {/* 종결·차단 상태 배너 */}
      {stateKey && req.status !== "pending" ? (
        <Alert variant={req.status === "approved" ? "success" : req.status === "rejected" || req.status === "revoked" ? "warning" : "info"}>
          <AlertDescription>{t(stateKey)}</AlertDescription>
        </Alert>
      ) : null}
      {req.status === "pending" && expired ? (
        <Alert variant="warning">
          <TriangleAlertIcon />
          <AlertDescription>{t("stateExpired")}</AlertDescription>
        </Alert>
      ) : null}
      {self && req.status === "pending" ? (
        <Alert variant="info">
          <ShieldAlertIcon />
          <AlertDescription>{t("selfApproval")}</AlertDescription>
        </Alert>
      ) : null}
      {requiresMailPreview && !reviewableMailContent && req.status === "pending" ? (
        <Alert variant="warning">
          <ShieldAlertIcon />
          <AlertDescription>{t("snapshotUnavailable")}</AlertDescription>
        </Alert>
      ) : null}
      {legacyProviderApproval ? (
        <Alert variant="warning">
          <ShieldAlertIcon />
          <AlertTitle>{t("retiredProviderTitle")}</AlertTitle>
          <AlertDescription>{t("retiredProviderDescription")}</AlertDescription>
        </Alert>
      ) : null}
      {!self && req.can_decide === false && req.status === "pending" ? (
        <Alert variant="warning">
          <ShieldAlertIcon />
          <AlertDescription>{t("noDecidePermission")}</AlertDescription>
        </Alert>
      ) : null}
      {conflict === "version" ? (
        <Alert variant="warning">
          <TriangleAlertIcon />
          <AlertTitle>{t("versionMismatch")}</AlertTitle>
          <AlertDescription>
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
      {conflict === "snapshot" ? (
        <Alert variant="warning">
          <TriangleAlertIcon />
          <AlertTitle>{t("snapshotMismatch")}</AlertTitle>
          <AlertDescription>
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

      {/* 요청 메타 */}
      <section className="rounded-2xl border border-border bg-card px-4 py-4">
        <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm sm:grid-cols-[auto_1fr_auto_1fr]">
          <dt className="text-muted-foreground">{t("requester")}</dt>
          <dd className="font-medium">
            {requesterName}
            {self ? (
              <Badge variant="outline" className="ml-2">
                {t("selfBadge")}
              </Badge>
            ) : null}
          </dd>
          <dt className="text-muted-foreground">{t("kind")}</dt>
          <dd>
            <Badge variant="secondary">
              {tKind.has(req.action_type)
                ? tKind(req.action_type)
                : req.action_type}
            </Badge>
          </dd>
          <dt className="text-muted-foreground">{t("created")}</dt>
          <dd>{formatDateTime(req.created_at, locale)}</dd>
          <dt className="text-muted-foreground">{t("expires")}</dt>
          <dd>{formatDateTime(req.expires_at, locale)}</dd>
          <dt className="text-muted-foreground">{t("snapshotHash")}</dt>
          <dd className="font-mono text-ui" title={snapshotHash}>
            {shortHash(snapshotHash)}
          </dd>
        </dl>
      </section>
      {targetId ? (
        <details className="rounded-2xl border border-border bg-card px-4 py-3">
          <summary className="cursor-pointer text-sm font-medium">{t("technicalDetails")}</summary>
          <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm">
            <dt className="text-muted-foreground">{t("targetId")}</dt>
            <dd className="break-all font-mono text-ui">{targetId}</dd>
          </dl>
        </details>
      ) : null}

      {/* 동결 스냅샷 — 실제 수신자·발신 주소·렌더링된 본문 */}
      <section className="flex flex-col gap-3 rounded-2xl border border-border bg-card px-4 py-4">
        <h2 className="text-sm font-medium">{t("renderedBody")}</h2>
        {snapshot ? (
          <>
            <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm">
              <dt className="text-muted-foreground">{t("recipients")}</dt>
              <dd className="font-medium">
                {t("recipientsCount", { count: recipientsCount })}
              </dd>
              <dt className="text-muted-foreground">{t("sender")}</dt>
              <dd className="font-medium">{sender}</dd>
              {snapshot.purpose ? (
                <>
                  <dt className="text-muted-foreground">{t("purpose")}</dt>
                  <dd className="font-medium">{snapshot.purpose}</dd>
                </>
              ) : null}
            </dl>

            {snapshot.subject ? (
              <div className="grid gap-1">
                <h3 className="text-xs font-medium text-muted-foreground">
                  {t("subject")}
                </h3>
                <p className="text-sm font-medium">{snapshot.subject}</p>
              </div>
            ) : null}
            {snapshot.body_text ? (
              <div className="grid gap-1">
                <h3 className="text-xs font-medium text-muted-foreground">
                  {t("bodyText")}
                </h3>
                <pre className="max-h-64 overflow-auto rounded-lg bg-app-panel-muted p-3 text-sm whitespace-pre-wrap">
                  {snapshot.body_text}
                </pre>
              </div>
            ) : null}
            {snapshot.body_html ? (
              <div className="grid gap-1">
                <h3 className="text-xs font-medium text-muted-foreground">
                  {t("bodyHtml")}
                </h3>
                {/* 서버가 동결한 HTML은 스크립트 없는 샌드박스 프레임에만 표시 */}
                <iframe
                  sandbox=""
                  title={t("bodyHtml")}
                  srcDoc={snapshot.body_html}
                  className="h-48 w-full rounded-lg border border-border bg-card"
                />
                <p className="text-2xs text-muted-foreground">
                  {t("htmlSandboxNote")}
                </p>
              </div>
            ) : null}

            {messages.length > 0 ? (
              <div className="grid gap-2">
                <h3 className="text-xs font-medium text-muted-foreground">
                  {t("messages")}
                </h3>
                <ul className="flex flex-col gap-2">
                  {messages.map((m, i) => (
                    <li
                      key={i}
                      className="rounded-lg border border-border px-3 py-2"
                    >
                      <div className="flex flex-wrap items-center gap-2 text-sm">
                        <span className="font-medium">
                          {t("messageN", { order: m.step_order ?? i + 1 })}
                        </span>
                        <span className="text-muted-foreground">
                          → {m.to ?? t("recipientUnavailable")}
                        </span>
                      </div>
                      {m.subject ? (
                        <p className="mt-1 text-sm font-medium">{m.subject}</p>
                      ) : null}
                      {m.body_text ? (
                        <pre className="mt-1 max-h-32 overflow-auto text-2xs whitespace-pre-wrap text-muted-foreground">
                          {m.body_text}
                        </pre>
                      ) : null}
                    </li>
                  ))}
                </ul>
              </div>
            ) : null}
          </>
        ) : (
          <p className="text-sm text-muted-foreground">{t("noSnapshot")}</p>
        )}
      </section>

      {/* 정책 검사 결과 */}
      <section className="flex flex-col gap-3 rounded-2xl border border-border bg-card px-4 py-4">
        <h2 className="text-sm font-medium">{t("policyResults")}</h2>
        {policies.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            {t("noPolicyResults")}
          </p>
        ) : (
          <ul className="flex flex-col gap-1.5">
            {policies.map((p, i) => (
              <PolicyRow key={p.code ?? i} result={p} />
            ))}
          </ul>
        )}
      </section>

      {/* 결정 기록 */}
      {req.decision ? (
        <section className="rounded-2xl border border-border bg-card px-4 py-4">
          <h2 className="text-sm font-medium">{t("decision")}</h2>
          <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm">
            <dt className="text-muted-foreground">{t("decision")}</dt>
            <dd>
              <StatusBadge status={req.decision.decision} />
            </dd>
            <dt className="text-muted-foreground">{t("decisionAt")}</dt>
            <dd>{formatDateTime(req.decision.decided_at, locale)}</dd>
            {req.decision.reason ? (
              <>
                <dt className="text-muted-foreground">{t("decisionReason")}</dt>
                <dd className="whitespace-pre-wrap">{req.decision.reason}</dd>
              </>
            ) : null}
          </dl>
        </section>
      ) : null}

      <ApproveDialog
        open={approveOpen}
        onOpenChange={setApproveOpen}
        request={req}
        recipientsCount={recipientsCount}
        sender={sender}
        snapshotHash={snapshotHash}
        etag={etag}
        onError={handleMutationError}
        onDone={() => {
          setApproveOpen(false);
          toast.success(t("decisionRecorded"));
          qc.invalidateQueries({ queryKey: ["approvals"] });
          refetch();
        }}
      />
      <RejectDialog
        open={rejectOpen}
        onOpenChange={setRejectOpen}
        request={req}
        snapshotHash={snapshotHash}
        etag={etag}
        onError={handleMutationError}
        onDone={() => {
          setRejectOpen(false);
          toast.success(t("decisionRecorded"));
          qc.invalidateQueries({ queryKey: ["approvals"] });
          refetch();
        }}
      />
      <RevokeDialog
        open={revokeOpen}
        onOpenChange={setRevokeOpen}
        request={req}
        etag={etag}
        onError={handleMutationError}
        onDone={() => {
          setRevokeOpen(false);
          toast.success(t("decisionRecorded"));
          qc.invalidateQueries({ queryKey: ["approvals"] });
          refetch();
        }}
      />
    </div>
  );
}

const POLICY_OUTCOME_VARIANT: Record<
  string,
  "success-subtle" | "amber-subtle" | "rose-subtle"
> = {
  pass: "success-subtle",
  passed: "success-subtle",
  ok: "success-subtle",
  warn: "amber-subtle",
  warning: "amber-subtle",
  fail: "rose-subtle",
  failed: "rose-subtle",
  block: "rose-subtle",
  blocked: "rose-subtle",
};

function PolicyRow({ result }: { result: ApprovalPolicyResult }) {
  const t = useTranslations("approvals.detail");
  const outcome = result.outcome ?? result.status ?? "";
  const variant = POLICY_OUTCOME_VARIANT[outcome] ?? "secondary";
  const outcomeKey = `policyOutcome.${outcome}`;
  return (
    <li className="flex flex-wrap items-center gap-2 rounded-lg border border-border px-3 py-2 text-sm">
      <Badge variant={variant}>
        {outcome ? (t.has(outcomeKey) ? t(outcomeKey) : outcome) : "—"}
      </Badge>
      {result.code ? (
        <code className="font-mono text-ui">{result.code}</code>
      ) : null}
      {result.detail ? (
        <span className="text-muted-foreground">{result.detail}</span>
      ) : null}
    </li>
  );
}

function ApproveDialog({
  open,
  onOpenChange,
  request,
  recipientsCount,
  sender,
  snapshotHash,
  etag,
  onError,
  onDone,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  request: ApprovalRequestDetail;
  recipientsCount: number;
  sender: string;
  snapshotHash: string;
  etag: () => string | number | undefined;
  onError: (err: unknown) => void;
  onDone: () => void;
}) {
  const t = useTranslations("approvals.detail");
  const idem = useIdempotencyKey();
  const [pending, setPending] = useState(false);
  const snapshot = request.target?.preview ?? request.snapshot ?? undefined;
  async function confirm() {
    setPending(true);
    try {
      const res = await postApprovalDecision(
        request.id,
        {
          decision: "approved",
          expected_snapshot_hash: snapshotHash,
        },
        { etag: etag(), idempotencyKey: idem.key() }
      );
      idem.settle();
      // execution_status 는 비동기 실행 — 발송 완료가 아니다(B.3.9).
      if (res.data.execution_status) {
        toast.message(t("executionQueued"));
      }
      onDone();
    } catch (err) {
      idem.settle(err);
      onError(err);
      onOpenChange(false);
    } finally {
      setPending(false);
    }
  }

  return (
    <ConfirmIrreversible
      open={open}
      onOpenChange={onOpenChange}
      title={t("approveTitle")}
      description={t("approveDescription")}
      recipientsCount={recipientsCount}
      sender={sender}
      purpose={snapshot?.purpose ?? request.action_type}
      contentPreview={{
        subject: snapshot?.subject,
        body: snapshot?.body_text,
      }}
      confirmText={t("approveConfirmPhrase")}
      confirmLabel={pending ? t("submitting") : t("submitApprove")}
      destructive={false}
      pending={pending}
      onConfirm={confirm}
    />
  );
}

function RejectDialog({
  open,
  onOpenChange,
  request,
  snapshotHash,
  etag,
  onError,
  onDone,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  request: ApprovalRequestDetail;
  snapshotHash: string;
  etag: () => string | number | undefined;
  onError: (err: unknown) => void;
  onDone: () => void;
}) {
  const t = useTranslations("approvals.detail");
  const tCommon = useTranslations("common");
  const idem = useIdempotencyKey();
  const [reason, setReason] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit() {
    if (!reason.trim()) {
      setError(t("rejectReasonRequired"));
      return;
    }
    setPending(true);
    setError(null);
    try {
      await postApprovalDecision(
        request.id,
        {
          decision: "rejected",
          expected_snapshot_hash: snapshotHash,
          reason: reason.trim(),
        },
        { etag: etag(), idempotencyKey: idem.key() }
      );
      idem.settle();
      setReason("");
      onDone();
    } catch (err) {
      idem.settle(err);
      onError(err);
      onOpenChange(false);
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("rejectTitle")}</DialogTitle>
          <DialogDescription>{t("rejectDescription")}</DialogDescription>
        </DialogHeader>
        <div className="grid gap-1.5">
          <Label htmlFor="reject-reason">{t("rejectReason")}</Label>
          <Textarea
            id="reject-reason"
            rows={4}
            value={reason}
            onChange={(e) => setReason(e.target.value)}
          />
        </div>
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            {tCommon("cancel")}
          </Button>
          <Button
            type="button"
            variant="destructive"
            disabled={pending || !reason.trim()}
            onClick={submit}
          >
            {pending ? t("submitting") : t("submitReject")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function RevokeDialog({
  open,
  onOpenChange,
  request,
  etag,
  onError,
  onDone,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  request: ApprovalRequestDetail;
  etag: () => string | number | undefined;
  onError: (err: unknown) => void;
  onDone: () => void;
}) {
  const t = useTranslations("approvals.detail");
  const tCommon = useTranslations("common");
  const idem = useIdempotencyKey();
  const [reason, setReason] = useState("");
  const [pending, setPending] = useState(false);

  async function submit() {
    setPending(true);
    try {
      await revokeApproval(
        request.id,
        { reason: reason.trim() || undefined },
        { etag: etag(), idempotencyKey: idem.key() }
      );
      idem.settle();
      setReason("");
      onDone();
    } catch (err) {
      idem.settle(err);
      onError(err);
      onOpenChange(false);
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("revokeTitle")}</DialogTitle>
          <DialogDescription>{t("revokeDescription")}</DialogDescription>
        </DialogHeader>
        <div className="grid gap-1.5">
          <Label htmlFor="revoke-reason">{t("revokeReason")}</Label>
          <Textarea
            id="revoke-reason"
            rows={3}
            value={reason}
            onChange={(e) => setReason(e.target.value)}
          />
        </div>
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            {tCommon("cancel")}
          </Button>
          <Button
            type="button"
            variant="destructive"
            disabled={pending}
            onClick={submit}
          >
            {pending ? t("submitting") : t("submitRevoke")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
