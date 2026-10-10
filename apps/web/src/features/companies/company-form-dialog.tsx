"use client";

/**
 * 기업 생성·수정 대화상자.
 * - 생성: POST /companies + Idempotency-Key (폼당 하나 — DESIGN.md).
 * - 수정: PATCH + If-Match(ETag). 412 VERSION_MISMATCH 이면 최신 값을 다시
 *   읽어 ConflictDiff 3-way 비교로 필드별 선택을 받는다 (§7.2).
 *   428(VERSION_PRECONDITION_REQUIRED)이면 ETag를 다시 읽어 재시도한다.
 */
import { useTranslations } from "next-intl";
import { useRef, useState, type FormEvent } from "react";

import {
  ConflictDiff,
  type ConflictChoice,
  type ConflictField,
} from "@/components/patterns/conflict-diff";
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
import { Textarea } from "@/components/ui/textarea";
import { OwnerSelect } from "@/features/companies/owner-select";
import {
  createCompany,
  getCompany,
  patchCompany,
  LIFECYCLE_STAGES,
  type Company,
  type CompanyCreated,
  type CreateCompanyInput,
  type LifecycleStage,
  type PatchCompanyInput,
} from "@/lib/api/companies";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import { readEtag } from "@/lib/api/mutate";

const COUNTRY_RE = /^[A-Z]{2}$/;

/** PATCH allowlist 필드 — 412 비교 열도 이 목록을 따른다. */
const EDIT_FIELDS = [
  "name",
  "domain",
  "website_url",
  "industry",
  "country_code",
  "lifecycle_stage",
  "description",
] as const;
type EditField = (typeof EDIT_FIELDS)[number];

interface FieldState {
  name: string;
  domain: string;
  website_url: string;
  industry: string;
  country_code: string;
  lifecycle_stage: string;
  owner_principal_id: string | null;
  description: string;
}

function toState(c?: Company): FieldState {
  return {
    name: c?.name ?? "",
    domain: c?.domain ?? "",
    website_url: c?.website_url ?? "",
    industry: c?.industry ?? "",
    country_code: c?.country_code ?? "",
    lifecycle_stage: c?.lifecycle_stage ?? "prospect",
    owner_principal_id: c?.owner_principal_id ?? null,
    description: c?.description ?? "",
  };
}

function toPatch(s: FieldState): PatchCompanyInput {
  return {
    name: s.name.trim(),
    domain: s.domain.trim() || null,
    website_url: s.website_url.trim() || null,
    industry: s.industry.trim() || null,
    country_code: s.country_code.trim() || null,
    lifecycle_stage: s.lifecycle_stage,
    description: s.description.trim() || null,
  };
}

function stateValue(s: FieldState, f: EditField): string {
  const v = s[f];
  return v == null ? "" : String(v);
}

export interface CompanyFormDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 있으면 수정 모드. */
  company?: Company;
  /** 수정 모드의 If-Match 값 (ETag 또는 version). */
  etag?: string | number;
  /** 생성·저장 성공 콜백 — 생성 응답의 duplicate_candidates 를 넘긴다. */
  onSaved?: (result: CompanyCreated | Company, mode: "created" | "updated") => void;
}

export function CompanyFormDialog({
  open,
  onOpenChange,
  company,
  etag,
  onSaved,
}: CompanyFormDialogProps) {
  const t = useTranslations("companies.form");
  const ts = useTranslations("status");
  const tc = useTranslations("common");
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      {/* 닫히면 언마운트되어 폼·멱등 키·충돌 상태가 리셋된다. */}
      <CompanyFormBody
        key={company?.id ?? "new"}
        company={company}
        etag={etag}
        onClose={() => onOpenChange(false)}
        onSaved={onSaved}
        t={t}
        ts={ts}
        tc={tc}
      />
    </Dialog>
  );
}

