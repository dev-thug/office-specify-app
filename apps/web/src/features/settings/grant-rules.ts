/**
 * 메일함 grant 의존 규칙 (§5.4 + DDL CHECK).
 * compose→read, send→compose, approve→send, assign→read.
 * 켜기는 조상을 자동으로 켜고, 끄기는 자손을 자동으로 끈다.
 * 에이전트의 can_send 는 사람이 승인한 메일에만 쓸 수 있고, can_approve 는 부여할 수 없다.
 */
import type { GrantBooleans, Principal } from "@/lib/api/settings";

export type GrantKey = keyof GrantBooleans;

const REQUIRES: Record<GrantKey, readonly GrantKey[]> = {
  can_read: [],
  can_compose: ["can_read"],
  can_send: ["can_compose"],
  can_approve: ["can_send"],
  can_assign: ["can_read"],
};

/** key 의 모든 조상(직접·간접 의존) — can_approve → send/compose/read. */
export function grantAncestors(key: GrantKey): GrantKey[] {
  const out: GrantKey[] = [];
  const queue = [...REQUIRES[key]];
  while (queue.length) {
    const k = queue.shift()!;
    if (out.includes(k)) continue;
    out.push(k);
    queue.push(...REQUIRES[k]);
  }
  return out;
}

/** key 에 (직접·간접) 의존하는 자손 — can_read → compose/send/approve/assign. */
export function grantDescendants(key: GrantKey): GrantKey[] {
  const keys = Object.keys(REQUIRES) as GrantKey[];
  const out: GrantKey[] = [];
  let changed = true;
  while (changed) {
    changed = false;
    for (const k of keys) {
      if (out.includes(k) || k === key) continue;
      if (REQUIRES[k].some((req) => req === key || out.includes(req))) {
        out.push(k);
        changed = true;
      }
    }
  }
  return out;
}

/** 토글 하나를 적용한 새 grant — DDL CHECK 를 만족하는 결과만 반환한다. */
export function applyGrantChange(
  grant: GrantBooleans,
  key: GrantKey,
  value: boolean
): GrantBooleans {
  const next: GrantBooleans = { ...grant, [key]: value };
  if (value) {
    for (const a of grantAncestors(key)) next[a] = true;
  } else {
    for (const d of grantDescendants(key)) next[d] = false;
  }
  return next;
}

/** 부여 불가 사유 — "agent"=에이전트 정책 위반, "role"=역할 부족, null=허용. */
export function grantBlockReason(
  principal: Pick<Principal, "kind" | "role">,
  key: GrantKey
): "agent" | "role" | null {
  if (principal.kind === "agent") {
    return key === "can_approve" ? "agent" : null;
  }
  if (key === "can_approve") {
    return principal.role === "admin" || principal.role === "manager" ? null : "role";
  }
  return null;
}

/** 에이전트·비관리 역할에는 부여 불가한 스위치를 비활성화한다. */
export function grantToggleDisabled(
  principal: Pick<Principal, "kind" | "role">,
  key: GrantKey
): boolean {
  return grantBlockReason(principal, key) !== null;
}
