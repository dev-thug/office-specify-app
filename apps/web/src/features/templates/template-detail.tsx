"use client";

import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "@/lib/api/client";
import { useLocale, useTranslations } from "next-intl";
import Link from "next/link";
import { useMemo, useState } from "react";
import { ArrowLeftIcon, PlusIcon } from "lucide-react";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { PageHeader } from "@/components/patterns/page-header";
import { Alert, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  EMAIL_TEMPLATES_PATH,
  type TemplateVariableProp,
  type TemplateVersion,
  type TemplateVersionSummary,
} from "@/lib/api/templates";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { cn } from "@/lib/utils";

import { formatDateTime, shortHash } from "./format";
import { RenderPreview } from "./render-preview";
import { useEmailTemplate } from "./use-email-template";
import { VersionEditor } from "./version-editor";

export function TemplateDetail({ templateId }: { templateId: string }) {
  const t = useTranslations("templates.detail");
  const tCommon = useTranslations("common");
  const locale = useLocale();
  const template = useEmailTemplate(templateId);
  const [selectedVersionId, setSelectedVersionId] = useState<string | null>(
    null
  );
  const [editorOpen, setEditorOpen] = useState(false);

  const versions = useCursorList<TemplateVersionSummary>({
    queryKey: ["templates", "versions", templateId],
    path: `${EMAIL_TEMPLATES_PATH}/${encodeURIComponent(templateId)}/versions`,
    limit: 100,
  });

  const rows = useMemo(
    () => versions.data?.pages.flatMap((p) => p.data) ?? [],
    [versions.data]
  );

  const selectedId = rows.find((v) => v.template_version_id === selectedVersionId)?.template_version_id
    ?? rows[0]?.template_version_id;
  const content = useQuery({
    queryKey: ["templates", "version", selectedId],
    enabled: Boolean(selectedId),
    queryFn: async () => (await apiFetch<TemplateVersion>(`/api/v1/template-versions/${selectedId}`)).data,
  });
  const selected = content.data ?? null;

  if (template.isError) {
    return (
      <div className="mx-auto w-full max-w-6xl px-6 py-6">
        <ErrorState error={template.error} onRetry={() => template.refetch()} />
      </div>
    );
  }

  if (template.data === null) {
    return (
      <div className="mx-auto w-full max-w-6xl px-6 py-6">
        <EmptyState
          title={t("notFoundTitle")}
          description={t("notFoundDescription")}
          action={
            <Button asChild variant="outline" size="sm">
              <Link href="/app/templates">{t("back")}</Link>
            </Button>
          }
        />
      </div>
    );
  }

  const archived = Boolean(template.data?.archived_at);

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <div>
        <Link
          href="/app/templates"
          className="inline-flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeftIcon className="size-4" aria-hidden />
          {t("back")}
        </Link>
      </div>

      <PageHeader
        title={template.data?.name ?? "…"}
        description={t("immutableNote")}
        actions={
          <Button
            type="button"
            size="sm"
            disabled={archived}
            onClick={() => setEditorOpen(true)}
          >
            <PlusIcon aria-hidden />
            {t("newVersion")}
          </Button>
        }
      />

      {archived ? (
        <Alert variant="warning">
          <AlertTitle>{t("archivedBanner")}</AlertTitle>
        </Alert>
      ) : null}

      <div className="grid gap-5 lg:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]">
        {/* 버전 목록 — 불변이며 행 선택으로 상세를 바꾼다. */}
        <section
          aria-label={t("versionsTitle")}
          className="flex flex-col gap-2"
        >
          <h2 className="text-sm font-medium">{t("versionsTitle")}</h2>
          {versions.isLoading ? (
            <div className="flex flex-col gap-2">
              {Array.from({ length: 3 }).map((_, i) => (
                <Skeleton key={i} className="h-14 w-full rounded-lg" />
              ))}
            </div>
          ) : versions.isError ? (
            <ErrorState
              error={versions.error}
              onRetry={() => versions.refetch()}
            />
          ) : rows.length === 0 ? (
            <EmptyState
              compact
              title={t("emptyVersionsTitle")}
              description={t("emptyVersionsDescription")}
            />
          ) : (
            <ul className="flex flex-col gap-2">
              {rows.map((v) => (
                <li key={v.template_version_id}>
                  <button
                    type="button"
                    onClick={() => setSelectedVersionId(v.template_version_id)}
                    aria-pressed={selectedId === v.template_version_id}
                    className={cn(
                      "flex w-full items-center justify-between gap-3 rounded-lg border px-3.5 py-2.5 text-left text-sm transition-colors",
                      selectedId === v.template_version_id
                        ? "border-ring bg-accent/50"
                        : "border-border bg-card hover:bg-muted/60"
                    )}
                  >
                    <span className="flex items-center gap-2 font-medium">
                      {t("versionLabel", { no: v.version_no })}
                    </span>
                    <span className="text-2xs text-muted-foreground">
                      {v.content_hash ? shortHash(v.content_hash) : ""}
                      {" · "}
                      {formatDateTime(v.created_at, locale)}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          {versions.hasNextPage ? (
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={versions.isFetchingNextPage}
              onClick={() => versions.fetchNextPage()}
            >
              {tCommon("table.loadMore")}
            </Button>
          ) : null}
        </section>

        {/* 선택한 버전 상세 + 미리보기 */}
        <div className="flex min-w-0 flex-col gap-5">
          {content.isError ? (
            <ErrorState error={content.error} onRetry={() => content.refetch()} />
          ) : selectedId && content.isPending ? (
            <Skeleton className="h-48 rounded-2xl" />
          ) : selected ? (
            <VersionDetail version={selected} />
          ) : (
            <EmptyState compact title={t("emptyVersionsTitle")} />
          )}
          <RenderPreview version={selected} />
        </div>
      </div>

      <VersionEditor
        templateId={templateId}
        open={editorOpen}
        onOpenChange={setEditorOpen}
        onCreated={(id) => { setSelectedVersionId(id); void versions.refetch(); }}
      />
    </div>
  );
}

function VersionDetail({ version }: { version: TemplateVersion }) {
  const t = useTranslations("templates.detail");
  const props = version.variable_schema?.properties ?? {};
  const required = new Set(version.variable_schema?.required ?? []);
  const entries = Object.entries(props);

  return (
    <section className="flex flex-col gap-4 rounded-2xl border border-border bg-card px-4 py-4">
      <div className="grid gap-1.5">
        <h3 className="text-xs font-medium text-muted-foreground">
          {t("subject")}
        </h3>
        <p className="text-sm font-medium">{version.subject}</p>
      </div>

      <div className="grid gap-1.5">
        <h3 className="text-xs font-medium text-muted-foreground">
          {t("bodyText")}
        </h3>
        <pre className="max-h-64 overflow-auto rounded-lg bg-app-panel-muted p-3 text-sm whitespace-pre-wrap">
          {version.body_text}
        </pre>
      </div>

      <div className="grid gap-1.5">
        <h3 className="text-xs font-medium text-muted-foreground">
          {t("bodyHtml")}
        </h3>
        {version.body_html ? (
          <pre className="max-h-64 overflow-auto rounded-lg bg-app-panel-muted p-3 text-2xs whitespace-pre-wrap">
            {version.body_html}
          </pre>
        ) : (
          <p className="text-sm text-muted-foreground">{t("noHtmlBody")}</p>
        )}
      </div>

      <div className="grid gap-1.5">
        <h3 className="text-xs font-medium text-muted-foreground">
          {t("variablesTitle")}
        </h3>
        {entries.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("noVariables")}</p>
        ) : (
          <ul className="flex flex-col gap-1.5">
            {entries.map(([name, prop]) => (
              <VariableRow
                key={name}
                name={name}
                prop={prop}
                required={required.has(name)}
              />
            ))}
          </ul>
        )}
      </div>

      {version.content_hash ? (
        <p className="text-2xs text-muted-foreground">
          {t("contentHash")}: {version.content_hash}
        </p>
      ) : null}
    </section>
  );
}

function VariableRow({
  name,
  prop,
  required,
}: {
  name: string;
  prop: TemplateVariableProp;
  required: boolean;
}) {
  const t = useTranslations("templates.detail");
  return (
    <li className="flex flex-wrap items-center gap-2 rounded-lg border border-border px-3 py-2 text-sm">
      <code className="font-mono text-ui font-medium">{name}</code>
      <Badge variant={required ? "info-subtle" : "secondary"}>
        {required ? t("varRequired") : t("varOptional")}
      </Badge>
      {prop.type ? (
        <span className="text-2xs text-muted-foreground">{prop.type}</span>
      ) : null}
      {prop.maxLength != null ? (
        <span className="text-2xs text-muted-foreground">
          {t("varMaxLength", { count: prop.maxLength })}
        </span>
      ) : null}
      {prop.format ? (
        <span className="text-2xs text-muted-foreground">
          {t("varFormat", { format: prop.format })}
        </span>
      ) : null}
      {prop.default !== undefined && prop.default !== null ? (
        <span className="text-2xs text-muted-foreground">
          {t("varDefault", { value: String(prop.default) })}
        </span>
      ) : null}
    </li>
  );
}
