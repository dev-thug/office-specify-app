"use client";

import { useTranslations } from "next-intl";
import { useMemo, useRef, useState, type FormEvent } from "react";
import { PlusIcon, Trash2Icon, TriangleAlertIcon } from "lucide-react";
import { toast } from "sonner";

import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
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
import {
  createTemplateVersion,
  type CreateTemplateVersionInput,
  type TemplateVariableSchema,
} from "@/lib/api/templates";
import { ApiError } from "@/lib/api/problem";

import { EmailCanvas, type EmailCanvasHandle } from "./email-canvas";
import { cleanEmailHtml, DEFAULT_EMAIL_VARIABLES, emailHtmlToText, escapeEmailText, renderSampleEmail } from "./email-authoring";

import { useApiErrorMessage } from "./api-error-message";
import { useIdempotencyKey } from "./use-idempotency-key";

/** §8.1 — 변수명의 점은 등록된 식별자 일부. 허용 문자만 받는다. */
const VARIABLE_NAME = /^[A-Za-z_][A-Za-z0-9_.]*$/;
const PLACEHOLDER = /\{\{\s*([A-Za-z_][A-Za-z0-9_.]*)\s*\}\}/g;

export interface VariableRow {
  /** 로컬 행 키 — 서버 필드가 아니다. */
  key: string;
  name: string;
  type: "string" | "uri";
  required: boolean;
  maxLength: string;
  defaultValue?: TemplateVariableSchema["properties"][string]["default"];
}

export interface VersionEditorSeed {
  subject: string;
  bodyText: string;
  bodyHtml: string | null;
  variableSchema?: TemplateVariableSchema;
}

interface VersionEditorProps {
  templateId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (id: string) => void;
  initialSeed?: VersionEditorSeed | null;
  inline?: boolean;
}

function emailTextToHtml(value: string): string {
  return value
    .replace(/\r\n?/g, "\n")
    .split(/\n{2,}/)
    .filter(paragraph => paragraph.length > 0)
    .map(paragraph => "<p>" + paragraph.split("\n").map(escapeEmailText).join("<br>") + "</p>")
    .join("");
}