function CompanyFormBody({
  company,
  etag,
  onClose,
  onSaved,
  t,
  ts,
  tc,
}: {
  company?: Company;
  etag?: string | number;
  onClose: () => void;
  onSaved?: CompanyFormDialogProps["onSaved"];
  t: ReturnType<typeof useTranslations<"companies.form">>;
  ts: ReturnType<typeof useTranslations<"status">>;
  tc: ReturnType<typeof useTranslations<"common">>;
}) {
  const editing = company !== undefined;
  const [form, setForm] = useState<FieldState>(() => toState(company));
  // 412 시점의 base — 대화상자를 열 때 읽은 값.
  const baseRef = useRef<FieldState>(toState(company));
  const [currentEtag, setCurrentEtag] = useState<string | number | undefined>(
    () => etag ?? company?.version
  );
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [countryInvalid, setCountryInvalid] = useState(false);
  const [conflict, setConflict] = useState<{
    fields: ConflictField[];
    server: FieldState;
    serverEtag: string | number;
  } | null>(null);
  const [choices, setChoices] = useState<Record<string, ConflictChoice>>({});
  const idemRef = useRef<string>(idempotencyKey());

  const set = <K extends keyof FieldState>(k: K, v: FieldState[K]) =>
    setForm((f) => ({ ...f, [k]: v }));

  function validate(): boolean {
    const okCountry =
      form.country_code.trim() === "" ||
      COUNTRY_RE.test(form.country_code.trim());
    setCountryInvalid(!okCountry);
    return okCountry && form.name.trim().length > 0;
  }

  async function refreshConflict(): Promise<boolean> {
    /** 최신 서버 값으로 ConflictDiff를 연다. 실패 시 false. */
    try {
      const latest = await getCompany(company!.id);
      const server = toState(latest.data);
      const serverEtag = readEtag(latest) ?? latest.data.version;
      const fields: ConflictField[] = EDIT_FIELDS.map((f) => ({
        key: f,
        label: fieldLabel(f),
        base: stateValue(baseRef.current, f),
        server: stateValue(server, f),
        mine: stateValue(form, f),
      }));
      setConflict({ fields, server, serverEtag });
      setChoices({});
      return true;
    } catch {
      setError(t("error"));
      return false;
    }
  }

  function fieldLabel(f: EditField): string {
    switch (f) {
      case "name": return t("name");
      case "domain": return t("domain");
      case "website_url": return t("website");
      case "industry": return t("industry");
      case "country_code": return t("country");
      case "lifecycle_stage": return t("stage");
      case "description": return t("description");
    }
  }

  async function savePatch(patch: PatchCompanyInput, etagValue: string | number) {
    return patchCompany(company!.id, patch, {
      etag: etagValue,
      idempotencyKey: idemRef.current,
    });
  }

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    if (!validate()) return;
    setPending(true);
    setError(null);
    try {
      if (!editing) {
        const input: CreateCompanyInput = {
          name: form.name.trim(),
          domain: form.domain.trim() || undefined,
          website_url: form.website_url.trim() || undefined,
          industry: form.industry.trim() || undefined,
          country_code: form.country_code.trim() || undefined,
          owner_principal_id: form.owner_principal_id ?? undefined,
          description: form.description.trim() || undefined,
        };
        const res = await createCompany(input, {
          idempotencyKey: idemRef.current,
        });
        onSaved?.(res.data, "created");
        onClose();
      } else {
        const res = await savePatch(toPatch(form), currentEtag ?? company.version);
        onSaved?.(res.data, "updated");
        onClose();
      }
    } catch (err) {
      const apiErr = err instanceof ApiError ? err : null;
      if (apiErr?.status === 412 && editing) {
        // 동시 편집 충돌 — 저장 실패를 숨기지 않고 3-way 비교를 연다.
        await refreshConflict();
      } else if (apiErr?.status === 428 && editing) {
        // If-Match 누락 — 최신 ETag를 읽어 재시도 가능하게 한다.
        try {
          const latest = await getCompany(company.id);
          setCurrentEtag(readEtag(latest) ?? latest.data.version);
          setError(t("conflictReload"));
        } catch {
          setError(t("error"));
        }
      } else {
        setError(apiErr?.problem.detail ?? t("error"));
      }
      if (apiErr && apiErr.problem.retryable !== true && apiErr.status < 500) {
        idemRef.current = idempotencyKey();
      }
    } finally {
      setPending(false);
    }
  }

  /** ConflictDiff 선택값으로 patch를 만들어 최신 ETag로 저장한다. */
  async function applyConflict() {
    if (!conflict) return;
    setPending(true);
    setError(null);
    try {
      const merged: FieldState = { ...conflict.server };
      for (const f of EDIT_FIELDS) {
        if (choices[f] === "mine") Object.assign(merged, { [f]: form[f] });
      }
      const res = await savePatch(toPatch(merged), conflict.serverEtag);
      onSaved?.(res.data, "updated");
      onClose();
    } catch (err) {
      const apiErr = err instanceof ApiError ? err : null;
      if (apiErr?.status === 412) {
        // 또 바뀌었다 — 비교 기준을 다시 맞춘다.
        baseRef.current = conflict.server;
        await refreshConflict();
      } else {
        setError(apiErr?.problem.detail ?? t("conflictApplyError"));
      }
      if (apiErr && apiErr.problem.retryable !== true && apiErr.status < 500) {
        idemRef.current = idempotencyKey();
      }
    } finally {
      setPending(false);
    }
  }

  if (conflict) {
    return (
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{tc("conflict.title")}</DialogTitle>
          <DialogDescription>{tc("conflict.description")}</DialogDescription>
        </DialogHeader>
        <ConflictDiff
          fields={conflict.fields}
          choices={choices}
          onChoiceChange={(key, choice) =>
            setChoices((c) => ({ ...c, [key]: choice }))
          }
        />
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <DialogFooter>
          <Button type="button" variant="outline" onClick={onClose}>
            {tc("cancel")}
          </Button>
          <Button type="button" disabled={pending} onClick={() => void applyConflict()}>
            {pending ? t("saving") : tc("conflict.apply")}
          </Button>
        </DialogFooter>
      </DialogContent>
    );
  }

  return (
    <DialogContent>
      <DialogHeader>
        <DialogTitle>{editing ? t("editTitle") : t("createTitle")}</DialogTitle>
      </DialogHeader>
      <form onSubmit={onSubmit} className="flex flex-col gap-4">
        <div className="grid gap-1.5">
          <Label htmlFor="company-name">{t("name")}</Label>
          <Input
            id="company-name"
            required
            maxLength={300}
            value={form.name}
            onChange={(e) => set("name", e.target.value)}
            placeholder={t("namePlaceholder")}
          />
        </div>
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <div className="grid gap-1.5">
            <Label htmlFor="company-domain">{t("domain")}</Label>
            <Input
              id="company-domain"
              value={form.domain}
              onChange={(e) => set("domain", e.target.value)}
              placeholder={t("domainPlaceholder")}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="company-website">{t("website")}</Label>
            <Input
              id="company-website"
              type="url"
              value={form.website_url}
              onChange={(e) => set("website_url", e.target.value)}
              placeholder={t("websitePlaceholder")}
            />
          </div>
        </div>
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <div className="grid gap-1.5">
            <Label htmlFor="company-industry">{t("industry")}</Label>
            <Input
              id="company-industry"
              value={form.industry}
              onChange={(e) => set("industry", e.target.value)}
              placeholder={t("industryPlaceholder")}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="company-country">{t("country")}</Label>
            <Input
              id="company-country"
              value={form.country_code}
              onChange={(e) => set("country_code", e.target.value.toUpperCase())}
              placeholder={t("countryPlaceholder")}
              maxLength={2}
              aria-invalid={countryInvalid}
              aria-describedby="company-country-hint"
            />
            <p id="company-country-hint" className="text-2xs text-muted-foreground">
              {countryInvalid ? t("countryInvalid") : t("countryHint")}
            </p>
          </div>
        </div>
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <div className="grid gap-1.5">
            <Label htmlFor="company-stage">{t("stage")}</Label>
            <Select
              value={form.lifecycle_stage}
              onValueChange={(v) => set("lifecycle_stage", v)}
            >
              <SelectTrigger id="company-stage" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {LIFECYCLE_STAGES.map((s: LifecycleStage) => (
                  <SelectItem key={s} value={s}>
                    {ts.has(s) ? ts(s) : s}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="company-owner">{t("owner")}</Label>
            <OwnerSelect id="company-owner" value={form.owner_principal_id} onChange={(v) => set("owner_principal_id", v)} unassignedLabel={t("ownerUnassigned")} meLabel={t("ownerMe")} disabled={editing} />
            <p className="text-2xs text-muted-foreground">{editing ? t("ownerFixed") : t("ownerHint")}</p>
          </div>
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="company-description">{t("description")}</Label>
          <Textarea
            id="company-description"
            rows={3}
            value={form.description}
            onChange={(e) => set("description", e.target.value)}
            placeholder={t("descriptionPlaceholder")}
          />
        </div>
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <DialogFooter>
          <Button type="button" variant="outline" onClick={onClose}>
            {tc("cancel")}
          </Button>
          <Button type="submit" disabled={pending}>
            {pending ? t("saving") : editing ? t("save") : t("submit")}
          </Button>
        </DialogFooter>
      </form>
    </DialogContent>
  );
}
