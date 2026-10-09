/**
 * 리드 후보 검토 순수 규칙 — 컴포넌트와 테스트가 공유한다.
 * §13.4: 이메일은 공개 출처가 확인돼도 발송 근거 pending 으로 시작하고
 * 이름 패턴 생성 주소는 금지 — 미검증 주소를 시각적으로 표시한다.
 */
import type { LeadProposalStatus } from "@/lib/api/research";

const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

export interface FlaggedEmail {
  /** proposed_data 안의 점 표기 경로 (예: contacts[0].email). */
  path: string;
  value: string;
}

function looksLikeEmailKey(key: string): boolean {
  return /email|e-mail|mail/i.test(key);
}

/** proposed_data 를 재귀로 훑어 이메일 형태 값을 모은다 — 깊이 제한으로 방어. */
export function findUnverifiedEmails(
  data: Record<string, unknown>,
  basePath = "",
  depth = 0
): FlaggedEmail[] {
  if (depth > 6) return [];
  const found: FlaggedEmail[] = [];
  for (const [key, value] of Object.entries(data)) {
    const path = basePath ? `${basePath}.${key}` : key;
    if (typeof value === "string") {
      if (EMAIL_RE.test(value) || (looksLikeEmailKey(key) && value.includes("@"))) {
        found.push({ path, value });
      }
    } else if (Array.isArray(value)) {
      value.forEach((item, i) => {
        if (typeof item === "string") {
          if (EMAIL_RE.test(item)) {
            found.push({ path: `${path}[${i}]`, value: item });
          }
        } else if (typeof item === "object" && item !== null) {
          found.push(
            ...findUnverifiedEmails(
              item as Record<string, unknown>,
              `${path}[${i}]`,
              depth + 1
            )
          );
        }
      });
    } else if (typeof value === "object" && value !== null) {
      found.push(
        ...findUnverifiedEmails(
          value as Record<string, unknown>,
          path,
          depth + 1
        )
      );
    }
  }
  return found;
}

/** 채택·거절 가능한 상태인가 — DDL CHECK 의 대기 계열 상태만 허용한다. */
export function canDecideProposal(status: LeadProposalStatus | string): boolean {
  return status === "pending" || status === "needs_review";
}

/** JSONB 값의 표시용 문자열 — 객체는 JSON 으로 직렬화한다. */
export function displayValue(value: unknown): string {
  if (value === null || value === undefined) return "—";
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}
