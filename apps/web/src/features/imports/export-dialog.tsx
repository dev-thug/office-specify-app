"use client";

import { useTranslations } from "next-intl";
import { useRef, useState, type FormEvent } from "react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import { createExport } from "@/lib/api/imports";

/**
 * 보내기(export) 요청 대화상자 — 비동기 작업을 만들고 작업 ID 를 보여준다.
 * 진행 추적은 /app/operations/jobs 에서 한다 (B.2 3574행).
 */
export function ExportDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useTranslations("imports.export");
  const tCommon = useTranslations("common");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [jobId, setJobId] = useState<string | null>(null);
  const idemRef = useRef<string>(idempotencyKey());

  function handleOpenChange(next: boolean) {
    if (!next) {
      setError(null);
      setJobId(null);
      idemRef.current = idempotencyKey();
    }
    onOpenChange(next);
  }

  async function onSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const form = new FormData(e.currentTarget);
    const fieldsRaw = String(form.get("fields") ?? "").trim();
    const filtersRaw = String(form.get("filters") ?? "").trim();
    const fields = fieldsRaw.split(",").map((field) => field.trim()).filter(Boolean);
    if (fields.length === 0) {
      setError(t("fieldsRequired"));
      return;
    }

    let filters: Record<string, unknown> | undefined;
    if (filtersRaw) {
      try {
        const parsed = JSON.parse(filtersRaw) as unknown;
        if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
          setError(t("filtersInvalid"));
          return;
        }
        const candidate = parsed as Record<string, unknown>;
        const allowed = new Set(["company_ids", "lifecycle_stage", "q"]);
        if (Object.keys(candidate).some((key) => !allowed.has(key))) {
          setError(t("filtersInvalid"));
          return;
        }
        if (candidate.company_ids !== undefined && (!Array.isArray(candidate.company_ids) || candidate.company_ids.some((id) => typeof id !== "string"))) {
          setError(t("filtersInvalid"));
          return;
        }
        if (["lifecycle_stage", "q"].some((key) => candidate[key] !== undefined && typeof candidate[key] !== "string")) {
          setError(t("filtersInvalid"));
          return;
        }
        filters = candidate;
      } catch {
        setError(t("filtersInvalid"));
        return;
      }
    }

    setPending(true);
    setError(null);
    try {
      const res = await createExport(
        {
          fields,
          filters,
        },
        { idempotencyKey: idemRef.current }
      );
      idemRef.current = idempotencyKey();
      setJobId(res.data.import_id ?? res.data.job_id ?? res.data.export_job_id ?? null);
    } catch (err) {
      const apiErr = err instanceof ApiError ? err : null;
      setError(apiErr?.problem.detail ?? t("failed"));
      if (apiErr && apiErr.problem.retryable !== true && apiErr.status < 500) {
        idemRef.current = idempotencyKey();
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>{t("description")}</DialogDescription>
        </DialogHeader>

        {jobId !== null ? (
          <div className="grid gap-4">
            <p className="text-sm">{t("success")}</p>
            <details className="rounded-2xl border border-border bg-app-panel-muted px-4 py-3 text-sm">
              <summary className="cursor-pointer font-medium">{t("technicalDetails")}</summary>
              <dl className="mt-2 grid grid-cols-[6rem_1fr] gap-x-3 gap-y-1.5">
                <dt className="text-muted-foreground">{t("jobId")}</dt>
                <dd className="break-all font-mono text-2xs">{jobId}</dd>
              </dl>
            </details>
            <DialogFooter>
              <Button type="button" onClick={() => handleOpenChange(false)}>
                {tCommon("close")}
              </Button>
            </DialogFooter>
          </div>
        ) : (
          <form onSubmit={onSubmit} className="grid gap-4">
            <div className="grid gap-1.5">
              <Label htmlFor="export-fields">{t("fields")}</Label>
              <Input id="export-fields" name="fields" required placeholder="company.name, contact.name, contact.email" />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="export-filters">{t("filters")}</Label>
              <Textarea
                id="export-filters"
                name="filters"
                rows={3}
                className="font-mono text-ui"
                placeholder='{"lifecycle_stage": "prospect", "q": "example"}'
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
                onClick={() => handleOpenChange(false)}
              >
                {tCommon("cancel")}
              </Button>
              <Button type="submit" disabled={pending}>
                {pending ? t("submitting") : t("submit")}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
