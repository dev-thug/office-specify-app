"use client";

import { useTranslations } from "next-intl";
import { useMemo, useState, type FormEvent } from "react";
import { PlusIcon, Trash2Icon, TriangleAlertIcon } from "lucide-react";
import { toast } from "sonner";

import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import {
  createTemplateVersion,
  type CreateTemplateVersionInput,
  type TemplateVariableSchema,
} from "@/lib/api/templates";
import { ApiError } from "@/lib/api/problem";

import { useApiErrorMessage } from "./api-error-message";
import { useIdempotencyKey } from "./use-idempotency-key";

/** §8.1 — 변수명의 점은 등록된 식별자 일부. 허용 문자만 받는다. */
const VARIABLE_NAME = /^[A-Za-z_][A-Za-z0-9_.]*$/;
const PLACEHOLDER = /\{\{\s*([A-Za-z_][A-Za-z0-9_.]*)\s*\}\}/g;

export interface VariableRow {
  /** 로컬 행 키 — 서버 필드가 아니다. */
  key: string;
  name: string;
  type: "string" | "number" | "boolean" | "uri";
  required: boolean;
  maxLength: string;
}

let rowSeq = 0;
const newRow = (): VariableRow => ({
  key: `v${++rowSeq}`,
  name: "",
  type: "string",
  required: false,
  maxLength: "",
});

/** 본문에 쓰였지만 스키마에 없는 변수명을 찾는다 (동결 전 검증 실패 예방). */
export function variablesOutsideSchema(
  rows: VariableRow[],
  ...texts: (string | null | undefined)[]
): string[] {
  const known = new Set(rows.map((r) => r.name.trim()).filter(Boolean));
  const used = new Set<string>();
  for (const text of texts) {
    if (!text) continue;
    for (const m of text.matchAll(PLACEHOLDER)) {
      const name = m[1];
      if (name) used.add(name);
    }
  }
  return [...used].filter((name) => !known.has(name));
}

export function buildVariableSchema(
  rows: VariableRow[]
): TemplateVariableSchema {
  const properties: TemplateVariableSchema["properties"] = {};
  const required: string[] = [];
  for (const row of rows) {
    const name = row.name.trim();
    if (!name) continue;
    const prop: NonNullable<
      TemplateVariableSchema["properties"][string]
    > = {};
    if (row.type === "uri") {
      prop.type = "string";
      prop.format = "uri";
    } else {
      prop.type = row.type;
    }
    const max = Number.parseInt(row.maxLength, 10);
    if (Number.isFinite(max) && max > 0) prop.maxLength = max;
    properties[name] = prop;
    if (row.required) required.push(name);
  }
  return {
    type: "object",
    properties,
    required,
    additionalProperties: false,
  };
}

