"use client";

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { RefreshCw, Send, ShieldCheck, TriangleAlert } from "lucide-react";
import { useTranslations } from "next-intl";
import Link from "next/link";
import { useId, useMemo, useRef, useState } from "react";
import { toast } from "sonner";

import { ConfirmIrreversible } from "@/components/patterns/confirm-irreversible";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { getMe } from "@/lib/api/auth";
import {
  APPROVALS_PATH,
  type ApprovalListItem,
} from "@/lib/api/approvals";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  cancelOutbound,
  freezeMessageDraft,
  type FreezeMessageDraftInput,
  getMessageDraft,
  getOutboundMessage,
  listMessageDrafts,
  preflightOutbound,
  requestOutboundApproval,
  submitOutbound,
  type MessageDraftListItem,
  type OutboundPreflightResponse,
} from "@/lib/api/sending";
import { readEtag } from "@/lib/api/mutate";
import { useMailAccounts } from "@/lib/api/inbox";
import {
  useDeliveryChannels,
  type DeliveryChannel,
} from "@/lib/api/settings";
import { MEETING_TYPES_LIST_LIMIT, MEETING_TYPES_PATH, MEETING_TYPES_QUERY_KEY, PRINCIPALS_PATH, principalDisplayName, type MeetingType, type PrincipalRef } from "@/lib/api/meetings";
import { useCursorList } from "@/lib/api/use-cursor-list";

import {
  canCancelOutbound,
  canRequestOutboundApproval,
  canSubmitOutbound,
  preflightDisposition,
} from "./sending-state";

export interface DraftSendingWorkflowProps {
  /** Existing draft id enables the same flow for Inbox and CRM composition. */
  draftId?: string;
  /** When present, lists this thread's drafts so they can be resumed after reload. */
  threadId?: string;
  /** Sending is always scoped to the mailbox that owns the caller's draft. */
  mailAccountId: string;
  /** Inbox review/reconnect gates must still apply to drafts created earlier. */
  sendBlocked?: boolean;
  /** Prevents freezing a campaign draft while its editor has unsaved changes. */
  freezeBlocked?: boolean;
  onChanged?: () => void | Promise<unknown>;
  /** Campaign drafts must be queued through their board's batch boundary. */
  allowSubmit?: boolean;
  /** Shows the campaign board link for campaign-owned drafts. */
  salesCampaignId?: string;
  /** Pins freeze to the channel already bound to a campaign sender. */
  lockedDeliveryChannelId?: string;
  /** Pins booking links to the campaign's configured meeting type, including none. */
  campaignMeetingTypeId?: string | null;
}

type PendingAction = "freeze" | "approval" | "submit" | "cancel" | "refresh" | null;

interface FrozenOverride {
  draftId: string;
  outboundId: string;
}

interface CreatedApproval {
  outboundId: string;
  approvalId: string;
}

interface DraftSelection {
  propDraftId?: string;
  selectedDraftId: string;
}

interface DraftControls {
  draftId: string;
  selectedChannelId: string;
  meetingTypeId: string;
  approvalReason: string;
  trackingRequested: boolean;
}

