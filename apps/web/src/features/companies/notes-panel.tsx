"use client";

/**
 * 메모 패널 — POST /notes (대상 1개·본문, B.2). B.2에 메모 목록 GET이 없으므로
 * 목록 영역은 호출자가 활동 스트림의 note 행(feed prop)으로 채운다.
 * 멱등 키는 폼 마운트 시 발급, 확정 응답 후에만 재발급한다 (DESIGN.md).
 */
import { useTranslations } from "next-intl";
import { useRef, useState, type ReactNode } from "react";
import { useQueryClient } from "@tanstack/react-query";

import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { createNote } from "@/lib/api/companies";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";

interface NotesPanelProps {
  /** 메시지 네임스페이스 — companies/contacts 둘 다 notes.* 구조가 같다. */
  ns: "companies" | "contacts";
  companyId?: string;
  contactId?: string;
  /** 작성 성공 후 무효화할 쿼리 키 목록. */
  invalidateKeys?: readonly (readonly unknown[])[];
  /** 서버 allowed_actions 힌트 — false 면 작성 폼을 숨긴다. */
  canWrite?: boolean;
  /** 메모 목록 영역 — 보통 note.* 활동 행을 그린 ActivityFeed. */
  feed?: ReactNode;
}

export function NotesPanel({
  ns,
  companyId,
  contactId,
  invalidateKeys = [],
  canWrite = true,
  feed,
}: NotesPanelProps) {
  const t = useTranslations(`${ns}.notes`);
  const queryClient = useQueryClient();
  const [body, setBody] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const idemRef = useRef<string>(idempotencyKey());

  async function submit() {
    setPending(true);
    setError(null);
    try {
      await createNote(
        { company_id: companyId, contact_id: contactId, body: body.trim() },
        { idempotencyKey: idemRef.current }
      );
      idemRef.current = idempotencyKey();
      setBody("");
      for (const key of invalidateKeys) {
        await queryClient.invalidateQueries({ queryKey: [...key] });
      }
    } catch (err) {
      setError(
        err instanceof ApiError ? (err.problem.detail ?? t("error")) : t("error")
      );
      if (
        err instanceof ApiError &&
        err.problem.retryable !== true &&
        err.status < 500
      ) {
        idemRef.current = idempotencyKey();
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="flex flex-col gap-4">
      {canWrite ? (
        <form
          className="flex flex-col gap-2 rounded-2xl border border-border p-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (body.trim()) void submit();
          }}
        >
          <Label htmlFor={`note-body-${companyId ?? contactId ?? "x"}`}>
            {t("addTitle")}
          </Label>
          <Textarea
            id={`note-body-${companyId ?? contactId ?? "x"}`}
            value={body}
            onChange={(e) => setBody(e.target.value)}
            placeholder={t("placeholder")}
            rows={3}
            maxLength={200_000}
          />
          <div className="flex justify-end">
            <Button type="submit" size="sm" disabled={pending || !body.trim()}>
              {pending ? t("submitting") : t("submit")}
            </Button>
          </div>
          {error ? (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          ) : null}
        </form>
      ) : null}
      {feed}
    </div>
  );
}