/** A new editor session starts blank, while an import seed initializes only that session. */
export function VersionEditor(props: VersionEditorProps) {
  let seedId = "empty";
  if (props.initialSeed) {
    let firstHash = 0x811c9dc5;
    let secondHash = 0x9e3779b9;
    const source = [props.initialSeed.subject, props.initialSeed.bodyText, props.initialSeed.bodyHtml ?? ""].join("\u0000");
    for (let index = 0; index < source.length; index += 1) {
      const code = source.charCodeAt(index);
      firstHash = Math.imul(firstHash ^ code, 0x01000193);
      secondHash = Math.imul(secondHash ^ code, 0x85ebca6b);
    }
    seedId = `${firstHash >>> 0}-${secondHash >>> 0}`;
  }
  return <VersionEditorForm key={seedId} {...props} />;
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
    if (row.defaultValue !== undefined) prop.default = row.defaultValue;
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

function rowsForSeed(seed: VersionEditorSeed): VariableRow[] {
  if (seed.variableSchema) {
    const required = new Set(seed.variableSchema.required ?? []);
    return Object.entries(seed.variableSchema.properties).map(([name, prop]) => ({
      ...newRow(), name, type: prop.format === "uri" ? "uri" : "string",
      maxLength: prop.maxLength == null ? "" : String(prop.maxLength),
      required: required.has(name), defaultValue: prop.default,
    }));
  }
  const names = new Set<string>();
  for (const text of [seed.subject, seed.bodyText, seed.bodyHtml ?? ""]) {
    for (const match of text.matchAll(PLACEHOLDER)) {
      if (match[1]) names.add(match[1]);
    }
  }
  return [...names].map((name) => {
    const preset = DEFAULT_EMAIL_VARIABLES.find((item) => item.name === name);
    return {
      ...newRow(),
      name,
      type: preset?.type ?? "string",
      maxLength: preset?.maxLength ?? "",
      required: true,
    };
  });
}

function VersionEditorForm({
  templateId,
  open,
  onOpenChange,
  onCreated,
  initialSeed,
  inline = false,
}: VersionEditorProps) {
  const t = useTranslations("templates.editor");
  const tCommon = useTranslations("common");
  const a = useTranslations("templates.authoring");
  const errorMessage = useApiErrorMessage("templates");
  const idem = useIdempotencyKey();

  const [subject, setSubject] = useState(initialSeed?.subject ?? "");
  const seededBodyText = initialSeed?.bodyText ?? "";
  const seededBodyHtml = initialSeed?.bodyHtml?.trim()
    ? initialSeed.bodyHtml
    : emailTextToHtml(seededBodyText);
  const [bodyText, setBodyText] = useState(seededBodyText);
  const [bodyHtml, setBodyHtml] = useState(seededBodyHtml);
  const [legacyTextDismissed, setLegacyTextDismissed] = useState(false);
  const legacyTextFallback =
    !legacyTextDismissed &&
    bodyHtml === seededBodyHtml &&
    seededBodyText.trim() &&
    (!initialSeed?.bodyHtml?.trim() ||
      (typeof document !== "undefined" && seededBodyText !== emailHtmlToText(seededBodyHtml)))
      ? seededBodyText
      : null;
  const [rows, setRows] = useState<VariableRow[]>(() => initialSeed ? rowsForSeed(initialSeed) : []);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fieldErrors, setFieldErrors] = useState<string[]>([]);
  const canvas = useRef<EmailCanvasHandle>(null);
  const subjectInput = useRef<HTMLInputElement>(null);
  const insertTarget = useRef<"subject" | "body">("body");
  const [samples, setSamples] = useState<Record<string, string>>({
    "company.name": a("sampleCompany"), "contact.name": a("sampleContact"),
    "sender.display_name": a("sampleSender"),
    "meeting.booking_url": "https://example.invalid/booking/sample",
    "compliance.unsubscribe_url": "https://example.invalid/unsubscribe/sample",
  });
  function registerPresets(...texts: string[]) {
    const used = new Set(texts.flatMap(text => [...text.matchAll(PLACEHOLDER)].map(match => match[1])));
    setRows(current => {
      const added = DEFAULT_EMAIL_VARIABLES.filter(preset => used.has(preset.name) && !current.some(row => row.name.trim() === preset.name));
      return added.length ? [...current, ...added.map(preset => ({...newRow(), name: preset.name, type: preset.type, maxLength: preset.maxLength, required: true}))] : current;
    });
  }
  function updateHtml(html: string) {
    setBodyHtml(html);
    setBodyText(emailHtmlToText(html));
    setLegacyTextDismissed(true);
    registerPresets(html);
  }
  function insertPreset(preset: typeof DEFAULT_EMAIL_VARIABLES[number]) {
    setRows(current => current.some(row => row.name.trim() === preset.name) ? current : [...current, {
      ...newRow(), name: preset.name, type: preset.type, maxLength: preset.maxLength, required: true,
    }]);
    const value = "{{" + preset.name + "}}";
    const input = insertTarget.current === "subject" ? subjectInput.current : null;
    if (input) {
      const start = input.selectionStart ?? input.value.length;
      const next = input.value.slice(0, start) + value + input.value.slice(input.selectionEnd ?? start);
      setSubject(next);
      input.focus();
      requestAnimationFrame(() => input.setSelectionRange(start + value.length, start + value.length));
    } else {
      canvas.current?.insertVariable(preset.name);
    }
  }
  function insertBookingCta() {
    const label = a("bookingCtaLabel");
    registerPresets("{{meeting.booking_url}}");
    canvas.current?.insertBookingCta(label);
  }
  const sampleSubject = subject.replace(/\{\{\s*([A-Za-z_][A-Za-z0-9_.]*)\s*\}\}/g, (token, name: string) => samples[name] ?? token);
  const sampleHtml = typeof document === "undefined" ? "" : renderSampleEmail(bodyHtml || "<p>" + escapeEmailText(bodyText).replaceAll("\n", "<br>") + "</p>", samples);


  // 본문 자리표시자 ↔ 스키마 정합 경고 (저장은 막지 않고 서버가 최종 검증한다).
  const outsideSchema = useMemo(
    () => variablesOutsideSchema(rows, subject, bodyText, bodyHtml),
    [rows, subject, bodyText, bodyHtml]
  );

  function validate(): boolean {
    const errs: string[] = [];
    if (!subject.trim()) errs.push(t("subjectRequired"));
    const savedBodyText = legacyTextFallback ?? emailHtmlToText(bodyHtml);
    if (!savedBodyText.trim()) errs.push(t("bodyRequired"));
    if (cleanEmailHtml(bodyHtml).removed) errs.push(a("unsupportedHtml"));
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
      body_text: legacyTextFallback ?? emailHtmlToText(bodyHtml),
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
      if (!inline) {
        setSubject("");
        setBodyText("");
        setBodyHtml("");
        setLegacyTextDismissed(false);
        setRows([]);
        onOpenChange(false);
      }
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

  const form = (
        <form onSubmit={onSubmit} className="flex min-h-0 flex-1 flex-col">
          <div data-slot="email-editor-scroll" className="flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto px-6 py-5">
          <div className="grid gap-1.5">
            <Label htmlFor="tv-subject">{t("subject")}</Label>
            <Input
              ref={subjectInput}
              onFocus={() => { insertTarget.current = "subject"; }}
              id="tv-subject"
              required
              value={subject}
              onChange={(e) => { setSubject(e.target.value); registerPresets(e.target.value); }}
              placeholder={t("subjectPlaceholder")}
            />
          </div>

          <div className="grid min-w-0 gap-5 lg:grid-cols-[minmax(0,1fr)_16rem]">
            <div className="flex min-w-0 flex-col gap-4">
              <EmailCanvas ref={canvas} html={bodyHtml} previewHtml={sampleHtml} previewSubject={sampleSubject}
                onChange={updateHtml} onFocus={() => { insertTarget.current = "body"; }} />
              {legacyTextFallback !== null ? (
                <Alert variant="warning">
                  <TriangleAlertIcon />
                  <AlertDescription className="grid gap-2">
                    <p>{a("legacyTextNotice")}</p>
                    <details>
                      <summary className="cursor-pointer text-xs font-medium">{a("legacyTextTitle")}</summary>
                      <p className="mt-2 text-2xs text-muted-foreground">{a("legacyTextDescription")}</p>
                      <pre className="mt-2 whitespace-pre-wrap rounded-lg bg-muted/40 p-3 text-xs">{legacyTextFallback}</pre>
                    </details>
                    <div>
                      <Button type="button" size="xs" variant="ghost" onClick={() => {
                        setBodyText(emailHtmlToText(bodyHtml));
                        setLegacyTextDismissed(true);
                      }}>{a("useGeneratedText")}</Button>
                    </div>
                  </AlertDescription>
                </Alert>
              ) : null}
            </div>
            <aside className="flex min-w-0 flex-col gap-5 rounded-xl border bg-muted/20 p-4">
              <div>
                <h3 className="text-sm font-medium">{a("presets")}</h3>
                <p className="mt-1 text-2xs text-muted-foreground">{a("presetsHint")}</p>
              </div>
              <div className="flex flex-col gap-2">
                {DEFAULT_EMAIL_VARIABLES.map(preset => <Button key={preset.name} type="button" variant="outline"
                  className="h-auto justify-start gap-2 py-2 text-left" onMouseDown={event => event.preventDefault()}
                  onClick={() => insertPreset(preset)} aria-label={a("insertPreset", {name: a(preset.label)})}>
                  <PlusIcon className="size-3.5 shrink-0" aria-hidden />
                  <span className="min-w-0"><span className="block text-xs">{a(preset.label)}</span><code className="block break-all text-2xs font-normal text-muted-foreground">{preset.name}</code></span>
                </Button>)}
              </div>
              <Button
                type="button"
                variant="outline"
                className="h-auto justify-start gap-2 py-2 text-left"
                onMouseDown={event => event.preventDefault()}
                onClick={insertBookingCta}
                aria-label={a("insertBookingCta")}
              >
                <PlusIcon className="size-3.5 shrink-0" aria-hidden />
                <span className="min-w-0">
                  <span className="block text-xs">{a("bookingCtaLabel")}</span>
                  <code className="block break-all text-2xs font-normal text-muted-foreground">
                    {'{{meeting.booking_url}}'}
                  </code>
                </span>
              </Button>
              <p className="text-2xs text-muted-foreground">{a("bookingCtaHint")}</p>
              <p className="text-2xs text-muted-foreground">{a("systemHint")}</p>
              <details className="border-t pt-3">
                <summary className="cursor-pointer text-xs font-medium">{a("sampleValues")}</summary>
                <p className="mt-2 text-2xs text-muted-foreground">{a("sampleOnly")}</p>
                <div className="mt-3 grid gap-3">
                  {DEFAULT_EMAIL_VARIABLES.filter(preset => !preset.system).map(preset => <div key={preset.name} className="grid gap-1">
                    <Label htmlFor={"sample-" + preset.label} className="text-xs">{a(preset.label)}</Label>
                    <Input id={"sample-" + preset.label} aria-label={a("sampleInput", {name: a(preset.label)})} value={samples[preset.name]}
                      onChange={event => setSamples(current => ({...current, [preset.name]: event.target.value}))} />
                  </div>)}
                </div>
              </details>
            </aside>
          </div>
          <details className="rounded-xl border px-4 py-3">
            <summary className="cursor-pointer text-sm font-medium">{t("variablesTitle")} ({rows.length})</summary>

          <fieldset className="mt-3 grid gap-2">
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
          </details>

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

          </div>
          <div className="flex shrink-0 justify-end gap-2 border-t bg-background px-6 py-4">
            {!inline ? (
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              {tCommon("cancel")}
            </Button>
            ) : null}
            <Button type="submit" disabled={pending}>
              {pending ? t("submitting") : t("submit")}
            </Button>
          </div>
        </form>
  );
  if (inline) return <section className="min-w-0 overflow-hidden rounded-2xl border bg-card">{form}</section>;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="flex max-h-[90dvh] flex-col gap-0 overflow-hidden p-0 sm:max-w-6xl">
        <DialogHeader className="shrink-0 border-b px-6 py-4">
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>{t("description")}</DialogDescription>
        </DialogHeader>
        {form}
      </DialogContent>
    </Dialog>
  );
}
