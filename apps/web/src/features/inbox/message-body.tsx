"use client";

/**
 * 메시지 본문 렌더러 (§15.1).
 * - body_html_sanitized(서버가 정제한 HTML)만 렌더한다 — 정제 필드가 없으면
 *   절대 HTML 을 주입하지 않고 body_text 를 텍스트로 보여준다.
 * - HTML 은 sandbox 속성이 빈 <iframe> + CSP meta(srcDoc)로 샌드박스한다:
 *   스크립트·폼·same-origin·팝업·원격 리소스(추적 이미지) 모두 차단.
 */
import { useTranslations } from "next-intl";

import { cn } from "@/lib/utils";
import { wrapSanitizedHtml } from "./thread-state";

interface MessageBodyProps {
  bodyText: string | null;
  bodyHtmlSanitized: string | null;
  subject?: string | null;
  className?: string;
}

export function MessageBody({
  bodyText,
  bodyHtmlSanitized,
  subject,
  className,
}: MessageBodyProps) {
  const t = useTranslations("inbox.message");

  if (bodyHtmlSanitized) {
    return (
      <iframe
        // sandbox 비어 있음 = 모든 권한 차단 (스크립트·폼·팝업·same-origin).
        sandbox=""
        referrerPolicy="no-referrer"
        title={subject ?? t("bodyTitle")}
        srcDoc={wrapSanitizedHtml(bodyHtmlSanitized)}
        className={cn(
          "h-72 w-full rounded-lg border border-border bg-background",
          className
        )}
      />
    );
  }

  if (bodyText) {
    return (
      <p className={cn("whitespace-pre-wrap text-sm", className)}>
        {bodyText}
      </p>
    );
  }

  return (
    <p className={cn("text-sm text-muted-foreground", className)}>
      {t("noBody")}
    </p>
  );
}
