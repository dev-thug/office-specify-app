"use client";

/**
 * 대화 내 메시지 한 통 — 참여자 헤더, 방향·분류 배지, 본문(sandboxed),
 * 비-인라인 첨부 다운로드, 분류 검토 진입.
 */
import { Download, Paperclip } from "lucide-react";
import { useFormatter, useTranslations } from "next-intl";
import { useRef, useState } from "react";
import { toast } from "sonner";

import { StatusBadge } from "@/components/patterns/status-badge";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  requestAttachmentDownload,
  type EmailMessage,
  type MessageAttachment,
  type ParticipantRole,
} from "@/lib/api/inbox";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import { cn } from "@/lib/utils";
import { MessageBody } from "./message-body";
import {
  downloadableAttachments,
  formatBytes,
  inboxErrorKey,
  isReviewable,
} from "./thread-state";

const ROLE_ORDER: ParticipantRole[] = ["from", "reply_to", "to", "cc", "bcc"];

function groupParticipants(message: EmailMessage) {
  const grouped = new Map<ParticipantRole, { name: string | null; address: string }[]>();
  for (const p of message.participants ?? []) {
    const list = grouped.get(p.role) ?? [];
    list.push({ name: p.display_name, address: p.address });
    grouped.set(p.role, list);
  }
  return grouped;
}

const ROLE_LABEL_KEY: Record<ParticipantRole, string> = {
  from: "from",
  reply_to: "replyTo",
  to: "to",
  cc: "cc",
  bcc: "bcc",
};

interface MessageCardProps {
  message: EmailMessage;
  canReviewClassification: boolean;
  onReview: (message: EmailMessage) => void;
}

export function MessageCard({
  message,
  canReviewClassification,
  onReview,
}: MessageCardProps) {
  const t = useTranslations("inbox");
  const format = useFormatter();
  const downloadKeyRef = useRef<string | undefined>(undefined);
  const [downloading, setDownloading] = useState<string | null>(null);

  const grouped = groupParticipants(message);
  const attachments = downloadableAttachments(message);

  async function download(attachment: MessageAttachment) {
    // 논리 작업(이 첨부의 다운로드)당 키 하나 — 재시도는 같은 키로.
    downloadKeyRef.current ??= idempotencyKey();
    try {
      setDownloading(attachment.id);
      const res = await requestAttachmentDownload(attachment.id, {
        idempotencyKey: downloadKeyRef.current,
      });
      // 성공하면 논리 작업 완료 — 다음 첨부 요청은 새 키로.
      downloadKeyRef.current = undefined;
      window.open(res.data.download_url, "_blank", "noopener,noreferrer");
    } catch (error) {
      const key =
        error instanceof ApiError ? inboxErrorKey(error.problem) : null;
      toast.error(
        key ? t(`errors.${key}`) : t("message.downloadFailed")
      );
    } finally {
      setDownloading(null);
    }
  }

  return (
    <article
      className={cn(
        "flex flex-col gap-3 rounded-2xl border border-border bg-card px-4 py-3.5",
        message.direction === "outbound" && "bg-app-panel-muted/50"
      )}
    >
      <header className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
        <Badge
          variant={message.direction === "inbound" ? "info-subtle" : "secondary"}
        >
          {message.direction === "inbound"
            ? t("message.inbound")
            : t("message.outbound")}
        </Badge>
        {ROLE_ORDER.map((role) => {
          const list = grouped.get(role);
          if (!list?.length) return null;
          return (
            <span
              key={role}
              className="min-w-0 truncate text-xs text-muted-foreground"
            >
              <span className="font-medium">
                {t(`message.${ROLE_LABEL_KEY[role]}`)}
              </span>{" "}
              {list
                .map((p) => p.name ?? p.address)
                .join(", ")}
            </span>
          );
        })}
        <time
          dateTime={message.message_at}
          className="ml-auto shrink-0 text-xs text-muted-foreground tabular-nums"
        >
          {format.dateTime(new Date(message.message_at), {
            dateStyle: "medium",
            timeStyle: "short",
          })}
        </time>
      </header>

      {message.subject ? (
        <h3 className="text-sm font-medium">{message.subject}</h3>
      ) : null}

      <MessageBody
        bodyText={message.body_text}
        bodyHtmlSanitized={message.body_html_sanitized}
        subject={message.subject}
      />

      {/* 발신 메시지의 발송 작업 상태 — accepted 는 전달 완료가 아니다. */}
      {message.direction === "outbound" && message.outbound_status ? (
        <div className="flex flex-wrap items-center gap-2 text-xs">
          <span className="text-muted-foreground">
            {t("message.outboundStatus")}
          </span>
          <StatusBadge status={message.outbound_status} />
          {message.outbound_status === "accepted" ? (
            <span className="text-muted-foreground">
              {t("message.acceptedNotDelivered")}
            </span>
          ) : null}
          {message.outbound_status === "unknown" ? (
            // unknown 결과는 "조사" 경로만 — 재발송 버튼을 두지 않는다 (B.5).
            <span className="text-muted-foreground">
              {t("message.unknownInvestigate")}
            </span>
          ) : null}
        </div>
      ) : null}

      {attachments.length > 0 ? (
        <div className="flex flex-col gap-1.5">
          <p className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
            <Paperclip aria-hidden className="size-3.5" />
            {t("message.attachments")}
          </p>
          <ul className="flex flex-wrap gap-1.5">
            {attachments.map((attachment) => (
              <li key={attachment.id}>
                <Button
                  type="button"
                  variant="outline"
                  size="xs"
                  disabled={downloading !== null}
                  onClick={() => download(attachment)}
                >
                  <Download aria-hidden />
                  {attachment.filename}
                  {formatBytes(attachment.size_bytes) ? (
                    <span className="text-muted-foreground">
                      {formatBytes(attachment.size_bytes)}
                    </span>
                  ) : null}
                </Button>
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {isReviewable(message) ? (
        <footer className="flex flex-wrap items-center gap-2 border-t border-border/60 pt-2.5">
          <span className="text-xs text-muted-foreground">
            {t("message.classification")}
          </span>
          <Badge variant="amber-subtle">
            {t(`classifications.${message.classification}`)}
          </Badge>
          <span className="text-xs text-muted-foreground">
            {message.classification_reviewed_by
              ? t("message.reviewed")
              : t("message.unreviewed")}
          </span>
          <Button
            type="button"
            variant="ghost"
            size="xs"
            className="ml-auto"
            disabled={!canReviewClassification}
            onClick={() => onReview(message)}
          >
            {t("message.reviewAction")}
          </Button>
        </footer>
      ) : null}
    </article>
  );
}
