/**
 * 가져오기 상태·CSV 순수 규칙 — 컴포넌트와 테스트가 공유한다.
 * import_jobs.status CHECK ('uploaded','validating','ready','committing',
 * 'completed','failed','canceled') 기준.
 */
import type {
  ImportJob,
  ImportRow,
  ImportStatus,
} from "@/lib/api/imports";

/** 진행 중 상태 — 상세 화면 자동 갱신 대상. */
export const ACTIVE_IMPORT_STATUSES: ReadonlySet<string> = new Set([
  "uploaded",
  "validating",
  "committing",
]);

export function isActiveImportStatus(status: ImportStatus | string): boolean {
  return ACTIVE_IMPORT_STATUSES.has(status);
}

/**
 * 커밋 가능 조건 — dry-run 검증이 끝나(ready) validation_hash 가 있어야 한다.
 * commit 은 스냅샷 해시로 검증 결과가 그대로인지 확인한다 (B.3.13).
 */
export function canCommitImport(
  job: Pick<ImportJob, "status" | "validation_hash">
): boolean {
  return job.status === "ready" && Boolean(job.validation_hash);
}

// ── 매핑 대상 필드 ──────────────────────────────────────────

/** wizard.fields 메시지 키와 1:1 — 매핑 대상 CRM 필드 목록. */
export const IMPORT_TARGET_FIELDS = [
  "company.name",
  "company.domain",
  "company.website_url",
  "company.industry",
  "company.country_code",
  "company.description",
  "contact.name",
  "contact.job_title",
  "contact.department",
  "contact.email",
] as const;

/** CSV 헤더 → 대상 필드 기본 추측 — 사용자가 바꿀 수 있는 초기값일 뿐이다. */
export function guessTargetField(header: string): string {
  const h = header.trim().toLowerCase();
  if (!h) return "";
  if (/e-?mail|메일/.test(h)) return "contact.email";
  if (/domain|도메인/.test(h)) return "company.domain";
  if (/url|site|웹사이트|홈페이지/.test(h)) return "company.website_url";
  if (/industry|산업|업종/.test(h)) return "company.industry";
  if (/country|국가/.test(h)) return "company.country_code";
  if (/desc|설명|소개/.test(h)) return "company.description";
  if (/title|직함|직책|position|role/.test(h)) return "contact.job_title";
  if (/dept|부서|department/.test(h)) return "contact.department";
  if (/company|회사|organization/.test(h)) return "company.name";
  if (/name|이름|담당/.test(h)) return "contact.name";
  return "";
}

// ── CSV 파싱 (클라이언트 샘플·헤더용) ─────────────────────────

export interface CsvSample {
  headers: string[];
  rows: string[][];
}

/**
 * RFC 4180 따른 최소 CSV 파서 — 업로드 전 헤더와 샘플 미리보기용.
 * 서버의 검증(dry-run)을 대신하지 않는다. 잘린 입력도 부분 파싱한다.
 */
export function parseCsvSample(text: string, maxRows = 5): CsvSample {
  const parsed: string[][] = [];
  let field = "";
  let row: string[] = [];
  let inQuotes = false;

  const pushField = () => {
    row.push(field);
    field = "";
  };
  const pushRow = () => {
    pushField();
    // 완전히 빈 줄은 무시한다.
    if (row.length > 1 || row[0] !== "") parsed.push(row);
    row = [];
  };

  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (inQuotes) {
      if (ch === '"') {
        if (text[i + 1] === '"') {
          field += '"';
          i++;
        } else {
          inQuotes = false;
        }
      } else {
        field += ch;
      }
    } else if (ch === '"') {
      inQuotes = true;
    } else if (ch === ",") {
      pushField();
    } else if (ch === "\n") {
      pushRow();
      if (parsed.length >= maxRows + 1) break;
    } else if (ch !== "\r") {
      field += ch;
    }
  }
  if (field !== "" || row.length > 0) pushRow();

  const headers = parsed.shift() ?? [];
  return { headers, rows: parsed.slice(0, maxRows) };
}

// ── CSV 생성 (실패 행 다시 입력용) ────────────────────────────

/** 수식 주입 위험 선행문자 — 내보낼 때 앞에 ' 를 붙인다 (§15.1). */
const FORMULA_PREFIX_RE = /^[=+\-@\t\r]/;

export function csvCell(value: string): string {
  let v = value;
  if (FORMULA_PREFIX_RE.test(v)) v = `'${v}`;
  if (/[",\n\r]/.test(v)) v = `"${v.replace(/"/g, '""')}"`;
  return v;
}

export function rowsToCsv(rows: readonly (readonly string[])[]): string {
  return `${rows.map((r) => r.map(csvCell).join(",")).join("\r\n")}\r\n`;
}

/**
 * 실패·건너뜀 행을 재입력용 CSV 로 만든다 — normalized_data 의 키 합집합을
 * 열로 쓰고 상태·오류 열을 뒤에 붙인다. 현재 불러온 행만 포함한다.
 */
export function failedRowsCsv(rows: ImportRow[]): string {
  const cols: string[] = [];
  for (const r of rows) {
    for (const k of Object.keys(r.normalized_data ?? {})) {
      if (!cols.includes(k)) cols.push(k);
    }
  }
  const header = [...cols, "__status", "__errors"];
  const body = rows.map((r) => [
    ...cols.map((c) => {
      const v = r.normalized_data?.[c];
      return v === null || v === undefined ? "" : String(v);
    }),
    r.status,
    (r.errors ?? [])
      .map((e) => [e.field, e.code, e.detail ?? e.message].filter(Boolean).join(": "))
      .join("; "),
  ]);
  return rowsToCsv([header, ...body]);
}
