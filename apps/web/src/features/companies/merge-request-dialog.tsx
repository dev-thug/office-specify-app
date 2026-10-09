"use client";

/**
 * 기업 병합 요청 — POST /companies/{target}/merge-requests (Manager/Admin, 202 job).
 * 어느 쪽이 "남을 기업(target)"인지 사용자가 고르고, 값이 다른 필드는
 * 개별 라디오로 어느 쪽을 남길지 선택한다. 도메인 일치만으로 자동 병합하지 않는다.
 */
import { useTranslations } from "next-intl";
import { useRef, useState } from "react";

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
import { CompanyPicker } from "@/features/companies/company-picker";
import {
  createCompanyMergeRequest,
  type Company,
} from "@/lib/api/companies";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import { cn } from "@/lib/utils";

const MERGE_FIELDS = [
  "name",
  "domain",
  "website_url",
  "industry",
  "country_code",
  "owner_principal_id",
  "description",
] as const;
type MergeField = (typeof MERGE_FIELDS)[number];

function fieldValue(c: Company | undefined | null, f: MergeField): string {
  const v = c?.[f];
  return v == null ? "" : String(v);
}

export interface MergeRequestDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /**
   * 두 회사가 이미 정해진 경우 (목록에서 2행 선택).
   * 없으면 `fixedTarget` 만 주고 소스는 대화상자에서 고른다 (기업 상세).
   */
  pair?: [Company, Company];
  /** 기업 상세에서 여는 경우 — 남을 기업은 현재 기업으로 고정. */
  fixedTarget?: Company;
  pickerLabels: { placeholder: string; none: string; empty: string };
  onRequested?: () => void;
}

export function MergeRequestDialog({
  open,
  onOpenChange,
  pair,
  fixedTarget,
  pickerLabels,
  onRequested,
}: MergeRequestDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <MergeBody
        key={(pair ?? [fixedTarget]).map((c) => c?.id).join(":")}
        pair={pair}
        fixedTarget={fixedTarget}
        pickerLabels={pickerLabels}
        onClose={() => onOpenChange(false)}
        onRequested={onRequested}
      />
    </Dialog>
  );
}

