"use client";

/**
 * 회신 초안 작성 (B.3.10, §9.3).
 * - 초안 생성만 한다 — 제출·발송은 발송 흐름(동결→검토→제출)에서 진행한다.
 * - expected_last_inbound_id 를 보내 작성 중 새 답장 도착을 서버가 감지한다
 *   (409 REPLY_CONTEXT_CHANGED → 재검토 요구).
 * - 성공 패널에는 "accepted ≠ delivered" 규칙을 명시한다.
 */
import { Send } from "lucide-react";
import { useTranslations } from "next-intl";
import { useRef, useState } from "react";
import { toast } from "sonner";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { idempotencyKey } from "@/lib/api/client";
import {
  createReplyDraft,
  type DraftVisibility,
  type ReplyDraftResult,
} from "@/lib/api/inbox";
import { ApiError } from "@/lib/api/problem";
import { DraftSendingWorkflow } from "@/features/sending/draft-sending-workflow";
import {
  inboxErrorKey,
  type ReplyBlockReason,
} from "./thread-state";

export interface ReplyComposerProps {
  threadId: string;
  /** 사용자가 마지막으로 확인한 마지막 수신 메시지 id — 서버 불일치 시 409. */
  expectedLastInboundId: string | null;
  /** 마지막 수신 보낸 사람 주소(추정) — 실제 수신자는 서버가 확인한다. */
  inferredTo: string | null;
  /** 작성을 막는 이유 — null 이면 작성 가능. */
  blockReason: ReplyBlockReason | null;
  /** 충돌 해소(새 내용 확인) 요청 — collision 차단 시 버튼으로 연다. */
  onAcknowledgeCollision?: () => void;
  onCreated?: (draft: ReplyDraftResult) => void;
  /** 실제 메일함 id — 있으면 저장 회신 초안의 발송 흐름도 연다. */
  mailAccountId?: string;
  onSendingChanged?: () => void | Promise<unknown>;
  /** API 오류 통지 — REPLY_CONTEXT_CHANGED 등을 부모가 충돌 처리하게 한다. */
  onReplyError?: (error: ApiError) => void;
}

const BLOCK_MESSAGE_KEY: Record<ReplyBlockReason, string> = {
  reconnect: "reply.blockedReconnect",
  collision: "reply.blockedCollision",
  forbidden: "reply.blockedForbidden",
  no_inbound: "reply.blockedNoInbound",
};

