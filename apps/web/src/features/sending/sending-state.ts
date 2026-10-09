import type {
  OutboundMessageResponse,
  OutboundPreflightResponse,
} from "@/lib/api/sending";

export type OutboundMessage = OutboundMessageResponse;
export type OutboundPreflight = OutboundPreflightResponse;
export type SendingPrincipalKind = "human" | "agent" | "system" | string;

export interface PreflightDisposition {
  canSubmit: boolean;
  canRequestApproval: boolean;
  failedChecks: string[];
}

const APPROVAL_REQUIRED = "APPROVAL_REQUIRED";

/** A preflight is advisory; it only unlocks UI after every required check passes. */
export function preflightDisposition(
  result: OutboundPreflight | null | undefined
): PreflightDisposition {
  if (!result || result.is_execution_authorization !== false) {
    return { canSubmit: false, canRequestApproval: false, failedChecks: [] };
  }

  const failed = result.checks.filter((check) => !check.passed);
  const failedChecks = failed.map((check) => check.code ?? check.name);
  const noGlobalBlocks = result.global_blocking_reasons.length === 0;

  return {
    canSubmit: noGlobalBlocks && failed.length === 0 && result.checks.length > 0,
    canRequestApproval:
      noGlobalBlocks &&
      failed.length > 0 &&
      failed.every((check) => check.code === APPROVAL_REQUIRED),
    failedChecks,
  };
}

export function canSubmitOutbound(
  outbound: OutboundMessage | null | undefined,
  principalKind: SendingPrincipalKind | undefined,
  preflight: OutboundPreflight | null | undefined,
  hasSendGrant: boolean
): boolean {
  return Boolean(
    outbound &&
      outbound.status === "draft" &&
      outbound.payload_hash &&
      principalKind === "human" &&
      hasSendGrant &&
      preflightDisposition(preflight).canSubmit
  );
}

export function canRequestOutboundApproval(
  outbound: OutboundMessage | null | undefined,
  principalKind: SendingPrincipalKind | undefined,
  preflight: OutboundPreflight | null | undefined
): boolean {
  return Boolean(
    outbound &&
      outbound.status === "draft" &&
      outbound.payload_hash &&
      principalKind === "human" &&
      preflightDisposition(preflight).canRequestApproval
  );
}

/** Cancel is safe only before the worker's external-provider boundary. */
export function canCancelOutbound(status: string | undefined): boolean {
  return status === "draft" || status === "queued" || status === "claimed";
}
