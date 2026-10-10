"use client";

import { useTranslations } from "next-intl";
import { useMemo, useState } from "react";
import { TriangleAlertIcon } from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  renderTemplateVersion,
  type RenderResult,
  type TemplateVersion,
} from "@/lib/api/templates";
import { ApiError } from "@/lib/api/problem";

import { DEFAULT_EMAIL_VARIABLES } from "./email-authoring";
import { useApiErrorMessage } from "./api-error-message";

const SYSTEM_EMAIL_VARIABLE_NAMES = new Set<string>(
  DEFAULT_EMAIL_VARIABLES.filter((variable) => variable.system).map((variable) => variable.name)
);

/** 스키마의 각 변수에 샘플 값을 채운 JSON 문자열을 만든다. */
export function sampleVariablesJson(version: TemplateVersion | null): string {
  const props = version?.variable_schema?.properties ?? {};
  const out: Record<string, unknown> = {};
  for (const [name, prop] of Object.entries(props)) {
    if (SYSTEM_EMAIL_VARIABLE_NAMES.has(name)) continue;
    if (prop.default !== undefined && prop.default !== null) {
      out[name] = prop.default;
    } else if (prop.format === "uri" || prop.type === "url") {
      out[name] = "https://example.com/sample";
    } else if (prop.type === "number") {
      out[name] = 1;
    } else if (prop.type === "boolean") {
      out[name] = true;
    } else {
      out[name] = `sample-${name}`;
    }
  }
  return JSON.stringify(out, null, 2);
}

export function RenderPreview({ version }: { version: TemplateVersion | null }) {
  const t = useTranslations("templates.preview");
  const errorMessage = useApiErrorMessage("templates");
  const [json, setJson] = useState(() => sampleVariablesJson(version));
  const [result, setResult] = useState<RenderResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  // 버전이 바뀌면 샘플 데이터를 스키마 기반으로 다시 채운다
  // (렌더 단계 상태 조정 — React 권장 "adjusting state when props change").
  const [prevVersion, setPrevVersion] = useState(version);
  if (prevVersion !== version) {
    setPrevVersion(version);
    setJson(sampleVariablesJson(version));
    setResult(null);
    setError(null);
  }

  const disabled = !version || pending;

  async function run() {
    if (!version) return;
    setError(null);
    let variables: Record<string, unknown>;
    try {
      const parsed: unknown = JSON.parse(json || "{}");
      if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
        throw new Error("not an object");
      variables = parsed as Record<string, unknown>;
    } catch {
      setError(t("invalidJson"));
      return;
    }
    setPending(true);
    try {
      const res = await renderTemplateVersion(version.template_version_id, { variables });
      setResult(res.data);
    } catch (err) {
      setResult(null);
      setError(errorMessage(err));
      if (err instanceof ApiError && err.problem.violations.length > 0) {
        // 필드별 violation 도 함께 경고 영역에 표시한다.
        setResult({
          warnings: err.problem.violations.map((v) => ({
            code: v.code,
            field: v.field,
            detail: v.detail,
          })),
        });
      }
    } finally {
      setPending(false);
    }
  }

  const warnings = useMemo(() => result?.warnings ?? [], [result]);
  const missing = useMemo(() => result?.missing_variables ?? [], [result]);

  return (
    <section
      aria-label={t("title")}
      className="flex flex-col gap-3 rounded-2xl border border-border bg-card px-4 py-4"
    >
      <div>
        <h2 className="text-sm font-medium">{t("title")}</h2>
        <p className="mt-0.5 text-2xs text-muted-foreground">
          {t("description")}
        </p>
      </div>

      {version ? (
        <>
          <div className="grid gap-1.5">
            <Label htmlFor="render-sample">{t("sampleData")}</Label>
            <Textarea
              id="render-sample"
              value={json}
              onChange={(e) => setJson(e.target.value)}
              rows={6}
              className="font-mono text-ui"
              spellCheck={false}
            />
          </div>
          <div>
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={disabled}
              onClick={run}
            >
              {pending ? t("running") : t("run")}
            </Button>
          </div>
        </>
      ) : (
        <p className="text-sm text-muted-foreground">{t("selectVersion")}</p>
      )}

      {error ? (
        <Alert variant="destructive">
          <TriangleAlertIcon />
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : null}

      {missing.length > 0 ? (
        <Alert variant="warning">
          <TriangleAlertIcon />
          <AlertTitle>{t("missingVariables")}</AlertTitle>
          <AlertDescription>
            <ul className="list-disc pl-4">
              {missing.map((m) => (
                <li key={m}>
                  <code className="font-mono text-ui">{m}</code>
                </li>
              ))}
            </ul>
          </AlertDescription>
        </Alert>
      ) : null}

      {warnings.length > 0 ? (
        <Alert variant="warning">
          <TriangleAlertIcon />
          <AlertTitle>{t("warnings")}</AlertTitle>
          <AlertDescription>
            <ul className="list-disc pl-4">
              {warnings.map((w, i) => (
                <li key={i}>
                  {typeof w === "string" ? w : <>{[w.field, w.code].filter(Boolean).join(" · ")}{" "}{w.detail ?? w.message ?? ""}</>}
                </li>
              ))}
            </ul>
          </AlertDescription>
        </Alert>
      ) : null}

      {result ? (
        <div className="flex flex-col gap-3">
          {result.subject !== undefined ? (
            <div className="grid gap-1">
              <h3 className="text-xs font-medium text-muted-foreground">
                {t("renderedSubject")}
              </h3>
              <p className="text-sm font-medium">{result.subject}</p>
            </div>
          ) : null}
          {result.body_text !== undefined ? (
            <div className="grid gap-1">
              <h3 className="text-xs font-medium text-muted-foreground">
                {t("renderedText")}
              </h3>
              <pre className="max-h-64 overflow-auto rounded-lg bg-app-panel-muted p-3 text-sm whitespace-pre-wrap">
                {result.body_text}
              </pre>
            </div>
          ) : null}
          {result.body_html ? (
            <div className="grid gap-1">
              <h3 className="text-xs font-medium text-muted-foreground">
                {t("renderedHtml")}
              </h3>
              {/* 서버가 렌더한 HTML은 스크립트 없는 샌드박스 프레임에서만 표시한다 */}
              <iframe
                sandbox=""
                title={t("renderedHtml")}
                srcDoc={result.body_html}
                className="h-48 w-full rounded-lg border border-border bg-card"
              />
              <p className="text-2xs text-muted-foreground">
                {t("htmlSandboxNote")}
              </p>
            </div>
          ) : null}
        </div>
      ) : (
        version && (
          <p className="text-sm text-muted-foreground">{t("emptyResult")}</p>
        )
      )}
    </section>
  );
}