export function VersionEditor({
  templateId,
  open,
  onOpenChange,
  onCreated,
}: {
  templateId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (id: string) => void;
}) {
  const t = useTranslations("templates.editor");
  const tCommon = useTranslations("common");
  const errorMessage = useApiErrorMessage("templates");
  const idem = useIdempotencyKey();

  const [subject, setSubject] = useState("");
  const [bodyText, setBodyText] = useState("");
  const [bodyHtml, setBodyHtml] = useState("");
  const [rows, setRows] = useState<VariableRow[]>([]);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fieldErrors, setFieldErrors] = useState<string[]>([]);

  // 본문 자리표시자 ↔ 스키마 정합 경고 (저장은 막지 않고 서버가 최종 검증한다).
  const outsideSchema = useMemo(
    () => variablesOutsideSchema(rows, subject, bodyText, bodyHtml),
    [rows, subject, bodyText, bodyHtml]
  );

  function validate(): boolean {
    const errs: string[] = [];
    if (!subject.trim()) errs.push(t("subjectRequired"));
    if (!bodyText.trim()) errs.push(t("bodyRequired"));
    const seen = new Set<string>();
    for (const row of rows) {
      const name = row.name.trim();
      if (!name) continue;
      if (!VARIABLE_NAME.test(name)) errs.push(t("invalidVariableName"));
      if (seen.has(name)) errs.push(t("duplicateVariable"));
      seen.add(name);
    }
    setFieldErrors(errs);
    return errs.length === 0;
  }

  async function onSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (!validate()) return;
    setPending(true);
    setError(null);
    const input: CreateTemplateVersionInput = {
      subject: subject.trim(),
      body_text: bodyText,
      body_html: bodyHtml.trim() ? bodyHtml : null,
      variable_schema: buildVariableSchema(rows),
    };
    try {
      const res = await createTemplateVersion(templateId, input, {
        idempotencyKey: idem.key(),
      });
      idem.settle();
      const no = res.data.version_no;
      toast.success(
        no != null ? t("createdToast", { no }) : t("createdToast", { no: "?" })
      );
      // 다음 버전 작성은 새 논리 작업 — 폼을 초기화한다.
      setSubject("");
      setBodyText("");
      setBodyHtml("");
      setRows([]);
      onOpenChange(false);
      onCreated(res.data.template_version_id);
    } catch (err) {
      idem.settle(err);
      setError(errorMessage(err));
      // 서버 필드 violation 을 함께 노출한다 (TEMPLATE_VARIABLE_MISSING 등).
      if (err instanceof ApiError && err.problem.violations.length > 0) {
        setFieldErrors(
          err.problem.violations.map((v) =>
            [v.field, v.code, v.detail].filter(Boolean).join(" · ")
          )
        );
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85dvh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>{t("description")}</DialogDescription>
        </DialogHeader>
        <form onSubmit={onSubmit} className="flex flex-col gap-4">
          <div className="grid gap-1.5">
            <Label htmlFor="tv-subject">{t("subject")}</Label>
            <Input
              id="tv-subject"
              required
              value={subject}
              onChange={(e) => setSubject(e.target.value)}
              placeholder={t("subjectPlaceholder")}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="tv-body-text">{t("bodyText")}</Label>
            <Textarea
              id="tv-body-text"
              required
              rows={6}
              value={bodyText}
              onChange={(e) => setBodyText(e.target.value)}
              placeholder={t("bodyTextPlaceholder")}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="tv-body-html">{t("bodyHtml")}</Label>
            <Textarea
              id="tv-body-html"
              rows={4}
              value={bodyHtml}
              onChange={(e) => setBodyHtml(e.target.value)}
              className="font-mono text-ui"
              spellCheck={false}
            />
            <p className="text-2xs text-muted-foreground">{t("bodyHtmlHint")}</p>
          </div>

          <fieldset className="grid gap-2">
            <legend className="text-sm font-medium">{t("variablesTitle")}</legend>
            <p className="text-2xs text-muted-foreground">
              {t("variablesHint")}
            </p>
            {rows.map((row) => (
              <div
                key={row.key}
                className="flex flex-wrap items-center gap-2 rounded-lg border border-border px-3 py-2"
              >
                <Input
                  aria-label={t("varName")}
                  value={row.name}
                  onChange={(e) =>
                    setRows((rs) =>
                      rs.map((r) =>
                        r.key === row.key ? { ...r, name: e.target.value } : r
                      )
                    )
                  }
                  placeholder={t("varNamePlaceholder")}
                  className="w-44 font-mono text-ui"
                />
                <Select
                  value={row.type}
                  onValueChange={(v) =>
                    setRows((rs) =>
                      rs.map((r) =>
                        r.key === row.key
                          ? { ...r, type: v as VariableRow["type"] }
                          : r
                      )
                    )
                  }
                >
                  <SelectTrigger
                    size="sm"
                    aria-label={t("varType")}
                    className="w-28"
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="string">string</SelectItem>
                    <SelectItem value="number">number</SelectItem>
                    <SelectItem value="boolean">boolean</SelectItem>
                    <SelectItem value="uri">uri</SelectItem>
                  </SelectContent>
                </Select>
                <Input
                  aria-label={t("varMaxLength")}
                  value={row.maxLength}
                  onChange={(e) =>
                    setRows((rs) =>
                      rs.map((r) =>
                        r.key === row.key
                          ? { ...r, maxLength: e.target.value }
                          : r
                      )
                    )
                  }
                  inputMode="numeric"
                  placeholder={t("varMaxLength")}
                  className="w-28"
                />
                <label className="flex items-center gap-1.5 text-sm">
                  <Checkbox
                    checked={row.required}
                    onCheckedChange={(v) =>
                      setRows((rs) =>
                        rs.map((r) =>
                          r.key === row.key
                            ? { ...r, required: v === true }
                            : r
                        )
                      )
                    }
                  />
                  {t("varRequired")}
                </label>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  aria-label={t("removeVariable")}
                  onClick={() =>
                    setRows((rs) => rs.filter((r) => r.key !== row.key))
                  }
                >
                  <Trash2Icon aria-hidden />
                </Button>
              </div>
            ))}
            <div>
              <Button
                type="button"
                variant="outline"
                size="xs"
                onClick={() => setRows((rs) => [...rs, newRow()])}
              >
                <PlusIcon aria-hidden />
                {t("addVariable")}
              </Button>
            </div>
          </fieldset>

          {outsideSchema.length > 0 ? (
            <Alert variant="warning">
              <TriangleAlertIcon />
              <AlertDescription>
                {t("unknownVariables", { names: outsideSchema.join(", ") })}
              </AlertDescription>
            </Alert>
          ) : null}

          {fieldErrors.length > 0 ? (
            <Alert variant="destructive">
              <TriangleAlertIcon />
              <AlertDescription>
                <ul className="list-disc pl-4">
                  {fieldErrors.map((e2) => (
                    <li key={e2}>{e2}</li>
                  ))}
                </ul>
              </AlertDescription>
            </Alert>
          ) : null}
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
            <Button type="submit" disabled={pending}>
              {pending ? t("submitting") : t("submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