export function ReplyComposer({
  threadId,
  expectedLastInboundId,
  inferredTo,
  blockReason,
  onAcknowledgeCollision,
  onCreated,
  mailAccountId,
  onSendingChanged,
  onReplyError,
}: ReplyComposerProps) {
  const t = useTranslations("inbox");
  const [body, setBody] = useState("");
  const [visibility, setVisibility] = useState<DraftVisibility>("private");
  const [pending, setPending] = useState(false);
  const [created, setCreated] = useState<ReplyDraftResult | null>(null);
  // 논리 작업(이 초안 작성)당 멱등 키 하나 — 네트워크·5xx 재시도는 같은 키.
  const opKeyRef = useRef<string | undefined>(undefined);

  const blocked = blockReason !== null || expectedLastInboundId === null;
  const canSubmit =
    !blocked && body.trim().length > 0 && !pending && created === null;

  async function submit() {
    if (!expectedLastInboundId || body.trim() === "") return;
    opKeyRef.current ??= idempotencyKey();
    setPending(true);
    try {
      const res = await createReplyDraft(
        threadId,
        {
          expected_last_inbound_id: expectedLastInboundId,
          body_text: body,
          body_html: null,
          visibility,
        },
        { idempotencyKey: opKeyRef.current }
      );
      // 논리 작업 완료 — 이후 다른 초안은 새 키로.
      opKeyRef.current = undefined;
      setCreated(res.data);
      onCreated?.(res.data);
    } catch (error) {
      if (error instanceof ApiError) {
        const key = inboxErrorKey(error.problem);
        toast.error(
          key ? t(`errors.${key}`) : (error.problem.detail ?? error.message)
        );
        // 확정적 실패(재시도 불가 <500)면 새 논리 작업 — 키 재발급.
        if (error.problem.retryable !== true && error.status < 500) {
          opKeyRef.current = undefined;
        }
        onReplyError?.(error);
      } else {
        toast.error(t("errors.title"));
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <section
      aria-label={t("reply.title")}
      className="flex flex-col gap-3 rounded-2xl border border-border bg-card p-4"
    >
      <h2 className="text-sm font-semibold">{t("reply.title")}</h2>

      {blockReason !== null ? (
        <Alert variant="warning">
          <AlertTitle>{t("errors.title")}</AlertTitle>
          <AlertDescription>
            <p>{t(BLOCK_MESSAGE_KEY[blockReason])}</p>
            {blockReason === "collision" && onAcknowledgeCollision ? (
              <Button
                type="button"
                variant="outline"
                size="xs"
                onClick={onAcknowledgeCollision}
              >
                {t("collision.showNew")}
              </Button>
            ) : null}
          </AlertDescription>
        </Alert>
      ) : null}

      {expectedLastInboundId === null && blockReason === null ? (
        <p className="text-sm text-muted-foreground">
          {t("reply.blockedNoInbound")}
        </p>
      ) : null}

      {created ? (
        <Alert variant="success">
          <AlertTitle>{t("reply.createdTitle")}</AlertTitle>
          <AlertDescription>
            <p>{t("reply.createdDescription")}</p>
            {created.to_address ? (
              <p>
                <span className="font-medium">
                  {t("reply.createdRecipient")}:
                </span>{" "}
                {created.to_address}
              </p>
            ) : null}
            <p className="font-medium">{t("reply.acceptedNotDelivered")}</p>
          </AlertDescription>
        </Alert>
      ) : (
        <>
          <div className="grid gap-1.5">
            <span className="text-xs font-medium text-muted-foreground">
              {t("reply.toLabel")}
            </span>
            <p className="text-sm">
              <span className="font-medium">{inferredTo ?? "—"}</span>{" "}
              <span className="text-xs text-muted-foreground">
                {t("reply.toInferred")}
              </span>
            </p>
          </div>

          <div className="grid gap-1.5">
            <Label htmlFor="reply-body">{t("reply.bodyLabel")}</Label>
            <Textarea
              id="reply-body"
              value={body}
              onChange={(e) => setBody(e.target.value)}
              placeholder={t("reply.placeholder")}
              rows={5}
              disabled={blocked || pending}
            />
          </div>

          <div className="flex flex-wrap items-end gap-3">
            <div className="grid gap-1.5">
              <Label id="reply-visibility-label" className="text-xs">
                {t("reply.visibility")}
              </Label>
              <Select
                value={visibility}
                onValueChange={(v) => setVisibility(v as DraftVisibility)}
              >
                <SelectTrigger
                  aria-labelledby="reply-visibility-label"
                  className="w-44"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="private">
                    {t("reply.visibilityPrivate")}
                  </SelectItem>
                  <SelectItem value="mailbox">
                    {t("reply.visibilityMailbox")}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>
            <Button
              type="button"
              size="sm"
              className="ml-auto"
              disabled={!canSubmit}
              onClick={submit}
            >
              <Send aria-hidden />
              {pending ? t("reply.submitting") : t("reply.submit")}
            </Button>
          </div>
        </>
      )}

      {mailAccountId ? (
        <DraftSendingWorkflow
          key={`${threadId}:${created?.id ?? "saved"}`}
          draftId={created?.id}
          threadId={threadId}
          mailAccountId={mailAccountId}
          sendBlocked={blocked}
          onChanged={onSendingChanged}
        />
      ) : null}
    </section>
  );
}
