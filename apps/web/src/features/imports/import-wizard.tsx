"use client";

import { useQueryClient } from "@tanstack/react-query";
import { useTranslations } from "next-intl";
import { useRouter } from "next/navigation";
import { useRef, useState } from "react";
import { toast } from "sonner";

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
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  createImport,
  createUploadRequest,
  importJobId,
  uploadObject,
} from "@/lib/api/imports";

import {
  guessTargetField,
  IMPORT_TARGET_FIELDS,
  parseCsvSample,
} from "./import-state";

/** CSV 한도 — B.1.1 전역 입력 제한 (20MB·10만 행). */
const MAX_CSV_BYTES = 20 * 1024 * 1024;
/** 헤더·샘플 추출용으로 읽는 앞부분 크기 — 전체 파일은 업로드 시 사용한다. */
const SAMPLE_SLICE_BYTES = 256 * 1024;
/** "무시" 선택 값 — mapping 에서 제외된다. */
const IGNORE = "__ignore__";

type Step = "file" | "mapping" | "confirm";

/**
 * 가져오기 마법사 (UI11) — 파일 선택 → 필드 매핑 → 검증 시작.
 * dry-run(검증)까지만 요청하고 CRM 반영은 상세 화면의 커밋 단계에서 한다.
 */
export function ImportWizard({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useTranslations("imports.wizard");
  const tFields = useTranslations("imports.wizard.fields");
  const tCommon = useTranslations("common");
  const router = useRouter();
  const queryClient = useQueryClient();

  const [step, setStep] = useState<Step>("file");
  const [file, setFile] = useState<File | null>(null);
  const [headers, setHeaders] = useState<string[]>([]);
  const [sampleRows, setSampleRows] = useState<string[][]>([]);
  const [mapping, setMapping] = useState<Record<string, string>>({});
  const [duplicateStrategy, setDuplicateStrategy] = useState<"review" | "link">(
    "review"
  );
  const [pending, setPending] = useState<"uploading" | "starting" | null>(null);
  const [error, setError] = useState<string | null>(null);
  // 논리 작업마다 멱등 키를 보유한다 — 업로드 URL 발급과 검증 생성은 별개 작업.
  const uploadKeyRef = useRef<string>(idempotencyKey());
  const createKeyRef = useRef<string>(idempotencyKey());

  function reset() {
    setStep("file");
    setFile(null);
    setHeaders([]);
    setSampleRows([]);
    setMapping({});
    setDuplicateStrategy("review");
    setPending(null);
    setError(null);
    uploadKeyRef.current = idempotencyKey();
    createKeyRef.current = idempotencyKey();
  }

  function handleOpenChange(next: boolean) {
    if (!next) reset();
    onOpenChange(next);
  }

  async function onFileChosen(f: File | null) {
    setError(null);
    if (!f) {
      setFile(null);
      setHeaders([]);
      setSampleRows([]);
      return;
    }
    const isCsv = /\.csv$/i.test(f.name) || f.type === "text/csv";
    if (!isCsv) {
      setFile(null);
      setError(t("fileTypeError"));
      return;
    }
    if (f.size > MAX_CSV_BYTES) {
      setFile(null);
      setError(t("fileTooLarge"));
      return;
    }
    const sample = parseCsvSample(
      await f.slice(0, SAMPLE_SLICE_BYTES).text(),
      5
    );
    if (sample.headers.length === 0) {
      setFile(null);
      setError(t("noHeaders"));
      return;
    }
    setFile(f);
    setHeaders(sample.headers);
    setSampleRows(sample.rows);
    // 헤더명으로 초기 매핑을 추측한다 — 사용자가 다음 단계에서 고친다.
    const guessed: Record<string, string> = {};
    for (const h of sample.headers) guessed[h] = guessTargetField(h) || IGNORE;
    setMapping(guessed);
  }

  async function onStart() {
    if (!file) {
      setError(t("fileRequired"));
      return;
    }
    const cleanMapping = Object.fromEntries(
      headers
        .map((h) => [h, mapping[h] ?? IGNORE] as const)
        .filter(([, target]) => target !== IGNORE)
    );
    if (Object.keys(cleanMapping).length === 0) {
      setError(t("mappingRequired"));
      return;
    }
    setPending("uploading");
    setError(null);
    try {
      const upload = await createUploadRequest(
        {
          filename: file.name,
          media_type: file.type || "text/csv",
          byte_size: file.size,
        },
        { idempotencyKey: uploadKeyRef.current }
      );
      const stored = await uploadObject(upload.data, file);
      setPending("starting");
      const job = await createImport(
        {
          stored_object_id: stored.object_id,
          mapping: {
            columns: cleanMapping,
            has_header: true,
            duplicate_strategy: duplicateStrategy,
          },
        },
        { idempotencyKey: createKeyRef.current }
      );
      createKeyRef.current = idempotencyKey();
      toast.success(t("started"));
      await queryClient.invalidateQueries({ queryKey: ["imports"] });
      handleOpenChange(false);
      router.push(`/app/imports/${importJobId(job.data)}`);
    } catch (err) {
      const apiErr = err instanceof ApiError ? err : null;
      setError(
        apiErr?.problem.detail ??
          (pending === "uploading" ? t("uploadFailed") : t("createFailed"))
      );
      if (apiErr && apiErr.problem.retryable !== true && apiErr.status < 500) {
        uploadKeyRef.current = idempotencyKey();
        createKeyRef.current = idempotencyKey();
      }
    } finally {
      setPending(null);
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>
            {step === "file"
              ? t("stepFile")
              : step === "mapping"
                ? t("stepMapping")
                : t("stepConfirm")}
          </DialogDescription>
        </DialogHeader>

        {step === "file" ? (
          <div className="grid gap-4">
            <div className="grid gap-1.5">
              <Label htmlFor="import-file">{t("chooseFile")}</Label>
              <Input
                id="import-file"
                type="file"
                accept=".csv,text/csv"
                onChange={(e) => void onFileChosen(e.target.files?.[0] ?? null)}
              />
              <p className="text-2xs text-muted-foreground">{t("fileHint")}</p>
            </div>
            {headers.length > 0 ? (
              <div className="grid gap-1.5">
                <p className="text-sm font-medium">{t("sample")}</p>
                <div className="overflow-x-auto rounded-2xl border border-border">
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="border-b border-border bg-app-panel-muted text-left">
                        {headers.map((h) => (
                          <th
                            key={h}
                            scope="col"
                            className="px-3 py-2 text-xs font-medium text-muted-foreground"
                          >
                            {h}
                          </th>
                        ))}
                      </tr>
                    </thead>
                    <tbody>
                      {sampleRows.map((row, i) => (
                        <tr
                          key={i}
                          className="border-b border-border/60 last:border-b-0"
                        >
                          {headers.map((_, c) => (
                            <td key={c} className="px-3 py-2">
                              {row[c] ?? ""}
                            </td>
                          ))}
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
                <p className="text-2xs text-muted-foreground">
                  {t("sampleNote", { count: sampleRows.length })}
                </p>
              </div>
            ) : null}
          </div>
        ) : null}

        {step === "mapping" ? (
          <div className="grid gap-3">
            <p className="text-sm text-muted-foreground">{t("mappingNote")}</p>
            <div className="grid max-h-72 gap-2 overflow-y-auto pe-1">
              {headers.map((h) => (
                <div
                  key={h}
                  className="grid grid-cols-[1fr_1fr] items-center gap-3"
                >
                  <span className="truncate font-mono text-ui" title={h}>
                    {h}
                  </span>
                  <Select
                    value={mapping[h] ?? IGNORE}
                    onValueChange={(v) =>
                      setMapping((m) => ({ ...m, [h]: v }))
                    }
                  >
                    <SelectTrigger
                      aria-label={`${t("targetField")}: ${h}`}
                      size="sm"
                    >
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value={IGNORE}>{t("ignore")}</SelectItem>
                      {IMPORT_TARGET_FIELDS.map((field) => (
                        <SelectItem key={field} value={field}>
                          {tFields.has(field) ? tFields(field) : field}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
              ))}
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="dup-strategy">{t("duplicateStrategy")}</Label>
              <Select
                value={duplicateStrategy}
                onValueChange={(v) =>
                  setDuplicateStrategy(v as "review" | "link")
                }
              >
                <SelectTrigger id="dup-strategy" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="review">{t("duplicateReview")}</SelectItem>
                  <SelectItem value="link">{t("duplicateLink")}</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
        ) : null}

        {step === "confirm" ? (
          <div className="grid gap-3">
            <dl className="grid grid-cols-[7rem_1fr] gap-x-3 gap-y-1.5 rounded-2xl border border-border bg-app-panel-muted px-4 py-3 text-sm">
              <dt className="text-muted-foreground">{t("csvColumn")}</dt>
              <dd className="font-medium">{file?.name}</dd>
              <dt className="text-muted-foreground">{t("targetField")}</dt>
              <dd>
                {Object.values(mapping).filter((v) => v !== IGNORE).length} /{" "}
                {headers.length}
              </dd>
            </dl>
            <p className="text-sm text-muted-foreground">{t("confirmNote")}</p>
            <p className="text-sm text-muted-foreground">{t("basisNote")}</p>
          </div>
        ) : null}

        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}

        <DialogFooter>
          {step === "file" ? (
            <Button
              type="button"
              variant="outline"
              onClick={() => handleOpenChange(false)}
            >
              {tCommon("cancel")}
            </Button>
          ) : (
            <Button
              type="button"
              variant="outline"
              disabled={pending !== null}
              onClick={() =>
                setStep(step === "confirm" ? "mapping" : "file")
              }
            >
              {t("back")}
            </Button>
          )}
          {step === "file" ? (
            <Button
              type="button"
              disabled={!file || headers.length === 0}
              onClick={() => {
                if (!file) setError(t("fileRequired"));
                else setStep("mapping");
              }}
            >
              {t("next")}
            </Button>
          ) : null}
          {step === "mapping" ? (
            <Button type="button" onClick={() => setStep("confirm")}>
              {t("next")}
            </Button>
          ) : null}
          {step === "confirm" ? (
            <Button
              type="button"
              disabled={pending !== null}
              onClick={() => void onStart()}
            >
              {pending === "uploading"
                ? t("uploading")
                : pending === "starting"
                  ? t("starting")
                  : t("start")}
            </Button>
          ) : null}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