function MergeBody({
  pair,
  fixedTarget,
  pickerLabels,
  onClose,
  onRequested,
}: {
  pair?: [Company, Company];
  fixedTarget?: Company;
  pickerLabels: { placeholder: string; none: string; empty: string };
  onClose: () => void;
  onRequested?: () => void;
}) {
  const t = useTranslations("companies.merge");
  const tc = useTranslations("common");
  const [targetId, setTargetId] = useState<string>(
    pair?.[0].id ?? fixedTarget?.id ?? ""
  );
  const [source, setSource] = useState<Company | null>(
    pair ? null : null // 쌍 모드는 targetId로 유도, 상세 모드는 picker로 선택
  );
  const [choices, setChoices] = useState<Record<MergeField, "source" | "target">>(
    () =>
      Object.fromEntries(MERGE_FIELDS.map((f) => [f, "target"])) as Record<
        MergeField,
        "source" | "target"
      >
  );
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const idemRef = useRef<string>(idempotencyKey());

  const target = pair
    ? (pair.find((c) => c.id === targetId) ?? pair[0])
    : fixedTarget;
  const resolvedSource = pair
    ? (pair.find((c) => c.id !== target?.id) ?? null)
    : source;
  const differing =
    target && resolvedSource
      ? MERGE_FIELDS.filter(
          (f) => fieldValue(resolvedSource, f) !== fieldValue(target, f)
        )
      : [];

  async function submit() {
    if (!target || !resolvedSource || target.id === resolvedSource.id) {
      setError(t("sameCompany"));
      return;
    }
    setPending(true);
    setError(null);
    try {
      await createCompanyMergeRequest(
        target.id,
        {
          source_id: resolvedSource.id,
          field_choices: Object.fromEntries(
            differing.map((f) => [f, choices[f] ?? "target"])
          ),
        },
        { idempotencyKey: idemRef.current }
      );
      onRequested?.();
      onClose();
    } catch (err) {
      const apiErr = err instanceof ApiError ? err : null;
      setError(
        apiErr?.status === 403
          ? t("forbidden")
          : (apiErr?.problem.detail ?? t("error"))
      );
      if (apiErr && apiErr.problem.retryable !== true && apiErr.status < 500) {
        idemRef.current = idempotencyKey();
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <DialogContent>
      <DialogHeader>
        <DialogTitle>{t("title")}</DialogTitle>
        <DialogDescription>{t("description")}</DialogDescription>
      </DialogHeader>

      <div className="flex flex-col gap-3">
        {pair ? (
          <fieldset className="grid gap-1.5">
            <legend className="text-sm font-medium">{t("target")}</legend>
            <div className="flex flex-col gap-1.5" role="radiogroup">
              {pair.map((c) => (
                <label
                  key={c.id}
                  className={cn(
                    "flex cursor-pointer items-center gap-2 rounded-lg border px-3 py-2 text-sm",
                    c.id === target?.id
                      ? "border-ring bg-accent/50"
                      : "border-border"
                  )}
                >
                  <input
                    type="radio"
                    name="merge-target"
                    checked={c.id === target?.id}
                    onChange={() => setTargetId(c.id)}
                    className="accent-primary"
                  />
                  <span className="min-w-0 flex-1 truncate font-medium">
                    {c.name}
                  </span>
                  {c.domain ? (
                    <span className="text-2xs text-muted-foreground">
                      {c.domain}
                    </span>
                  ) : null}
                </label>
              ))}
            </div>
          </fieldset>
        ) : (
          <>
            <div className="grid gap-1.5">
              <Label>{t("target")}</Label>
              <p className="rounded-lg border border-border bg-muted px-3 py-2 text-sm font-medium">
                {target?.name}
              </p>
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="merge-source">{t("source")}</Label>
              <CompanyPicker
                value={resolvedSource}
                onChange={setSource}
                placeholder={pickerLabels.placeholder}
                noneLabel={pickerLabels.none}
                emptyLabel={pickerLabels.empty}
                excludeId={target?.id}
              />
            </div>
          </>
        )}
        {pair && resolvedSource ? (
          <p className="text-2xs text-muted-foreground">
            {t("source")}: {resolvedSource.name}
          </p>
        ) : null}

        {target && resolvedSource && differing.length > 0 ? (
          <fieldset className="grid gap-1.5">
            <legend className="text-sm font-medium">{t("fieldChoices")}</legend>
            <div className="overflow-x-auto rounded-2xl border border-border">
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b border-border bg-app-panel-muted text-left text-xs text-muted-foreground">
                    <th scope="col" className="px-3 py-2 font-medium">
                      {t("fieldChoices")}
                    </th>
                    <th scope="col" className="px-3 py-2 font-medium">
                      {t("keepTarget")}
                    </th>
                    <th scope="col" className="px-3 py-2 font-medium">
                      {t("keepSource")}
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {differing.map((f) => (
                    <tr
                      key={f}
                      className="border-b border-border/60 last:border-b-0"
                    >
                      <th scope="row" className="px-3 py-2 text-left font-medium">
                        {t(`fields.${f}`)}
                      </th>
                      {(["target", "source"] as const).map((side) => {
                        const company = side === "target" ? target : resolvedSource;
                        const value = fieldValue(company, f);
                        return (
                          <td key={side} className="px-3 py-2">
                            <label
                              className={cn(
                                "flex cursor-pointer items-start gap-2 rounded-lg border px-2 py-1.5",
                                choices[f] === side
                                  ? "border-ring bg-accent/50"
                                  : "border-transparent"
                              )}
                            >
                              <input
                                type="radio"
                                name={`merge-field-${f}`}
                                checked={choices[f] === side}
                                onChange={() =>
                                  setChoices((c) => ({ ...c, [f]: side }))
                                }
                                className="mt-0.5 accent-primary"
                              />
                              <span className="break-words">
                                {value || "—"}
                              </span>
                            </label>
                          </td>
                        );
                      })}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </fieldset>
        ) : null}

        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
      </div>

      <DialogFooter>
        <Button type="button" variant="outline" onClick={onClose}>
          {tc("cancel")}
        </Button>
        <Button
          type="button"
          disabled={pending || !target || !resolvedSource || target.id === resolvedSource.id}
          onClick={() => void submit()}
        >
          {pending ? t("submitting") : t("submit")}
        </Button>
      </DialogFooter>
    </DialogContent>
  );
}
