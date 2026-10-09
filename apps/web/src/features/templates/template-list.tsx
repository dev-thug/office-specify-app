"use client";

import { useLocale, useTranslations } from "next-intl";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useMemo, useState, type FormEvent } from "react";
import { PlusIcon, SearchIcon } from "lucide-react";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { PageHeader } from "@/components/patterns/page-header";
import { Badge } from "@/components/ui/badge";
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
import {
  createEmailTemplate,
  EMAIL_TEMPLATES_PATH,
  type EmailTemplate,
} from "@/lib/api/templates";
import { useCursorList } from "@/lib/api/use-cursor-list";

import { useApiErrorMessage } from "./api-error-message";
import { formatDateTime } from "./format";
import { useIdempotencyKey } from "./use-idempotency-key";

export function TemplateList() {
  const t = useTranslations("templates.list");
  const errorMessage = useApiErrorMessage("templates");
  const locale = useLocale();
  const router = useRouter();
  const [query, setQuery] = useState("");
  const [createOpen, setCreateOpen] = useState(false);

  const list = useCursorList<EmailTemplate>({
    queryKey: ["templates", "list"],
    path: EMAIL_TEMPLATES_PATH,
  });

  const rows = useMemo(
    () => list.data?.pages.flatMap((p) => p.data) ?? [],
    [list.data]
  );

  // 검색은 불러온 페이지 범위에서 클라이언트 필터로 처리한다 (B.2에
  // 템플릿 검색 파라미터가 문서화되어 있지 않다).
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter((r) => r.name.toLowerCase().includes(q));
  }, [rows, query]);

  const lastPage = list.data?.pages[list.data.pages.length - 1];

  const columns: ColumnDef<EmailTemplate>[] = [
    {
      key: "name",
      header: t("colName"),
      cell: (row) => (
        <span className="flex items-center gap-2">
          <Link
            href={`/app/templates/${row.template_id}`}
            className="font-medium text-foreground underline-offset-4 hover:underline"
          >
            {row.name}
          </Link>
          {row.archived_at ? (
            <Badge variant="secondary">{t("archived")}</Badge>
          ) : null}
        </span>
      ),
    },
    {
      key: "version",
      header: t("colVersion"),
      className: "w-24",
      cell: (row) =>
        row.latest_version_no != null ? `v${row.latest_version_no}` : "—",
    },
    {
      key: "created",
      header: t("colCreated"),
      className: "w-44",
      cell: (row) => (
        <span className="text-muted-foreground">
          {formatDateTime(row.created_at, locale)}
        </span>
      ),
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader
        title={t("title")}
        description={t("description")}
        actions={
          <Button type="button" size="sm" onClick={() => setCreateOpen(true)}>
            <PlusIcon aria-hidden />
            {t("newTemplate")}
          </Button>
        }
      />

      <div className="relative max-w-sm">
        <SearchIcon
          aria-hidden
          className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground"
        />
        <Input
          type="search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t("searchPlaceholder")}
          aria-label={t("searchPlaceholder")}
          className="pl-9"
        />
      </div>

      <DataTable
        columns={columns}
        rows={filtered}
        rowKey={(row) => row.template_id}
        page={lastPage?.page}
        onLoadMore={() => list.fetchNextPage()}
        loadingMore={list.isFetchingNextPage}
        loading={list.isLoading}
        error={list.error}
        onRetry={() => list.refetch()}
        emptyState={
          <div className="flex flex-col items-center gap-1 py-8 text-center">
            <p className="text-sm font-medium">{t("emptyTitle")}</p>
            <p className="max-w-md text-sm text-muted-foreground">
              {t("emptyDescription")}
            </p>
          </div>
        }
      />

      <CreateTemplateDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        errorMessage={errorMessage}
        onCreated={(id) => {
          setCreateOpen(false);
          void list.refetch();
          router.push(`/app/templates/${id}`);
        }}
      />
    </div>
  );
}

function CreateTemplateDialog({
  open,
  onOpenChange,
  onCreated,
  errorMessage,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (id: string) => void;
  errorMessage: (err: unknown) => string;
}) {
  const t = useTranslations("templates.list");
  const tCommon = useTranslations("common");
  const [name, setName] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const idem = useIdempotencyKey();

  async function onSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setPending(true);
    setError(null);
    try {
      const res = await createEmailTemplate(
        { name: name.trim() },
        { idempotencyKey: idem.key() }
      );
      idem.settle();
      setName("");
      onCreated(res.data.template_id);
    } catch (err) {
      idem.settle(err);
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("createTitle")}</DialogTitle>
          <DialogDescription>{t("createDescription")}</DialogDescription>
        </DialogHeader>
        <form onSubmit={onSubmit} className="flex flex-col gap-4">
          <div className="grid gap-1.5">
            <Label htmlFor="template-name">{t("nameLabel")}</Label>
            <Input
              id="template-name"
              required
              maxLength={300}
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={t("namePlaceholder")}
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
              onClick={() => onOpenChange(false)}
            >
              {tCommon("cancel")}
            </Button>
            <Button type="submit" disabled={pending || !name.trim()}>
              {pending ? t("creating") : t("createSubmit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
