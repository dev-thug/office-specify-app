/**
 * approvals 영역 순수 규칙 — 상태·본인 여부·권한 힌트에 따른 행동 가능 여부.
 * 컴포넌트와 분리해 테스트한다. 서버가 최종 검사하며 이 결과는 UI 힌트다.
 *
 * 안전 규칙 (§5.5, §7.3 UI08, wave 규칙):
 * - 본인이 만든 요청에는 승인·거절 버튼을 아예 렌더하지 않는다(숨김).
 * - 만료·거절·철회된 요청은 결정 불가(종결).
 * - 버전·스냅샷 불일치 시 결정은 서버가 409/412로 거절한다 — UI는 상태만 표시.
 */
import type {
  ApprovalListItem,
  ApprovalRequestDetail,
} from "@/lib/api/approvals";

export interface ApprovalAction {
  /** 버튼을 렌더할지 — 권한·종결·본인 요청이면 숨긴다. */
  visible: boolean;
  /** 보이지만 누를 수 없는 상태. */
  disabled: boolean;
}

const VISIBLE = (disabled = false): ApprovalAction => ({ visible: true, disabled });
const HIDDEN: ApprovalAction = { visible: false, disabled: true };

export interface ApprovalActionSet {
  approve: ApprovalAction;
  /** 거절 — 의견·변경 요청을 겸한다 (B.2에 별도 comment endpoint는 없다). */
  reject: ApprovalAction;
  /** 이미 승인된 허가의 철회 — 승인자·Admin (B.2). */
  revoke: ApprovalAction;
}

type ApprovalLike = Pick<
  ApprovalListItem,
  "status" | "expires_at" | "requested_by" | "allowed_actions"
> &
  Partial<Pick<ApprovalRequestDetail, "self_request" | "can_decide" | "requester">>;

/** 만료 시각이 지났거나 서버가 expired로 표시한 요청. */
export function approvalExpired(
  req: Pick<ApprovalListItem, "status" | "expires_at"> | null | undefined,
  now: Date = new Date()
): boolean {
  if (!req) return false;
  if (req.status === "expired") return true;
  if (!req.expires_at) return false;
  const t = Date.parse(req.expires_at);
  return Number.isFinite(t) && t <= now.getTime();
}

/**
 * 본인 요청 판별 — 서버 힌트(self_request, requester.id)를 우선하고,
 * 없으면 requested_by 와 내 principal_id 를 비교한다.
 */
export function isSelfRequest(
  req: ApprovalLike | null | undefined,
  mePrincipalId: string | undefined
): boolean {
  if (!req) return false;
  if (typeof req.self_request === "boolean") return req.self_request;
  const requesterId = req.requester?.id ?? req.requested_by;
  return Boolean(mePrincipalId) && requesterId === mePrincipalId;
}

function allows(req: ApprovalLike, name: string): boolean {
  const allowed = req.allowed_actions;
  return allowed === undefined || allowed.includes(name);
}

/**
 * 승인 요청 행별 가능 행동.
 * - decide(approve/reject): pending 이고 만료 전이며 본인 요청이 아닐 때만.
 *   서버가 can_decide 를 내려주면 그 값을 우선한다 (메일함 can_approve 등).
 * - revoke: approved 상태에서만(아직 미실행 허가 철회). 서버 권한 힌트가 있으면 따른다.
 */
export function approvalActions(
  req: ApprovalLike | null | undefined,
  opts?: { mePrincipalId?: string; now?: Date }
): ApprovalActionSet {
  if (!req) return { approve: HIDDEN, reject: HIDDEN, revoke: HIDDEN };

  const self = isSelfRequest(req, opts?.mePrincipalId);
  const expired = approvalExpired(req, opts?.now);
  const pending = req.status === "pending" && !expired;

  // 서버 힌트가 명시적으로 false면 무조건 숨긴다(권한 부족·게이트).
  const canDecide = req.can_decide === false ? false : pending && !self;
  const decide: ApprovalAction =
    canDecide && allows(req, "decide") ? VISIBLE(false) : HIDDEN;

  const revoke: ApprovalAction =
    req.status === "approved" && allows(req, "revoke") ? VISIBLE(false) : HIDDEN;

  return { approve: decide, reject: decide, revoke };
}

/** 결정 후 표시할 상태 배너 키 — detail.state<Camel> 메시지와 대응. */
export function approvalStateKey(status: string | undefined): string | null {
  switch (status) {
    case "approved":
      return "stateApproved";
    case "rejected":
      return "stateRejected";
    case "expired":
      return "stateExpired";
    case "revoked":
      return "stateRevoked";
    case "pending":
      return "statePending";
    default:
      return null;
  }
}