export function DraftSendingWorkflow({
  draftId,
  threadId,
  mailAccountId,
  sendBlocked = false,
  freezeBlocked = false,
  allowSubmit = true,
  salesCampaignId,
  lockedDeliveryChannelId,
  campaignMeetingTypeId,
  onChanged,
}: DraftSendingWorkflowProps) {
  const t = useTranslations("sending.workflow");
  const tCommon = useTranslations("common");
  const queryClient = useQueryClient();
  const channelSelectId = useId();
  const meetingTypeSelectId = useId();
  const trackingSelectId = useId();
  const approvalReasonId = useId();

  const [draftSelection, setDraftSelection] = useState<DraftSelection>(() => ({
    propDraftId: draftId,
    selectedDraftId: draftId ?? "",
  }));
  if (draftSelection.propDraftId !== draftId) {
    setDraftSelection({
      propDraftId: draftId,
      selectedDraftId: draftId ?? "",
    });
  }
  const selectedDraftId = draftSelection.selectedDraftId;
  const setSelectedDraftId = (value: string) =>
    setDraftSelection((current) => ({ ...current, selectedDraftId: value }));

  const [draftControls, setDraftControls] = useState<DraftControls>(() => ({
    draftId: selectedDraftId,
    selectedChannelId: lockedDeliveryChannelId ?? "",
    meetingTypeId: campaignMeetingTypeId ?? "",
    approvalReason: "",
    trackingRequested: false,
  }));
  if (draftControls.draftId !== selectedDraftId) {
    setDraftControls({
      draftId: selectedDraftId,
      selectedChannelId: lockedDeliveryChannelId ?? "",
      meetingTypeId: campaignMeetingTypeId ?? "",
      approvalReason: "",
      trackingRequested: false,
    });
  }
  const { selectedChannelId, meetingTypeId, approvalReason, trackingRequested } = draftControls;
  const campaignMeetingTypeLocked = Boolean(campaignMeetingTypeId);
  const effectiveMeetingTypeId = campaignMeetingTypeLocked
    ? campaignMeetingTypeId ?? ""
    : meetingTypeId;
  const setSelectedChannelId = (value: string) =>
    setDraftControls((current) => ({ ...current, selectedChannelId: value }));
  const setMeetingTypeId = (value: string) =>
    setDraftControls((current) => ({ ...current, meetingTypeId: value }));
  const setApprovalReason = (value: string) =>
    setDraftControls((current) => ({ ...current, approvalReason: value }));
  const setTrackingRequested = (value: boolean) =>
    setDraftControls((current) => ({ ...current, trackingRequested: value }));
  const [pending, setPending] = useState<PendingAction>(null);
  const [confirmSubmitOpen, setConfirmSubmitOpen] = useState(false);
  const [confirmCancelOpen, setConfirmCancelOpen] = useState(false);
  const [frozenOverride, setFrozenOverride] = useState<FrozenOverride | null>(null);
  const [createdApproval, setCreatedApproval] = useState<CreatedApproval | null>(null);
  const operationKeys = useRef<Record<string, string>>({});

  const threadDraftsQuery = useQuery({
    queryKey: ["sending", "thread-drafts", threadId, mailAccountId],
    enabled: Boolean(threadId),
    queryFn: () => listMessageDrafts({ threadId, limit: 50 }),
    retry: false,
  });

  const threadDraftRows = useMemo(() => {
    const rows = threadDraftsQuery.data?.data ?? [];
    return rows.filter(
      (draft) =>
        draft.mail_account_id === mailAccountId &&
        (!draft.thread_id || draft.thread_id === threadId)
    );
  }, [threadDraftsQuery.data, mailAccountId, threadId]);

  const draftQuery = useQuery({
    queryKey: ["sending", "draft", selectedDraftId],
    enabled: selectedDraftId.length > 0,
    queryFn: () => getMessageDraft(selectedDraftId),
    retry: false,
  });
  const activeDraft = draftQuery.data?.data;
  const draftMatchesContext = Boolean(
    activeDraft &&
      activeDraft.mail_account_id === mailAccountId &&
      (!threadId || !activeDraft.thread_id || activeDraft.thread_id === threadId)
  );
  const mailboxMismatch = Boolean(
    activeDraft && activeDraft.mail_account_id !== mailAccountId
  );
  const threadMismatch = Boolean(
    activeDraft &&
      threadId &&
      activeDraft.thread_id &&
      activeDraft.thread_id !== threadId
  );

  const outboundId =
    draftMatchesContext && activeDraft
      ? frozenOverride?.draftId === activeDraft.draft_id
        ? frozenOverride.outboundId
        : activeDraft.outbound_message_id ??
          threadDraftRows.find((row) => row.draft_id === selectedDraftId)
            ?.outbound_message_id ??
          undefined
      : undefined;

  const outboundQuery = useQuery({
    queryKey: ["sending", "outbound", outboundId],
    enabled: Boolean(outboundId),
    queryFn: () => getOutboundMessage(outboundId!).then((result) => result.data),
    retry: false,
  });
  const outbound = outboundQuery.data;

  const preflightQuery = useQuery<OutboundPreflightResponse>({
    queryKey: ["sending", "preflight", outboundId],
    enabled: false,
    queryFn: () => preflightOutbound(outboundId!).then((result) => result.data),
    retry: false,
  });
  const preflight = preflightQuery.data;
  const disposition = preflightDisposition(preflight);

  const channelsQuery = useDeliveryChannels();
  const channelRows = useMemo(
    () =>
      (channelsQuery.data?.pages.flatMap((page) => page.data) ?? []).filter(
        (channel) =>
          channel.mail_account_id === mailAccountId &&
          channel.purpose === activeDraft?.purpose &&
          channel.enabled &&
          channel.policy_status === "allowed" &&
          (!lockedDeliveryChannelId || channel.id === lockedDeliveryChannelId)
      ),
    [channelsQuery.data, mailAccountId, activeDraft?.purpose, lockedDeliveryChannelId]
  );

  // 선택 가능한 채널이 하나뿐이면 따로 고르지 않아도 그 채널을 쓴다.
  const effectiveChannelId =
    selectedChannelId || (channelRows.length === 1 ? (channelRows[0]?.id ?? "") : "");

  const meetingTypesQuery = useCursorList<MeetingType>({
    queryKey: MEETING_TYPES_QUERY_KEY,
    path: MEETING_TYPES_PATH,
    limit: MEETING_TYPES_LIST_LIMIT,
    enabled: Boolean(selectedDraftId),
  });
  const meetingTypePrincipals = useCursorList<PrincipalRef>({
    queryKey: ["meetings", "principals"],
    path: PRINCIPALS_PATH,
    limit: MEETING_TYPES_LIST_LIMIT,
    enabled: Boolean(selectedDraftId),
  });
  const meetingTypes = useMemo(
    () =>
      (meetingTypesQuery.data?.pages.flatMap((page) => page.data) ?? []).filter(
        (meetingType) => meetingType.status === "active"
      ),
    [meetingTypesQuery.data]
  );
  const meetingTypeHostNames = useMemo(() => {
    const names = new Map<string, string>();
    for (const person of meetingTypePrincipals.data?.pages.flatMap((page) => page.data) ?? []) {
      const name = principalDisplayName(person);
      if (name) names.set(person.id, name);
    }
    return names;
  }, [meetingTypePrincipals.data]);

  const mailAccountsQuery = useMailAccounts();
  const mailAccount = (mailAccountsQuery.data?.data ?? []).find(
    (account) => account.id === mailAccountId
  );
  const meQuery = useQuery({
    queryKey: ["me"],
    queryFn: async () => (await getMe()).data,
    retry: false,
    staleTime: 60_000,
  });
  const me = meQuery.data;
  const hasSendGrant =
    mailAccount?.allowed_actions?.includes("mail.send") === true ||
    mailAccount?.grant?.can_send === true;
  const hasComposeGrant =
    mailAccount?.allowed_actions?.includes("mail.compose") === true ||
    mailAccount?.grant?.can_compose === true;

  const approvalsQuery = useCursorList<ApprovalListItem>({
    queryKey: ["sending", "approvals", outboundId],
    path: APPROVALS_PATH,
    params: { action_type: "message_submit" },
    limit: 100,
    enabled: Boolean(outboundId),
  });
  const matchingApproval = useMemo(() => {
    const rows = approvalsQuery.data?.pages.flatMap((page) => page.data) ?? [];
    return rows.find(
      (approval) =>
        (approval.outbound_message_id ?? approval.target_id) === outboundId
    );
  }, [approvalsQuery.data, outboundId]);
  const approvalId =
    (createdApproval && createdApproval.outboundId === outboundId
      ? createdApproval.approvalId
      : undefined) ?? matchingApproval?.id;
  const approvalRequestExists = Boolean(approvalId);

  const approvalIsValid = Boolean(
    outbound?.approval_status === "approved" &&
      outbound.approval_valid &&
      outbound.approval_snapshot_matches
  );
  const approvalAllowsSubmit =
    !outbound?.approval_status || approvalIsValid;
  const linkedCampaignId =
    salesCampaignId ?? activeDraft?.sales_campaign_id ?? null;
  const genericLinkedCampaignDraft = Boolean(
    activeDraft?.sales_campaign_id && !salesCampaignId
  );
  const canSubmit =
    canSubmitOutbound(outbound, me?.kind, preflight, hasSendGrant) &&
    !sendBlocked &&
    approvalAllowsSubmit &&
    allowSubmit &&
    !linkedCampaignId;
  const canRequestApproval =
    !sendBlocked &&
    !freezeBlocked &&
    hasComposeGrant &&
    canRequestOutboundApproval(outbound, me?.kind, preflight);
  const canCancel =
    Boolean(outbound && canCancelOutbound(outbound.status)) &&
    (activeDraft?.owner_principal_id === me?.principal_id ||
      hasComposeGrant ||
      me?.role === "admin" ||
      me?.role === "manager");

  const getOperationKey = (scope: string) => {
    operationKeys.current[scope] ??= idempotencyKey();
    return operationKeys.current[scope]!;
  };
  const settleOperationKey = (scope: string) => {
    delete operationKeys.current[scope];
  };

  function reportError(error: unknown) {
    const message =
      error instanceof ApiError
        ? error.problem.detail ?? error.message
        : t("draftError");
    toast.error(message);
    if (error instanceof ApiError && ["VERSION_MISMATCH", "PRECONDITION_REQUIRED"].includes(error.code)) {
      toast.error(t("versionConflict"));
    }
  }

  async function refreshStatus() {
    if (!outboundId) return;
    setPending("refresh");
    try {
      const requests: Promise<unknown>[] = [
        outboundQuery.refetch(),
        approvalsQuery.refetch(),
      ];
      if (outbound?.status === "draft") requests.push(preflightQuery.refetch());
      await Promise.allSettled(requests);
      await onChanged?.();
    } catch (error) {
      reportError(error);
    } finally {
      setPending(null);
    }
  }

  async function freeze() {
    if (
      !activeDraft ||
      !draftMatchesContext ||
      sendBlocked ||
      freezeBlocked ||
      activeDraft.status !== "draft" ||
      !effectiveChannelId
    ) {
      return;
    }
    const scope = `freeze:${activeDraft.draft_id}:${effectiveChannelId}:${effectiveMeetingTypeId}:${trackingRequested}`;
    const request: FreezeMessageDraftInput = {
      delivery_channel_id: effectiveChannelId,
      tracking_requested: trackingRequested,
    };
    if (effectiveMeetingTypeId) request.meeting_type_id = effectiveMeetingTypeId;

    setPending("freeze");
    try {
      const result = await freezeMessageDraft(activeDraft.draft_id, request, {
        etag: readEtag(draftQuery.data!) ?? activeDraft.version,
        idempotencyKey: getOperationKey(scope),
      });
      settleOperationKey(scope);
      setFrozenOverride({
        draftId: activeDraft.draft_id,
        outboundId: result.data.outbound_message_id,
      });
      setSelectedChannelId("");
      // 검토 요청은 한 번에: 승인 요청까지 이어서 보낸다. 실패하면 아래의
      // 승인 요청 영역에서 직접 다시 시도할 수 있다.
      if (hasComposeGrant && me?.kind === "human") {
        try {
          const reason = approvalReason.trim() || t("defaultApprovalReason");
          const approvalScope = `approval:${result.data.outbound_message_id}:${result.data.payload_hash}:${reason}`;
          const approval = await requestOutboundApproval(
            result.data.outbound_message_id,
            { reason, expected_payload_hash: result.data.payload_hash },
            { idempotencyKey: getOperationKey(approvalScope) }
          );
          settleOperationKey(approvalScope);
          setCreatedApproval({
            outboundId: result.data.outbound_message_id,
            approvalId: approval.data.approval_request_id,
          });
        } catch {
          // 승인 요청 영역으로 폴백
        }
      }
      await queryClient.invalidateQueries({
        queryKey: ["sending", "draft", activeDraft.draft_id],
      });
      await queryClient.invalidateQueries({
        queryKey: ["sending", "thread-drafts", threadId, mailAccountId],
      });
      await onChanged?.();
    } catch (error) {
      await Promise.allSettled([
        queryClient.invalidateQueries({
          queryKey: ["sending", "draft", activeDraft.draft_id],
        }),
        threadDraftsQuery.refetch(),
      ]);
      reportError(error);
    } finally {
      setPending(null);
    }
  }

  async function runPreflight() {
    if (!outboundId) return;
    setPending("refresh");
    try {
      await preflightQuery.refetch();
    } catch (error) {
      reportError(error);
    } finally {
      setPending(null);
    }
  }

  async function requestApproval() {
    if (!outbound || !canRequestApproval) return;
    const reason = approvalReason.trim() || t("defaultApprovalReason");
    const scope = `approval:${outbound.outbound_message_id}:${outbound.payload_hash}:${reason}`;
    setPending("approval");
    try {
      const result = await requestOutboundApproval(
        outbound.outbound_message_id,
        {
          reason,
          expected_payload_hash: outbound.payload_hash,
        },
        { idempotencyKey: getOperationKey(scope) }
      );
      settleOperationKey(scope);
      setCreatedApproval({
        outboundId: outbound.outbound_message_id,
        approvalId: result.data.approval_request_id,
      });
      await approvalsQuery.refetch();
    } catch (error) {
      reportError(error);
      await approvalsQuery.refetch().catch(() => undefined);
    } finally {
      setPending(null);
    }
  }

  async function submit() {
    if (!outbound || !canSubmit) return;
    const scope = `submit:${outbound.outbound_message_id}:${outbound.payload_hash}`;
    setPending("submit");
    try {
      await submitOutbound(
        outbound.outbound_message_id,
        { expected_payload_hash: outbound.payload_hash },
        { idempotencyKey: getOperationKey(scope) }
      );
      settleOperationKey(scope);
      await outboundQuery.refetch();
      await preflightQuery.refetch();
      await onChanged?.();
    } catch (error) {
      reportError(error);
      await outboundQuery.refetch().catch(() => undefined);
    } finally {
      setPending(null);
    }
  }

  async function cancel() {
    if (!outbound || !canCancel) return;
    const scope = `cancel:${outbound.outbound_message_id}:${outbound.version}`;
    setPending("cancel");
    try {
      await cancelOutbound(
        outbound.outbound_message_id,
        {},
        {
          etag: outbound.version,
          idempotencyKey: getOperationKey(scope),
        }
      );
      settleOperationKey(scope);
      await outboundQuery.refetch();
      await onChanged?.();
    } catch (error) {
      reportError(error);
      await outboundQuery.refetch().catch(() => undefined);
    } finally {
      setPending(null);
    }
  }

  const statusLabel = (status: string) =>
    t.has(`statuses.${status}`) ? t(`statuses.${status}`) : status;
  const deliveryStatusLabel = (status: string) =>
    t.has(`deliveryStatuses.${status}`)
      ? t(`deliveryStatuses.${status}`)
      : status;
  const approvalStatusLabel = (status: string) =>
    t.has(`approvalStatuses.${status}`)
      ? t(`approvalStatuses.${status}`)
      : status;
  const purposeLabel = (purpose: string) =>
    t.has(`purposes.${purpose}`) ? t(`purposes.${purpose}`) : purpose;
  const operatorActionLabel = (action: string) =>
    t.has(`nextActions.${action}`) ? t(`nextActions.${action}`) : action;
  const draftRows: MessageDraftListItem[] = threadDraftRows;
  const preflightFailed = disposition.failedChecks;
  const status = outbound?.status;
  const readOnly = Boolean(
    status && !canCancelOutbound(status) && status !== "draft"
  );

  return (
    <section
      aria-label={t("title")}
      className="flex flex-col gap-3 rounded-2xl border border-border bg-card p-4"
    >
      {linkedCampaignId ? (
        <Alert>
          <AlertDescription className="flex flex-wrap items-center justify-between gap-2">
            <span>{t("campaignLinked")}</span>
            <Button asChild size="xs" variant="link">
              <Link href={`/app/campaigns/${encodeURIComponent(linkedCampaignId)}`}>
                {t("openCampaign")}
              </Link>
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="min-w-0 flex-1 text-sm font-semibold">{t("title")}</h3>
        {outbound ? (
          <>
            <span className="text-xs text-muted-foreground">
              {t("submissionStatus", { status: statusLabel(outbound.status) })}
            </span>
            <span className="text-xs text-muted-foreground">
              {t("deliveryStatus", {
                status: deliveryStatusLabel(outbound.delivery_status),
              })}
            </span>
          </>
        ) : null}
      </div>

      {threadId ? (
        <div className="grid gap-1.5">
          <Label htmlFor={`${channelSelectId}-draft`}>{t("savedDrafts")}</Label>
          <select
            id={`${channelSelectId}-draft`}
            aria-label={t("savedDrafts")}
            className="h-9 rounded-md border border-input bg-background px-3 text-sm"
            value={selectedDraftId}
            onChange={(event) => setSelectedDraftId(event.target.value)}
          >
            <option value="">{t("selectDraft")}</option>
            {draftRows.map((row) => (
              <option key={row.draft_id} value={row.draft_id}>
                {row.subject || t("showDraft")} — {statusLabel(row.status)}
              </option>
            ))}
          </select>
          {threadDraftsQuery.isLoading ? (
            <p className="text-xs text-muted-foreground">{t("loadingDraft")}</p>
          ) : null}
          {!threadDraftsQuery.isLoading && draftRows.length === 0 ? (
            <p className="text-xs text-muted-foreground">{t("noDrafts")}</p>
          ) : null}
        </div>
      ) : null}

      {draftQuery.isLoading ? (
        <p className="text-sm text-muted-foreground">{t("loadingDraft")}</p>
      ) : null}
      {draftQuery.error ? (
        <Alert variant="warning">
          <AlertTitle>{t("draftError")}</AlertTitle>
          <AlertDescription>{t("draftError")}</AlertDescription>
        </Alert>
      ) : null}
      {mailboxMismatch ? (
        <Alert variant="warning">
          <AlertTitle>{t("draftMailboxMismatch")}</AlertTitle>
        </Alert>
      ) : null}
      {threadMismatch ? (
        <Alert variant="warning">
          <AlertTitle>{t("draftThreadMismatch")}</AlertTitle>
        </Alert>
      ) : null}

      {activeDraft && draftMatchesContext && !genericLinkedCampaignDraft ? (
        <>
          {!outboundId ? (
            <div className="grid gap-1 rounded-2xl border border-border bg-app-panel-muted p-3 text-sm">
              <p className="font-medium">{activeDraft.subject}</p>
              <p className="whitespace-pre-wrap break-words text-muted-foreground">
                {activeDraft.body_text}
              </p>
            </div>
          ) : null}
          {!outboundId && activeDraft.status === "draft" && !salesCampaignId ? (
            <>
              <details
                key={String(!lockedDeliveryChannelId && channelRows.length !== 1)}
                className="rounded-xl border border-border px-3 py-2"
                open={!lockedDeliveryChannelId && channelRows.length !== 1}
              >
              <summary className="cursor-pointer text-sm font-medium">{t("sendOptions")}</summary>
              <div className="mt-3 grid gap-3 sm:grid-cols-2">
                {lockedDeliveryChannelId ? (
                  <div className="grid gap-1.5">
                    <Label>{t("channel")}</Label>
                    <p className="flex min-h-9 items-center rounded-lg border border-border bg-app-panel-muted px-3 text-sm">
                      {channelRows.find((channel) => channel.id === lockedDeliveryChannelId)?.name ??
                        (channelsQuery.isLoading ? t("channelsLoading") : t("noChannel"))}
                    </p>
                  </div>
                ) : (
                  <div className="grid gap-1.5">
                    <Label htmlFor={channelSelectId}>{t("channel")}</Label>
                    <select
                      id={channelSelectId}
                      aria-label={t("channel")}
                      className="h-9 rounded-md border border-input bg-background px-3 text-sm"
                      value={effectiveChannelId}
                      onChange={(event) => setSelectedChannelId(event.target.value)}
                      disabled={channelsQuery.isLoading || pending === "freeze"}
                    >
                      <option value="">{t("selectChannel")}</option>
                      {channelRows.map((channel: DeliveryChannel) => (
                        <option key={channel.id} value={channel.id}>
                          {channel.name}
                        </option>
                      ))}
                    </select>
                  </div>
                )}
                {campaignMeetingTypeLocked ? (
                  <div className="grid gap-1.5">
                    <Label>{t("meetingType")}</Label>
                    <p className="flex min-h-9 items-center rounded-lg border border-border bg-app-panel-muted px-3 text-sm">
                      {campaignMeetingTypeId
                        ? meetingTypes.find((meetingType) => meetingType.id === campaignMeetingTypeId)?.name ??
                          (meetingTypesQuery.isLoading
                            ? t("meetingTypesLoading")
                            : t("noActiveMeetingTypes"))
                        : t("noMeetingType")}
                    </p>
                  </div>
                ) : (
                  <div className="grid gap-1.5">
                    <Label htmlFor={meetingTypeSelectId}>{t("meetingType")}</Label>
                    <select
                      id={meetingTypeSelectId}
                      aria-label={t("meetingType")}
                      className="h-9 rounded-md border border-input bg-background px-3 text-sm"
                      value={meetingTypeId}
                      onChange={(event) => setMeetingTypeId(event.target.value)}
                      disabled={meetingTypesQuery.isLoading || meetingTypesQuery.isError || pending === "freeze"}
                    >
                      <option value="">{t("noMeetingType")}</option>
                      {meetingTypes.map((meetingType: MeetingType) => (
                        <option key={meetingType.id} value={meetingType.id}>
                          {meetingType.name} · {t("meetingTypeDuration", { count: meetingType.duration_minutes })}
                          {meetingTypeHostNames.get(meetingType.host_principal_id) ? ` · ${t("meetingTypeHost", { name: meetingTypeHostNames.get(meetingType.host_principal_id)! })}` : ""}
                        </option>
                      ))}
                    </select>
                  </div>
                )}
                {meetingTypesQuery.isLoading ? <p role="status" className="mt-1 text-xs text-muted-foreground">{t("meetingTypesLoading")}</p> : null}
                {meetingTypesQuery.isError ? (
                  <div role="alert" className="mt-1 flex items-center gap-2 text-xs text-destructive">
                    <p>{t("meetingTypesLoadFailed")}</p>
                    <Button type="button" variant="link" size="xs" className="h-auto p-0 text-destructive" onClick={() => void meetingTypesQuery.refetch()}>{tCommon("retry")}</Button>
                  </div>
                ) : null}
                {meetingTypesQuery.isSuccess && meetingTypes.length === 0 ? <p role="status" className="mt-1 text-xs text-muted-foreground">{t("noActiveMeetingTypes")} <Link href="/app/meeting-types" className="underline underline-offset-4">{t("manageMeetingTypes")}</Link></p> : null}
                {meetingTypesQuery.hasNextPage ? <Button type="button" variant="link" size="xs" className="mt-1 h-auto w-fit p-0" disabled={meetingTypesQuery.isFetchingNextPage} onClick={() => void meetingTypesQuery.fetchNextPage()}>{meetingTypesQuery.isFetchingNextPage ? t("loadingMoreMeetingTypes") : t("loadMoreMeetingTypes")}</Button> : null}
                <div className="sm:col-span-2 flex items-start gap-2 rounded-md border border-border p-3">
                  <input
                    id={trackingSelectId}
                    type="checkbox"
                    className="mt-0.5 size-4 accent-primary"
                    checked={trackingRequested}
                    onChange={(event) => setTrackingRequested(event.target.checked)}
                    disabled={pending === "freeze"}
                    aria-describedby={`${trackingSelectId}-description`}
                  />
                  <div className="grid gap-1">
                    <Label htmlFor={trackingSelectId}>{t("trackingRequested")}</Label>
                    <p
                      id={`${trackingSelectId}-description`}
                      className="text-xs text-muted-foreground"
                    >
                      {t("trackingDescription")}
                    </p>
                  </div>
                </div>
              </div>
              </details>
              {channelsQuery.isLoading ? (
                <p className="text-xs text-muted-foreground">{t("channelsLoading")}</p>
              ) : null}
              {!channelsQuery.isLoading && channelRows.length === 0 ? (
                <Alert variant="warning">
                  <TriangleAlert aria-hidden />
                  <AlertDescription>{t("noChannel")}</AlertDescription>
                </Alert>
              ) : null}
              {channelsQuery.hasNextPage ? (
                <Button
                  type="button"
                  variant="outline"
                  size="xs"
                  disabled={channelsQuery.isFetchingNextPage}
                  onClick={() => void channelsQuery.fetchNextPage()}
                >
                  {t("loadMoreChannels")}
                </Button>
              ) : null}
              {freezeBlocked ? (
                <Alert variant="warning">
                  <AlertDescription>{t("campaignEditsUnsaved")}</AlertDescription>
                </Alert>
              ) : null}
              <Button
                type="button"
                size="sm"
                className="self-start"
                disabled={
                  freezeBlocked ||
                  !effectiveChannelId ||
                  channelRows.every((channel) => channel.id !== effectiveChannelId) ||
                  sendBlocked ||
                  pending !== null
                }
                onClick={() => void freeze()}
              >
                <ShieldCheck aria-hidden />
                {pending === "freeze" ? t("freezing") : t("freeze")}
              </Button>
            </>
          ) : null}

          {activeDraft.status !== "draft" && !outboundId ? (
            <Alert variant="warning">
              <AlertDescription>{t("versionConflict")}</AlertDescription>
            </Alert>
          ) : null}
        </>
      ) : null}

      {outboundQuery.isLoading ? (
        <p className="text-sm text-muted-foreground">{t("loadingDraft")}</p>
      ) : null}
      {outboundQuery.error ? (
        <Alert variant="warning">
          <AlertTitle>{t("draftError")}</AlertTitle>
          <AlertDescription>{t("draftError")}</AlertDescription>
        </Alert>
      ) : null}

      {outbound && draftMatchesContext ? (
        <>
          <div className="grid gap-2 rounded-2xl border border-border bg-app-panel-muted p-3 text-sm sm:grid-cols-2">
            <div>
              <span className="text-xs text-muted-foreground">{t("from")}</span>
              <p className="break-all font-medium">{outbound.from_address}</p>
            </div>
            <div>
              <span className="text-xs text-muted-foreground">{t("to")}</span>
              <p className="break-all font-medium">{outbound.to_address}</p>
            </div>
            <div>
              <span className="text-xs text-muted-foreground">{t("purpose")}</span>
              <p className="font-medium">{purposeLabel(outbound.purpose)}</p>
            </div>
            <div>
              <span className="text-xs text-muted-foreground">{t("payloadHash")}</span>
              <p className="break-all font-mono text-xs">{outbound.payload_hash}</p>
            </div>
            <div className="sm:col-span-2">
              <span className="text-xs text-muted-foreground">{t("subject")}</span>
              <p className="font-medium">{outbound.subject}</p>
            </div>
            <div className="sm:col-span-2">
              <span className="text-xs text-muted-foreground">{t("body")}</span>
              <p className="whitespace-pre-wrap break-words">{outbound.body_text}</p>
            </div>
          </div>

          {outbound.status === "draft" ? (
            <div className="flex flex-wrap items-center gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={pending !== null}
                onClick={() => void runPreflight()}
              >
                <ShieldCheck aria-hidden />
                {pending === "refresh" && preflightQuery.isFetching
                  ? t("checkingPolicy")
                  : t("checkPolicy")}
              </Button>
              {preflight ? (
                <span className="text-xs text-muted-foreground">
                  {t("preflightDescription")}
                </span>
              ) : null}
            </div>
          ) : null}

          {preflightQuery.error ? (
            <Alert variant="warning">
              <AlertTitle>{t("preflightTitle")}</AlertTitle>
              <AlertDescription>{t("draftError")}</AlertDescription>
            </Alert>
          ) : null}
          {preflight ? (
            <div className="grid gap-2" aria-live="polite">
              {preflight.is_execution_authorization ? (
                <Alert variant="warning">
                  <AlertDescription>{t("preflightNotAuthorization")}</AlertDescription>
                </Alert>
              ) : null}
              {preflight.global_blocking_reasons.length > 0 ? (
                <Alert variant="warning">
                  <AlertTitle>{t("globalBlocks")}</AlertTitle>
                  <AlertDescription>
                    <ul className="list-disc pl-5">
                      {preflight.global_blocking_reasons.map((reason, index) => (
                        <li key={`${reason}-${index}`}>{reason}</li>
                      ))}
                    </ul>
                  </AlertDescription>
                </Alert>
              ) : null}
              {preflightFailed.length > 0 ? (
                <Alert variant="warning">
                  <AlertTitle>{t("failedChecks")}</AlertTitle>
                  <AlertDescription>
                    <ul className="list-disc pl-5">
                      {preflightFailed.map((reason, index) => (
                        <li key={`${reason}-${index}`}>{reason}</li>
                      ))}
                    </ul>
                  </AlertDescription>
                </Alert>
              ) : null}
              {disposition.canSubmit ? (
                <Alert variant="success">
                  <AlertDescription>{t("preflightPassed")}</AlertDescription>
                </Alert>
              ) : null}
              {preflightFailed.length > 0 && !disposition.canRequestApproval ? (
                <p className="text-sm text-muted-foreground">{t("preflightBlocked")}</p>
              ) : null}
            </div>
          ) : null}

          {disposition.canRequestApproval &&
          outbound.status === "draft" &&
          !approvalRequestExists &&
          !salesCampaignId ? (
            <div className="grid gap-2 rounded-2xl border border-border p-3">
              <p className="text-sm">{t("approvalRequired")}</p>
              <div className="grid gap-1.5">
                <Label htmlFor={approvalReasonId}>{t("approvalReason")}</Label>
                <Textarea
                  id={approvalReasonId}
                  aria-label={t("approvalReason")}
                  value={approvalReason}
                  onChange={(event) => setApprovalReason(event.target.value)}
                  placeholder={t("approvalReasonPlaceholder")}
                  rows={2}
                  disabled={pending !== null}
                />
              </div>
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="self-start"
                disabled={!canRequestApproval || pending !== null}
                onClick={() => void requestApproval()}
              >
                {pending === "approval" ? t("requestingApproval") : t("requestApproval")}
              </Button>
            </div>
          ) : null}

          {outbound.approval_status === "approved" ? (
            <Alert variant={approvalIsValid ? "success" : "warning"}>
              <AlertTitle>
                {approvalIsValid
                  ? t("approvalValidTitle")
                  : t("approvalNeedsRefreshTitle")}
              </AlertTitle>
              <AlertDescription>
                {!outbound.approval_snapshot_matches
                  ? t("approvalSnapshotMismatch")
                  : approvalIsValid
                    ? t("approvalValidDescription")
                    : t("approvalInvalidDescription")}
              </AlertDescription>
            </Alert>
          ) : outbound.approval_status === "pending" ? (
            <Alert>
              <AlertDescription>{t("approvalPending")}</AlertDescription>
            </Alert>
          ) : outbound.approval_status ? (
            <Alert variant="warning">
              <AlertDescription>
                {t("approvalStatusOther", {
                  status: approvalStatusLabel(outbound.approval_status),
                })}
              </AlertDescription>
            </Alert>
          ) : null}
          {createdApproval?.outboundId === outboundId && !matchingApproval ? (
            <Alert variant="success">
              <AlertDescription>{t("approvalCreated")}</AlertDescription>
            </Alert>
          ) : null}
          {approvalId ? (
            <Link
              href={`/app/approvals/${encodeURIComponent(approvalId)}`}
              className="w-fit text-sm font-medium text-foreground underline underline-offset-4"
            >
              {t("approvalLink")}
            </Link>
          ) : null}

          {outbound.status === "draft" && me?.kind !== "human" ? (
            <Alert variant="warning">
              <AlertDescription>
                {t("humanSendRequired")}
              </AlertDescription>
            </Alert>
          ) : null}
          {outbound.status === "draft" &&
          !mailAccountsQuery.isLoading &&
          me?.kind === "human" &&
          !hasSendGrant ? (
            <Alert variant="warning">
              <AlertDescription>{t("sendGrantRequired")}</AlertDescription>
            </Alert>
          ) : null}

          {outbound.status === "draft" ? (
            <div className="flex flex-wrap items-center gap-2">
              {allowSubmit && !linkedCampaignId ? (
                <Button
                  type="button"
                  size="sm"
                  disabled={!canSubmit || pending !== null}
                  onClick={() => setConfirmSubmitOpen(true)}
                >
                  <Send aria-hidden />
                  {pending === "submit" ? t("submitting") : t("submit")}
                </Button>
              ) : null}
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={!canCancel || pending !== null}
                onClick={() => setConfirmCancelOpen(true)}
              >
                {t("cancel")}
              </Button>
              {!preflight ? (
                <span className="text-xs text-muted-foreground">{t("preflightRequired")}</span>
              ) : null}
            </div>
          ) : null}

          {outbound.status === "queued" || outbound.status === "claimed" ? (
            <>
              <Alert>
                <AlertDescription>{t("queued")}</AlertDescription>
              </Alert>
              {canCancel ? (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="self-start"
                  disabled={pending !== null}
                  onClick={() => setConfirmCancelOpen(true)}
                >
                  {t("cancel")}
                </Button>
              ) : null}
            </>
          ) : null}
          {outbound.status === "submitting" ? (
            <Alert variant="warning">
              <AlertDescription>{t("readOnly")}</AlertDescription>
            </Alert>
          ) : null}
          {outbound.status === "accepted" ? (
            <Alert variant="success">
              <AlertTitle>{t("accepted")}</AlertTitle>
              <AlertDescription>{t("acceptedNotDelivered")}</AlertDescription>
            </Alert>
          ) : null}
          {outbound.status === "unknown" ? (
            <Alert variant="warning">
              <AlertTitle>{t("unknown")}</AlertTitle>
              <AlertDescription>
                <p>
                  {t("operatorNextAction", {
                    action: operatorActionLabel(outbound.next_action),
                  })}
                </p>
                <Link
                  href="/app/operations/jobs"
                  className="mt-1 inline-block font-medium underline underline-offset-4"
                >
                  {t("openOperations")}
                </Link>
              </AlertDescription>
            </Alert>
          ) : null}
          {outbound.status === "failed" || outbound.status === "canceled" ? (
            <Alert variant="warning">
              <AlertDescription>
                {outbound.failure_code ?? t("readOnly")}
              </AlertDescription>
            </Alert>
          ) : null}

          {outbound.thread_id ? (
            <Link
              href={`/app/inbox/${encodeURIComponent(outbound.thread_id)}`}
              className="w-fit text-sm font-medium underline underline-offset-4"
            >
              {t("openConversation")}
            </Link>
          ) : null}

          <section
            aria-label={t("engagementTitle")}
            className="grid gap-1 rounded-xl border border-border p-3 text-sm"
          >
            <h4 className="font-medium">{t("engagementTitle")}</h4>
            {outbound.engagement.tracking_enabled ? (
              <>
                <p>{t("trackingActive")}</p>
                <p>{t("openRequestCount", { count: outbound.engagement.open_request_count })}</p>
                <p>{t("clickRequestCount", { count: outbound.engagement.click_request_count })}</p>
                <p className="text-xs text-muted-foreground">{t("engagementRequestDisclaimer")}</p>
              </>
            ) : (
              <p className="text-xs text-muted-foreground">{t("trackingOff")}</p>
            )}
          </section>

          <section
            aria-label={t("linkedReplies")}
            className="grid gap-2 rounded-xl border border-border p-3"
          >
            <h4 className="text-sm font-medium">{t("linkedReplies")}</h4>
            {outbound.known_replies.length > 0 ? (
              <ul className="grid gap-2">
                {outbound.known_replies.map((reply) => (
                  <li
                    key={reply.inbound_message_id}
                    className="flex flex-wrap items-center justify-between gap-2 text-sm"
                  >
                    <Link
                      href={`/app/inbox/${encodeURIComponent(reply.thread_id)}`}
                      className="font-medium underline underline-offset-4"
                    >
                      {t("replyFrom", { from: reply.from_address })}
                    </Link>
                    <time
                      dateTime={reply.received_at}
                      className="text-xs text-muted-foreground"
                    >
                      {reply.received_at}
                    </time>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="text-sm text-muted-foreground">{t("noLinkedReplies")}</p>
            )}
          </section>

          {outbound.status !== "draft" || approvalId ? (
            <div className="flex flex-wrap items-center gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={pending !== null || outboundQuery.isFetching}
                onClick={() => void refreshStatus()}
              >
                <RefreshCw aria-hidden />
                {pending === "refresh"
                  ? t("refreshingStatus")
                  : outbound.status === "unknown"
                    ? t("refreshStatus")
                    : t("refreshApproval")}
              </Button>
              {readOnly ? (
                <span className="text-xs text-muted-foreground">{t("readOnly")}</span>
              ) : null}
            </div>
          ) : null}

          <ConfirmIrreversible
            open={confirmSubmitOpen}
            onOpenChange={setConfirmSubmitOpen}
            title={t("confirmSubmitTitle")}
            description={t("confirmSubmitDescription")}
            recipientsCount={1}
            recipientAddresses={[outbound.to_address]}
            sender={outbound.from_address}
            purpose={outbound.purpose}
            contentPreview={{ subject: outbound.subject, body: outbound.body_text }}
            confirmText={t("confirmWord")}
            confirmLabel={t("confirmSubmit")}
            pending={pending === "submit"}
            onConfirm={() => {
              setConfirmSubmitOpen(false);
              void submit();
            }}
          />
          <ConfirmIrreversible
            open={confirmCancelOpen}
            onOpenChange={setConfirmCancelOpen}
            title={t("confirmCancelTitle")}
            description={t("confirmCancelDescription")}
            recipientsCount={1}
            recipientAddresses={[outbound.to_address]}
            sender={outbound.from_address}
            purpose={outbound.purpose}
            contentPreview={{ subject: outbound.subject, body: outbound.body_text }}
            confirmText={t("cancelWord")}
            confirmLabel={t("confirmCancel")}
            pending={pending === "cancel"}
            onConfirm={() => {
              setConfirmCancelOpen(false);
              void cancel();
            }}
          />
        </>
      ) : null}
    </section>
  );
}
