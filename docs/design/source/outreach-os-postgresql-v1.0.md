# Outreach OS — PostgreSQL 상세 명세 v1.0

> 2026-10-09 / 전체 설계서 부록 A의 독립 분리판. 실제 DB 실행 검증 전 기준 설계 DDL이다.

---

<a id="database-spec"></a>
# 부록 A. PostgreSQL 상세 설계

## A.1 범위와 검증 수준

아래는 전체 기능을 구현할 때의 **목표 관계형 스키마**다. 테이블 수는 초기 개발 첫날 모두 구현해야 한다는 의미가 아니라, 본문에서 정의한 CRM·메일·캠페인·예약·에이전트·운영 영역을 정규화한 결과다. G1~G6 순서에 따라 migration을 나누되, 최종 상태의 컬럼·FK·이름은 이 부록을 기준으로 한다.

기준 DDL은 PostgreSQL 17과 `btree_gist`를 사용한다. 메일·캠페인·예약의 데이터 원본과 projection을 분리하고, 대부분의 참조에 `workspace_id`를 포함한다. FK 대상의 존재·고유성·타입 일치는 문서 생성 과정에서 정적 검사했지만, **실제 PostgreSQL 인스턴스에 적용하거나 EXCLUDE·트리거·RLS를 실행한 결과는 아니다.** 운영에 사용하기 전 T01~T36 및 추가 DB 통합 테스트를 실행해야 한다.

### A.1.1 기존 초안 대비 주요 변경

| 기존 위험 | v1.0 변경 |
|---|---|
| 연락처마다 이메일이 유일해야 하는 구조 | `email_addresses` + `contact_emails`로 주소 식별과 사람 연결 분리 |
| 캠페인 수신자·단계가 서로 다른 캠페인을 가리킬 수 있음 | launch·revision·enrollment·step 복합 FK |
| 메시지와 thread가 다른 공용 계정에 속할 수 있음 | `(workspace, mailbox, thread)` 복합 FK |
| 발송 근거가 다른 주소·목적에 붙을 수 있음 | 주소·목적을 포함한 `communication_bases` 복합 FK |
| 전달 채널 이름으로 공급자 검사를 우회할 수 있음 | channel과 integration provider의 복합 FK, server adapter registry 검증 |
| 예약 변경의 새 점유가 기존 예약과 다른 테이블에서 경합 | `booking_slot_holds`를 초기 예약·변경의 공통 점유 원본으로 사용 |
| 일반 JSON만 있는 승인 대상 | 캠페인·메시지·예약변경·차단 해제 대상별 구체적 FK, 정확히 1개 |
| 차단 사유가 덮어써짐 | 사유별 활성 행 + append-only 이벤트 |
| queued 작업을 Redis에만 보관 | DB `jobs`, `outbox_events`, `processed_events` |

## A.2 공통 필드와 네이밍

UUID를 PK로 사용하고 FK 컬럼은 `*_id`, 외부 공급자 ID는 TEXT로 저장한다. 날짜는 TIMESTAMPTZ, 현지 반복 규칙의 시각은 TIME + IANA timezone이다. 금액이 필요한 후속 Deals에서는 통화 코드와 NUMERIC을 사용하고 부동소수점을 사용하지 않는다.

`version`이 있는 가변 엔티티는 업데이트할 때 `version = version + 1`, `updated_at = now()`를 서버가 명시적으로 적용한다. DB default는 생성 시각만 채우며 UPDATE 시 자동 변경된다고 가정하지 않는다. `JSONB`는 variable schema·정책 버전·외부 cursor 등에 사용하되 FK가 필요한 관계를 JSON에 숨기지 않는다.

문자열 enum은 `TEXT + CHECK`로 시작한다. enum 추가는 API·MCP·Worker·DB를 함께 변경하는 migration 대상이다. API DTO에서 임의 문자열을 통과시키지 않는다.

## A.3 도메인별 테이블

| 번호 | 테이블 | 책임 |
|---|---|---|
| 01 | `workspaces` | 단일 내부 조직. 초기 발송 긴급중지는 true로 시작한다. |
| 02 | `users` | 직원 로그인 신원. issuer와 subject를 함께 식별한다. |
| 03 | `principals` | 모든 업무 행위자의 공통 식별자. |
| 04 | `member_invitations` | SSO 등록 전 직원 초대. 토큰 원문 미저장. |
| 05 | `user_sessions` | 서버 세션. principal 비활성화도 매 요청 확인. |
| 06 | `service_credentials` | REST 서비스 credential. 원격 MCP OAuth와 별도 계약. |
| 07 | `integration_connections` | OAuth/외부 연동 자격 증명 참조. 비밀 원문 제외. |
| 08 | `mail_accounts` | 팀 전용 공용 Inbox. 개인 계정 소유 모델 아님. |
| 09 | `sender_identities` | 허용된 발신 주소. 주소 자체 변경은 새 행 생성. |
| 10 | `mail_account_grants` | 역할 권한과 교집합으로 사용하는 메일함 ACL. |
| 11 | `delivery_channels` | Inbox와 발송 공급자를 분리. Zoho 자동 marketing/transactional은 활성화 불가. |
| 12 | `channel_reviews` | 공급자 목적 허용의 증빙과 재검토 이력. |
| 13 | `companies` | 회사 관계 단계. 도메인 UNIQUE는 강제하지 않음. |
| 14 | `contacts` | 사람 식별. 주소·동의·차단은 별도. |
| 15 | `email_addresses` | 조직 내 주소 식별. 주소 변경은 새 행으로 처리. |
| 16 | `contact_emails` | 사람↔주소 N:M. 역할 주소 공동 연결 허용. |
| 17 | `communication_bases` | 목적·관할별 연락 근거. 주소 검증과 별개. |
| 18 | `agent_runs` | 에이전트 실행 목적·상한·사용량. |
| 19 | `lead_proposals` | 미검증 후보. 자동 발송 대상이 아님. |
| 20 | `research_evidence` | 필드별 증빙. 사실·추론·사람 검증 구분. |
| 21 | `email_templates` | 템플릿 논리적 이름. |
| 22 | `template_versions` | 내용 불변 템플릿 버전. |
| 23 | `campaigns` | 논리적 캠페인. 공용 회신 메일함 고정. |
| 24 | `campaign_revisions` | 발신 채널·시간·중단 규칙 스냅샷. |
| 25 | `campaign_steps` | 이전 단계 accepted 기준 상대 지연. 첫 단계 기준은 launch. |
| 26 | `campaign_launches` | 승인되는 실제 실행 묶음. 추가 대상은 새 launch. |
| 27 | `campaign_enrollments` | 고정 대상 주소와 실행 상태. 주소당 1행. |
| 28 | `email_threads` | 메일함 단위 대화. 업무 상태·담당자. |
| 29 | `message_drafts` | 직원·에이전트 편집 가능한 초안. 발송 스냅샷과 구분. |
| 30 | `outbound_messages` | 불변 발송 내용. 최초 HTTP 제출과 실제 메시지 식별 분리. |
| 31 | `email_messages` | 공급자에서 관측한 메시지. 로컬 발송 스냅샷과 연결 가능. |
| 32 | `message_participants` | 관측된 메시지의 주소 목록. BCC는 알 수 있는 범위만 저장. |
| 33 | `thread_read_states` | 직원별 읽음 상태. 다른 직원·Zoho 전역 읽음과 독립. |
| 34 | `send_attempts` | 외부 API 제출 시도별 이력. 요청 ID만으로 멱등성 보장 아님. |
| 35 | `reply_claims` | 공동 Inbox 동일 수신 메시지 회신 경쟁 방지. |
| 36 | `delivery_events` | 확인된 공급자 이벤트만 저장. delivered를 추정하지 않음. |
| 37 | `suppressions` | 사유별 차단. 해제해도 다른 활성 사유는 유지. |
| 38 | `suppression_events` | 반복 수신 거부·해제 감사 이벤트. |
| 39 | `unsubscribe_tokens` | 본문 링크용 capability. GET은 무변경, 원문 미저장. |
| 40 | `calendar_connections` | 각 인간 호스트의 Calendar 권한. 메일함 권한과 독립. |
| 41 | `availability_rules` | 현지 가용 규칙. 자정 경계 구간은 2행으로 분리. |
| 42 | `availability_exceptions` | 휴가·임시 가용/차단. blocked가 우선. |
| 43 | `meeting_types` | MVP는 유형당 호스트 1명. 변경은 신규 유형·링크. |
| 44 | `booking_links` | 개인화 예약 초대 링크. 공개 일반 링크는 정책으로 구분. |
| 45 | `bookings` | 앱 예약과 캘린더 처리 상태. unknown은 점유 유지. |
| 46 | `booking_changes` | 생성 제안·변경·취소 saga. 별도 점유 테이블로 새 슬롯 확보. |
| 47 | `booking_slot_holds` | 예약·변경의 통합 점유 원본. 이 테이블의 EXCLUDE로 상호 충돌까지 방어. |
| 48 | `booking_manage_tokens` | 예약 관리 전용 capability. 초대 토큰과 별도. |
| 49 | `approval_requests` | 승인 대상은 구체적 FK 1개. 범용 payload만으로 실행하지 않음. |
| 50 | `approval_decisions` | 요청마다 최종 결정 1개. 만료·자기승인·hash는 트랜잭션 검증. |
| 51 | `agent_actions` | 도구 호출 감사. 전체 원문·토큰·추론 미저장. |
| 52 | `notes` | 회사·담당자·대화 메모. 대화 메모도 mailbox ACL. |
| 53 | `tasks` | 내부 후속 업무. 관련 메일 조회에는 별도 ACL. |
| 54 | `activities` | 업무 타임라인 projection. 소스 권한을 따라 필터링. |
| 55 | `stored_objects` | 첨부·CSV·증빙의 비공개 객체 메타데이터. |
| 56 | `message_attachments` | 메일 첨부→객체 매핑. 다운로드 때 원본 메일 ACL. |
| 57 | `import_jobs` | CSV dry-run·검증 결과 스냅샷·commit. |
| 58 | `import_rows` | 부분 성공·행별 오류를 명확하게 반환. |
| 59 | `webhook_events` | 검증 후 내구성 저장·ACK. 원문 장기 저장하지 않음. |
| 60 | `sync_cursors` | 폴더·조회 경계별 메일 동기화 상태. |
| 61 | `jobs` | PostgreSQL durable job. Redis는 원본이 아님. |
| 62 | `outbox_events` | 업무 변경과 같은 트랜잭션에 기록. |
| 63 | `processed_events` | at-least-once 소비자 중복 제거. |
| 64 | `idempotency_records` | 내부 scope는 principal, 공개는 token 식별자. 원문 token 저장 금지. |
| 65 | `audit_logs` | 삭제된 대상의 감사도 남기므로 target은 의도적으로 FK 없음. 실행 근거로 사용 금지. |

## A.4 전체 기준 DDL

다음 블록은 하나의 신규 데이터베이스에 적용하는 기준 형태다. 기존 v0.3 스키마 위에 그대로 실행하는 upgrade migration은 아니며, 기존 데이터가 있다면 별도 backfill·중복 정리·검증 migration이 필요하다.

```sql
-- Outreach OS v1.0 / PostgreSQL 17 기준 설계 DDL
-- 실제 DB 실행 검증 전. 역할 생성·권한 부여·업무 함수는 별도 배포 단계를 따른다.
BEGIN;
CREATE EXTENSION IF NOT EXISTS btree_gist;

-- 단일 내부 조직. 초기 발송 긴급중지는 true로 시작한다.
CREATE TABLE workspaces (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','disabled')),
    default_timezone TEXT NOT NULL DEFAULT 'Asia/Seoul',
    policy_version BIGINT NOT NULL DEFAULT 1 CHECK (policy_version > 0),
    emergency_stop BOOLEAN NOT NULL DEFAULT true,
    settings JSONB NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(settings) = 'object'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 직원 로그인 신원. issuer와 subject를 함께 식별한다.
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    oidc_issuer TEXT,
    oidc_subject TEXT,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','disabled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (oidc_issuer, oidc_subject),
    CHECK ((oidc_issuer IS NULL) = (oidc_subject IS NULL))
);

-- 모든 업무 행위자의 공통 식별자.
CREATE TABLE principals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('human','agent','system')),
    user_id UUID,
    name TEXT NOT NULL,
    role TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('invited','active','disabled')),
    permission_version BIGINT NOT NULL DEFAULT 1 CHECK (permission_version > 0),
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, user_id),
    CHECK ((kind = 'human' AND user_id IS NOT NULL AND role IN ('admin','manager','member')) OR (kind = 'agent' AND user_id IS NULL AND role = 'agent') OR (kind = 'system' AND user_id IS NULL AND role = 'system'))
);

-- SSO 등록 전 직원 초대. 토큰 원문 미저장.
CREATE TABLE member_invitations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    email TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('admin','manager','member')),
    token_hash BYTEA NOT NULL UNIQUE,
    invited_by UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    accepted_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 서버 세션. principal 비활성화도 매 요청 확인.
CREATE TABLE user_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    principal_id UUID NOT NULL,
    session_hash BYTEA NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    last_seen_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- REST 서비스 credential. 원격 MCP OAuth와 별도 계약.
CREATE TABLE service_credentials (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    principal_id UUID NOT NULL,
    name TEXT NOT NULL,
    key_prefix TEXT NOT NULL,
    token_hash BYTEA NOT NULL UNIQUE,
    scopes TEXT[] NOT NULL DEFAULT '{}',
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- OAuth/외부 연동 자격 증명 참조. 비밀 원문 제외.
CREATE TABLE integration_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    provider TEXT NOT NULL CHECK (provider IN ('zoho_mail','zoho_calendar','delivery_provider','transactional_provider')),
    provider_subject TEXT,
    region TEXT NOT NULL,
    secret_ref TEXT NOT NULL,
    scopes TEXT[] NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','active','reauth_required','disabled')),
    connected_by UUID NOT NULL,
    last_checked_at TIMESTAMPTZ,
    last_error_code TEXT,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, id, provider)
);

-- 팀 전용 공용 Inbox. 개인 계정 소유 모델 아님.
CREATE TABLE mail_accounts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    provider TEXT NOT NULL DEFAULT 'zoho_mail' CHECK (provider = 'zoho_mail'),
    provider_account_id TEXT NOT NULL,
    primary_address TEXT NOT NULL,
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','active','reauth_required','disabled')),
    last_synced_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, connection_id, provider_account_id)
);

-- 허용된 발신 주소. 주소 자체 변경은 새 행 생성.
CREATE TABLE sender_identities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    address TEXT NOT NULL,
    display_name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','verified','disabled')),
    verified_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, mail_account_id, id),
    UNIQUE (workspace_id, mail_account_id, address)
);

-- 역할 권한과 교집합으로 사용하는 메일함 ACL.
CREATE TABLE mail_account_grants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    principal_id UUID NOT NULL,
    can_read BOOLEAN NOT NULL DEFAULT false,
    can_compose BOOLEAN NOT NULL DEFAULT false,
    can_send BOOLEAN NOT NULL DEFAULT false,
    can_approve BOOLEAN NOT NULL DEFAULT false,
    can_assign BOOLEAN NOT NULL DEFAULT false,
    granted_by UUID NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, mail_account_id, principal_id),
    CHECK (NOT can_compose OR can_read),
    CHECK (NOT can_send OR can_compose),
    CHECK (NOT can_approve OR can_send),
    CHECK (NOT can_assign OR can_read)
);

-- Inbox와 발송 공급자를 분리. Zoho 자동 marketing/transactional은 활성화 불가.
CREATE TABLE delivery_channels (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    sender_identity_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    provider TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('correspondence','marketing','transactional')),
    name TEXT NOT NULL,
    policy_status TEXT NOT NULL DEFAULT 'unverified' CHECK (policy_status IN ('unverified','allowed','blocked')),
    enabled BOOLEAN NOT NULL DEFAULT false,
    capabilities JSONB NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(capabilities) = 'object'),
    internal_daily_cap INTEGER CHECK (internal_daily_cap > 0),
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, mail_account_id, id),
    UNIQUE (workspace_id, id, sender_identity_id),
    CHECK (NOT enabled OR policy_status = 'allowed'),
    CHECK (NOT enabled OR provider <> 'zoho_mail' OR purpose = 'correspondence'),
    CHECK (provider IN ('zoho_mail','delivery_provider','transactional_provider')),
    UNIQUE (workspace_id, id, purpose)
);

-- 공급자 목적 허용의 증빙과 재검토 이력.
CREATE TABLE channel_reviews (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    delivery_channel_id UUID NOT NULL,
    reviewed_by UUID NOT NULL,
    result TEXT NOT NULL CHECK (result IN ('allowed','blocked','unverified')),
    purpose TEXT NOT NULL,
    policy_url TEXT,
    contract_evidence_ref TEXT,
    notes TEXT,
    reviewed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 회사 관계 단계. 도메인 UNIQUE는 강제하지 않음.
CREATE TABLE companies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 300),
    domain TEXT,
    website_url TEXT,
    industry TEXT,
    country_code TEXT CHECK (country_code IS NULL OR country_code ~ '^[A-Z]{2}$'),
    lifecycle_stage TEXT NOT NULL DEFAULT 'prospect' CHECK (lifecycle_stage IN ('prospect','engaged','qualified','customer','inactive')),
    owner_principal_id UUID,
    description TEXT,
    custom_fields JSONB NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(custom_fields) = 'object'),
    archived_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 사람 식별. 주소·동의·차단은 별도.
CREATE TABLE contacts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    company_id UUID,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 300),
    job_title TEXT,
    department TEXT,
    owner_principal_id UUID,
    status TEXT NOT NULL DEFAULT 'new' CHECK (status IN ('new','active','departed','inactive')),
    archived_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 조직 내 주소 식별. 주소 변경은 새 행으로 처리.
CREATE TABLE email_addresses (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    address_original TEXT NOT NULL,
    normalized_key TEXT NOT NULL,
    normalization_version SMALLINT NOT NULL DEFAULT 1,
    kind TEXT NOT NULL DEFAULT 'unknown' CHECK (kind IN ('person','role','unknown')),
    verification_status TEXT NOT NULL DEFAULT 'unknown' CHECK (verification_status IN ('unknown','verified','invalid','risky')),
    verified_at TIMESTAMPTZ,
    last_seen_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, normalized_key)
);

-- 사람↔주소 N:M. 역할 주소 공동 연결 허용.
CREATE TABLE contact_emails (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    contact_id UUID NOT NULL,
    email_address_id UUID NOT NULL,
    label TEXT,
    is_primary BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, contact_id, email_address_id)
);

-- 목적·관할별 연락 근거. 주소 검증과 별개.
CREATE TABLE communication_bases (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    email_address_id UUID NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('marketing','correspondence','transactional')),
    jurisdiction TEXT NOT NULL,
    basis_type TEXT NOT NULL CHECK (basis_type IN ('explicit_consent','requested_reply','requested_transaction','reviewed_exception')),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','valid','rejected','revoked','expired')),
    evidence_ref TEXT NOT NULL,
    collected_at TIMESTAMPTZ NOT NULL,
    reviewed_by UUID,
    reviewed_at TIMESTAMPTZ,
    valid_until TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    CHECK (status <> 'valid' OR (reviewed_by IS NOT NULL AND reviewed_at IS NOT NULL)),
    UNIQUE (workspace_id, email_address_id, purpose, id)
);

-- 에이전트 실행 목적·상한·사용량.
CREATE TABLE agent_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    principal_id UUID NOT NULL,
    requested_by UUID NOT NULL,
    source_credential_id UUID,
    objective TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued','running','awaiting_approval','completed','failed','canceled')),
    budget JSONB NOT NULL DEFAULT '{}',
    usage JSONB NOT NULL DEFAULT '{}',
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    cancel_requested_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 미검증 후보. 자동 발송 대상이 아님.
CREATE TABLE lead_proposals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    agent_run_id UUID,
    proposed_by UUID NOT NULL,
    external_key TEXT,
    company_name TEXT NOT NULL,
    domain TEXT,
    proposed_data JSONB NOT NULL CHECK (jsonb_typeof(proposed_data) = 'object'),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','accepted','rejected','needs_review')),
    accepted_company_id UUID,
    reviewed_by UUID,
    reviewed_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, agent_run_id, external_key)
);

-- 필드별 증빙. 사실·추론·사람 검증 구분.
CREATE TABLE research_evidence (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    company_id UUID,
    contact_id UUID,
    lead_proposal_id UUID,
    field_path TEXT NOT NULL,
    source_url TEXT NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL,
    excerpt TEXT,
    value JSONB NOT NULL,
    evidence_type TEXT NOT NULL CHECK (evidence_type IN ('observed','inferred')),
    confidence NUMERIC(4,3) CHECK (confidence BETWEEN 0 AND 1),
    review_status TEXT NOT NULL DEFAULT 'pending' CHECK (review_status IN ('pending','verified','rejected')),
    created_by UUID NOT NULL,
    reviewed_by UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    CHECK (num_nonnulls(company_id,contact_id,lead_proposal_id) = 1)
);

-- 템플릿 논리적 이름.
CREATE TABLE email_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    name TEXT NOT NULL,
    created_by UUID NOT NULL,
    archived_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 내용 불변 템플릿 버전.
CREATE TABLE template_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    template_id UUID NOT NULL,
    version_no INTEGER NOT NULL CHECK (version_no > 0),
    subject TEXT NOT NULL,
    body_text TEXT NOT NULL,
    body_html TEXT,
    variable_schema JSONB NOT NULL CHECK (jsonb_typeof(variable_schema) = 'object'),
    content_hash BYTEA NOT NULL,
    created_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, template_id, version_no)
);

-- 논리적 캠페인. 공용 회신 메일함 고정.
CREATE TABLE campaigns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    name TEXT NOT NULL,
    objective TEXT,
    owner_principal_id UUID NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft','awaiting_approval','active','paused','completed','canceled')),
    version BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, mail_account_id, id)
);

-- 발신 채널·시간·중단 규칙 스냅샷.
CREATE TABLE campaign_revisions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    purpose TEXT NOT NULL DEFAULT 'marketing' CHECK (purpose = 'marketing'),
    campaign_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    delivery_channel_id UUID NOT NULL,
    revision_no INTEGER NOT NULL CHECK (revision_no > 0),
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft','frozen')),
    timezone TEXT NOT NULL,
    schedule_rules JSONB NOT NULL CHECK (jsonb_typeof(schedule_rules) = 'object'),
    stop_on_reply BOOLEAN NOT NULL DEFAULT true,
    stop_on_booking BOOLEAN NOT NULL DEFAULT true,
    created_by UUID NOT NULL,
    frozen_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, campaign_id, revision_no),
    UNIQUE (workspace_id, id, delivery_channel_id),
    UNIQUE (workspace_id, id, campaign_id)
);

-- 이전 단계 accepted 기준 상대 지연. 첫 단계 기준은 launch.
CREATE TABLE campaign_steps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    revision_id UUID NOT NULL,
    template_version_id UUID NOT NULL,
    step_order INTEGER NOT NULL CHECK (step_order BETWEEN 1 AND 3),
    delay_hours INTEGER NOT NULL DEFAULT 0 CHECK (delay_hours >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, revision_id, id),
    UNIQUE (workspace_id, revision_id, step_order)
);

-- 승인되는 실제 실행 묶음. 추가 대상은 새 launch.
CREATE TABLE campaign_launches (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    revision_id UUID NOT NULL,
    delivery_channel_id UUID NOT NULL,
    requested_by UUID NOT NULL,
    status TEXT NOT NULL DEFAULT 'building' CHECK (status IN ('building','frozen','awaiting_approval','approved','running','paused','completed','canceled')),
    manifest_hash BYTEA,
    frozen_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, revision_id, id),
    UNIQUE (workspace_id, id, delivery_channel_id),
    CHECK (status = 'building' OR (manifest_hash IS NOT NULL AND frozen_at IS NOT NULL))
);

-- 고정 대상 주소와 실행 상태. 주소당 1행.
CREATE TABLE campaign_enrollments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    launch_id UUID NOT NULL,
    revision_id UUID NOT NULL,
    email_address_id UUID NOT NULL,
    contact_id UUID,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','active','completed','stopped','suppressed')),
    current_step_id UUID,
    next_send_at TIMESTAMPTZ,
    stop_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, launch_id, email_address_id),
    UNIQUE (workspace_id, launch_id, revision_id, id)
);

-- 메일함 단위 대화. 업무 상태·담당자.
CREATE TABLE email_threads (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    provider_thread_id TEXT,
    contact_id UUID,
    assigned_principal_id UUID,
    last_inbound_message_id UUID,
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','pending','resolved')),
    version BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, mail_account_id, id)
);

-- 직원·에이전트 편집 가능한 초안. 발송 스냅샷과 구분.
CREATE TABLE message_drafts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    owner_principal_id UUID NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'private' CHECK (visibility IN ('private','mailbox')),
    thread_id UUID,
    reply_to_message_id UUID,
    expected_last_inbound_id UUID,
    to_email_address_id UUID NOT NULL,
    template_version_id UUID,
    purpose TEXT NOT NULL CHECK (purpose IN ('correspondence','marketing','transactional')),
    subject TEXT NOT NULL,
    body_text TEXT NOT NULL,
    body_html TEXT,
    variables JSONB NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft','frozen','discarded')),
    version BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    CHECK (reply_to_message_id IS NULL OR thread_id IS NOT NULL),
    CHECK (expected_last_inbound_id IS NULL OR thread_id IS NOT NULL)
);

-- 불변 발송 내용. 최초 HTTP 제출과 실제 메시지 식별 분리.
CREATE TABLE outbound_messages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    delivery_channel_id UUID NOT NULL,
    sender_identity_id UUID NOT NULL,
    to_email_address_id UUID NOT NULL,
    communication_basis_id UUID,
    source_draft_id UUID,
    launch_id UUID,
    revision_id UUID,
    enrollment_id UUID,
    step_id UUID,
    thread_id UUID,
    reply_to_message_id UUID,
    requested_by UUID NOT NULL,
    source_credential_id UUID,
    purpose TEXT NOT NULL CHECK (purpose IN ('correspondence','marketing','transactional')),
    from_address_snapshot TEXT NOT NULL,
    to_address_snapshot TEXT NOT NULL,
    subject TEXT NOT NULL,
    body_text TEXT NOT NULL,
    body_html TEXT,
    payload_hash BYTEA NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft','queued','claimed','submitting','accepted','failed','unknown','canceled')),
    not_before TIMESTAMPTZ,
    accepted_at TIMESTAMPTZ,
    provider_message_id TEXT,
    failure_code TEXT,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, mail_account_id, id),
    CHECK (num_nonnulls(launch_id,revision_id,enrollment_id,step_id) IN (0,4)),
    UNIQUE (workspace_id, to_email_address_id, id),
    CHECK (reply_to_message_id IS NULL OR thread_id IS NOT NULL)
);

-- 공급자에서 관측한 메시지. 로컬 발송 스냅샷과 연결 가능.
CREATE TABLE email_messages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    thread_id UUID NOT NULL,
    outbound_message_id UUID,
    direction TEXT NOT NULL CHECK (direction IN ('inbound','outbound')),
    provider_message_id TEXT,
    internet_message_id TEXT,
    in_reply_to TEXT,
    reference_ids TEXT[] NOT NULL DEFAULT '{}',
    subject TEXT,
    body_text TEXT,
    body_html TEXT,
    message_at TIMESTAMPTZ NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    classification TEXT NOT NULL DEFAULT 'unknown' CHECK (classification IN ('human_positive','human_negative','question','unsubscribe_request','auto_reply','bounce','unknown')),
    classification_reviewed_by UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, mail_account_id, id),
    UNIQUE (workspace_id, mail_account_id, thread_id, id)
);

-- 관측된 메시지의 주소 목록. BCC는 알 수 있는 범위만 저장.
CREATE TABLE message_participants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    email_message_id UUID NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('from','to','cc','bcc','reply_to')),
    address TEXT NOT NULL,
    display_name TEXT,
    email_address_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, email_message_id, role, address)
);

-- 직원별 읽음 상태. 다른 직원·Zoho 전역 읽음과 독립.
CREATE TABLE thread_read_states (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    thread_id UUID NOT NULL,
    principal_id UUID NOT NULL,
    last_read_at TIMESTAMPTZ NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, thread_id, principal_id)
);

-- 외부 API 제출 시도별 이력. 요청 ID만으로 멱등성 보장 아님.
CREATE TABLE send_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    outbound_message_id UUID NOT NULL,
    attempt_no INTEGER NOT NULL CHECK (attempt_no > 0),
    lease_token UUID NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('started','accepted','definitive_failed','unknown')),
    provider_request_id TEXT,
    provider_message_id TEXT,
    http_status INTEGER CHECK (http_status BETWEEN 100 AND 599),
    error_code TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, outbound_message_id, attempt_no)
);

-- 공동 Inbox 동일 수신 메시지 회신 경쟁 방지.
CREATE TABLE reply_claims (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    thread_id UUID NOT NULL,
    inbound_message_id UUID NOT NULL,
    outbound_message_id UUID NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('reserved','submitted','accepted','released')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 확인된 공급자 이벤트만 저장. delivered를 추정하지 않음.
CREATE TABLE delivery_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    outbound_message_id UUID NOT NULL,
    provider_event_key TEXT NOT NULL,
    event_type TEXT NOT NULL CHECK (event_type IN ('accepted','delivered','bounced','complained')),
    occurred_at TIMESTAMPTZ NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    details JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, provider_event_key)
);

-- 사유별 차단. 해제해도 다른 활성 사유는 유지.
CREATE TABLE suppressions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    email_address_id UUID NOT NULL,
    purpose_scope TEXT NOT NULL CHECK (purpose_scope IN ('marketing','all')),
    reason TEXT NOT NULL CHECK (reason IN ('unsubscribe','complaint','hard_bounce','manual')),
    source_outbound_id UUID,
    created_by UUID,
    released_at TIMESTAMPTZ,
    released_by UUID,
    release_evidence_ref TEXT,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    CHECK (released_at IS NULL OR (released_by IS NOT NULL AND release_evidence_ref IS NOT NULL))
);

-- 반복 수신 거부·해제 감사 이벤트.
CREATE TABLE suppression_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    suppression_id UUID NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('created','reaffirmed','released')),
    actor_principal_id UUID,
    source_kind TEXT NOT NULL,
    evidence_ref TEXT,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 본문 링크용 capability. GET은 무변경, 원문 미저장.
CREATE TABLE unsubscribe_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    token_hash BYTEA NOT NULL UNIQUE,
    email_address_id UUID NOT NULL,
    outbound_message_id UUID NOT NULL,
    purpose_scope TEXT NOT NULL DEFAULT 'marketing' CHECK (purpose_scope = 'marketing'),
    used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 각 인간 호스트의 Calendar 권한. 메일함 권한과 독립.
CREATE TABLE calendar_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    provider TEXT NOT NULL DEFAULT 'zoho_calendar' CHECK (provider = 'zoho_calendar'),
    host_principal_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    provider_calendar_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','active','reauth_required','disabled')),
    include_in_busy BOOLEAN NOT NULL DEFAULT true,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, host_principal_id, id),
    UNIQUE (workspace_id, connection_id, provider_calendar_id)
);

-- 현지 가용 규칙. 자정 경계 구간은 2행으로 분리.
CREATE TABLE availability_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    host_principal_id UUID NOT NULL,
    timezone TEXT NOT NULL,
    weekday SMALLINT NOT NULL CHECK (weekday BETWEEN 1 AND 7),
    start_local TIME NOT NULL,
    end_local TIME NOT NULL,
    valid_from DATE,
    valid_until DATE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    CHECK (end_local > start_local),
    CHECK (valid_until IS NULL OR valid_from IS NULL OR valid_until >= valid_from)
);

-- 휴가·임시 가용/차단. blocked가 우선.
CREATE TABLE availability_exceptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    host_principal_id UUID NOT NULL,
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('blocked','available')),
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    CHECK (ends_at > starts_at)
);

-- MVP는 유형당 호스트 1명. 변경은 신규 유형·링크.
CREATE TABLE meeting_types (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    host_principal_id UUID NOT NULL,
    calendar_connection_id UUID NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    duration_minutes INTEGER NOT NULL DEFAULT 30 CHECK (duration_minutes BETWEEN 5 AND 480),
    buffer_before_minutes INTEGER NOT NULL DEFAULT 10 CHECK (buffer_before_minutes >= 0),
    buffer_after_minutes INTEGER NOT NULL DEFAULT 10 CHECK (buffer_after_minutes >= 0),
    minimum_notice_minutes INTEGER NOT NULL DEFAULT 1440 CHECK (minimum_notice_minutes >= 0),
    booking_horizon_days INTEGER NOT NULL DEFAULT 30 CHECK (booking_horizon_days BETWEEN 1 AND 365),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','disabled')),
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, host_principal_id, id)
);

-- 개인화 예약 초대 링크. 공개 일반 링크는 정책으로 구분.
CREATE TABLE booking_links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    meeting_type_id UUID NOT NULL,
    host_principal_id UUID NOT NULL,
    token_hash BYTEA NOT NULL UNIQUE,
    bound_email_address_id UUID,
    campaign_enrollment_id UUID,
    max_uses INTEGER NOT NULL DEFAULT 1 CHECK (max_uses > 0),
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, host_principal_id, id)
);

-- 앱 예약과 캘린더 처리 상태. unknown은 점유 유지.
CREATE TABLE bookings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    booking_link_id UUID NOT NULL,
    host_principal_id UUID NOT NULL,
    guest_email_address_id UUID NOT NULL,
    guest_name TEXT NOT NULL,
    guest_timezone TEXT NOT NULL,
    guest_email_verification_status TEXT NOT NULL DEFAULT 'unverified' CHECK (guest_email_verification_status IN ('unverified','invitation_bound','verified')),
    privacy_notice_version TEXT,
    privacy_acknowledged_at TIMESTAMPTZ,
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ NOT NULL,
    occupied_start TIMESTAMPTZ NOT NULL,
    occupied_end TIMESTAMPTZ NOT NULL,
    status TEXT NOT NULL DEFAULT 'held' CHECK (status IN ('held','confirming','confirmed','canceling','canceled','failed','unknown','expired')),
    hold_expires_at TIMESTAMPTZ,
    provider_event_id TEXT,
    provider_revision TEXT,
    confirmation_verified_at TIMESTAMPTZ,
    failure_code TEXT,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, host_principal_id, id),
    CHECK (ends_at > starts_at),
    CHECK (occupied_start <= starts_at AND occupied_end >= ends_at),
    CHECK (status <> 'held' OR hold_expires_at IS NOT NULL),
    CHECK (status <> 'confirmed' OR provider_event_id IS NOT NULL)
);

-- 생성 제안·변경·취소 saga. 별도 점유 테이블로 새 슬롯 확보.
CREATE TABLE booking_changes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    booking_id UUID,
    host_principal_id UUID NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('create','reschedule','cancel')),
    proposed_data JSONB NOT NULL CHECK (jsonb_typeof(proposed_data) = 'object'),
    requested_by UUID,
    status TEXT NOT NULL DEFAULT 'proposed' CHECK (status IN ('proposed','approved','processing','completed','failed','unknown','canceled')),
    new_occupied_start TIMESTAMPTZ,
    new_occupied_end TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    CHECK ((kind = 'create' AND booking_id IS NULL) OR (kind <> 'create' AND booking_id IS NOT NULL)),
    CHECK ((new_occupied_start IS NULL) = (new_occupied_end IS NULL)),
    CHECK (new_occupied_end IS NULL OR new_occupied_end > new_occupied_start),
    UNIQUE (workspace_id, host_principal_id, id)
);

-- 예약·변경의 통합 점유 원본. 이 테이블의 EXCLUDE로 상호 충돌까지 방어.
CREATE TABLE booking_slot_holds (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    host_principal_id UUID NOT NULL,
    booking_id UUID,
    booking_change_id UUID,
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('held','confirming','confirmed','unknown','released','expired')),
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    CHECK (num_nonnulls(booking_id,booking_change_id) = 1),
    CHECK (ends_at > starts_at)
);

-- 예약 관리 전용 capability. 초대 토큰과 별도.
CREATE TABLE booking_manage_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    booking_id UUID NOT NULL,
    token_hash BYTEA NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 승인 대상은 구체적 FK 1개. 범용 payload만으로 실행하지 않음.
CREATE TABLE approval_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    requested_by UUID NOT NULL,
    launch_id UUID,
    outbound_message_id UUID,
    booking_change_id UUID,
    suppression_id UUID,
    action_type TEXT NOT NULL CHECK (action_type IN ('campaign_launch','message_submit','booking_change','suppression_release')),
    snapshot_hash BYTEA NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved','rejected','expired','revoked')),
    expires_at TIMESTAMPTZ NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    CHECK ((action_type = 'campaign_launch' AND launch_id IS NOT NULL AND num_nonnulls(launch_id,outbound_message_id,booking_change_id,suppression_id)=1) OR (action_type = 'message_submit' AND outbound_message_id IS NOT NULL AND num_nonnulls(launch_id,outbound_message_id,booking_change_id,suppression_id)=1) OR (action_type = 'booking_change' AND booking_change_id IS NOT NULL AND num_nonnulls(launch_id,outbound_message_id,booking_change_id,suppression_id)=1) OR (action_type = 'suppression_release' AND suppression_id IS NOT NULL AND num_nonnulls(launch_id,outbound_message_id,booking_change_id,suppression_id)=1))
);

-- 요청마다 최종 결정 1개. 만료·자기승인·hash는 트랜잭션 검증.
CREATE TABLE approval_decisions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    approval_request_id UUID NOT NULL,
    decided_by UUID NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('approved','rejected')),
    snapshot_hash BYTEA NOT NULL,
    reason TEXT,
    version_at_decision BIGINT NOT NULL,
    decided_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, approval_request_id)
);

-- 도구 호출 감사. 전체 원문·토큰·추론 미저장.
CREATE TABLE agent_actions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    agent_run_id UUID NOT NULL,
    tool_name TEXT NOT NULL,
    arguments_hash BYTEA NOT NULL,
    redacted_arguments JSONB NOT NULL DEFAULT '{}',
    result_code TEXT,
    result_ref TEXT,
    approval_request_id UUID,
    has_external_effect BOOLEAN NOT NULL DEFAULT false,
    trace_id TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 회사·담당자·대화 메모. 대화 메모도 mailbox ACL.
CREATE TABLE notes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    company_id UUID,
    contact_id UUID,
    thread_id UUID,
    mail_account_id UUID,
    created_by UUID NOT NULL,
    body TEXT NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    CHECK (num_nonnulls(company_id,contact_id,thread_id) = 1),
    CHECK ((thread_id IS NULL) = (mail_account_id IS NULL))
);

-- 내부 후속 업무. 관련 메일 조회에는 별도 ACL.
CREATE TABLE tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    title TEXT NOT NULL,
    owner_principal_id UUID NOT NULL,
    company_id UUID,
    contact_id UUID,
    thread_id UUID,
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','done','canceled')),
    due_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 업무 타임라인 projection. 소스 권한을 따라 필터링.
CREATE TABLE activities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    company_id UUID,
    contact_id UUID,
    mail_account_id UUID,
    thread_id UUID,
    booking_id UUID,
    actor_principal_id UUID,
    kind TEXT NOT NULL,
    source_event_id UUID NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    summary JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, source_event_id, kind),
    CHECK (thread_id IS NULL OR mail_account_id IS NOT NULL)
);

-- 첨부·CSV·증빙의 비공개 객체 메타데이터.
CREATE TABLE stored_objects (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    storage_key TEXT NOT NULL UNIQUE,
    media_type TEXT NOT NULL,
    byte_size BIGINT NOT NULL CHECK (byte_size >= 0),
    content_hash BYTEA NOT NULL,
    scan_status TEXT NOT NULL DEFAULT 'pending' CHECK (scan_status IN ('pending','clean','rejected')),
    created_by UUID,
    expires_at TIMESTAMPTZ,
    deleted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 메일 첨부→객체 매핑. 다운로드 때 원본 메일 ACL.
CREATE TABLE message_attachments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    email_message_id UUID NOT NULL,
    stored_object_id UUID NOT NULL,
    filename TEXT NOT NULL,
    provider_attachment_id TEXT,
    is_inline BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, email_message_id, stored_object_id)
);

-- CSV dry-run·검증 결과 스냅샷·commit.
CREATE TABLE import_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    stored_object_id UUID NOT NULL,
    requested_by UUID NOT NULL,
    mapping JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'uploaded' CHECK (status IN ('uploaded','validating','ready','committing','completed','failed','canceled')),
    validation_hash BYTEA,
    summary JSONB NOT NULL DEFAULT '{}',
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 부분 성공·행별 오류를 명확하게 반환.
CREATE TABLE import_rows (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    import_job_id UUID NOT NULL,
    row_no INTEGER NOT NULL CHECK (row_no > 0),
    normalized_data JSONB NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('valid','invalid','duplicate','imported','skipped')),
    errors JSONB NOT NULL DEFAULT '[]',
    company_id UUID,
    contact_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, import_job_id, row_no)
);

-- 검증 후 내구성 저장·ACK. 원문 장기 저장하지 않음.
CREATE TABLE webhook_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    provider_event_key TEXT NOT NULL,
    payload_hash BYTEA NOT NULL,
    redacted_payload JSONB NOT NULL,
    signature_verified BOOLEAN NOT NULL CHECK (signature_verified),
    status TEXT NOT NULL DEFAULT 'received' CHECK (status IN ('received','processed','failed')),
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, connection_id, provider_event_key)
);

-- 폴더·조회 경계별 메일 동기화 상태.
CREATE TABLE sync_cursors (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    mail_account_id UUID NOT NULL,
    stream_key TEXT NOT NULL,
    cursor_data JSONB NOT NULL DEFAULT '{}',
    last_success_at TIMESTAMPTZ,
    last_error_code TEXT,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, mail_account_id, stream_key)
);

-- PostgreSQL durable job. Redis는 원본이 아님.
CREATE TABLE jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    kind TEXT NOT NULL,
    payload_version INTEGER NOT NULL DEFAULT 1 CHECK (payload_version > 0),
    payload JSONB NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    dedupe_key TEXT NOT NULL,
    requested_by UUID,
    source_credential_id UUID,
    state TEXT NOT NULL DEFAULT 'queued' CHECK (state IN ('queued','running','retry_wait','completed','dead','canceled','unknown')),
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_owner TEXT,
    lease_token UUID,
    lease_expires_at TIMESTAMPTZ,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    last_error_code TEXT,
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, kind, dedupe_key),
    CHECK (state <> 'running' OR (lease_owner IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL))
);

-- 업무 변경과 같은 트랜잭션에 기록.
CREATE TABLE outbox_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    event_type TEXT NOT NULL,
    aggregate_type TEXT NOT NULL,
    aggregate_id UUID NOT NULL,
    aggregate_version BIGINT NOT NULL,
    payload JSONB NOT NULL,
    published_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- at-least-once 소비자 중복 제거.
CREATE TABLE processed_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    consumer TEXT NOT NULL,
    event_id UUID NOT NULL,
    processed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, consumer, event_id)
);

-- 내부 scope는 principal, 공개는 token 식별자. 원문 token 저장 금지.
CREATE TABLE idempotency_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    scope_key TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_hash BYTEA NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('processing','completed')),
    response_status INTEGER,
    response_body JSONB,
    resource_id UUID,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, scope_key, endpoint, idempotency_key)
);

-- 삭제된 대상의 감사도 남기므로 target은 의도적으로 FK 없음. 실행 근거로 사용 금지.
CREATE TABLE audit_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    actor_principal_id UUID,
    requested_by UUID,
    approved_by UUID,
    job_id UUID,
    action TEXT NOT NULL,
    target_type TEXT NOT NULL,
    target_id UUID,
    request_id TEXT NOT NULL,
    trace_id TEXT,
    details JSONB NOT NULL DEFAULT '{}',
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id)
);

-- 모든 테이블 생성 후 복합 FK를 추가하여 선언 순서·순환 참조를 분리한다.
ALTER TABLE principals ADD CONSTRAINT fk_001
    FOREIGN KEY (user_id)
    REFERENCES users (id);
ALTER TABLE member_invitations ADD CONSTRAINT fk_002
    FOREIGN KEY (workspace_id, invited_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE user_sessions ADD CONSTRAINT fk_003
    FOREIGN KEY (workspace_id, principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE service_credentials ADD CONSTRAINT fk_004
    FOREIGN KEY (workspace_id, principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE integration_connections ADD CONSTRAINT fk_005
    FOREIGN KEY (workspace_id, connected_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE mail_accounts ADD CONSTRAINT fk_006
    FOREIGN KEY (workspace_id, connection_id)
    REFERENCES integration_connections (workspace_id, id);
ALTER TABLE sender_identities ADD CONSTRAINT fk_007
    FOREIGN KEY (workspace_id, mail_account_id)
    REFERENCES mail_accounts (workspace_id, id);
ALTER TABLE mail_account_grants ADD CONSTRAINT fk_008
    FOREIGN KEY (workspace_id, mail_account_id)
    REFERENCES mail_accounts (workspace_id, id);
ALTER TABLE mail_account_grants ADD CONSTRAINT fk_009
    FOREIGN KEY (workspace_id, principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE mail_account_grants ADD CONSTRAINT fk_010
    FOREIGN KEY (workspace_id, granted_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE delivery_channels ADD CONSTRAINT fk_011
    FOREIGN KEY (workspace_id, mail_account_id)
    REFERENCES mail_accounts (workspace_id, id);
ALTER TABLE delivery_channels ADD CONSTRAINT fk_012
    FOREIGN KEY (workspace_id, connection_id)
    REFERENCES integration_connections (workspace_id, id);
ALTER TABLE delivery_channels ADD CONSTRAINT fk_013
    FOREIGN KEY (workspace_id, mail_account_id, sender_identity_id)
    REFERENCES sender_identities (workspace_id, mail_account_id, id);
ALTER TABLE channel_reviews ADD CONSTRAINT fk_014
    FOREIGN KEY (workspace_id, delivery_channel_id)
    REFERENCES delivery_channels (workspace_id, id);
ALTER TABLE channel_reviews ADD CONSTRAINT fk_015
    FOREIGN KEY (workspace_id, reviewed_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE companies ADD CONSTRAINT fk_016
    FOREIGN KEY (workspace_id, owner_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE contacts ADD CONSTRAINT fk_017
    FOREIGN KEY (workspace_id, company_id)
    REFERENCES companies (workspace_id, id);
ALTER TABLE contacts ADD CONSTRAINT fk_018
    FOREIGN KEY (workspace_id, owner_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE contact_emails ADD CONSTRAINT fk_019
    FOREIGN KEY (workspace_id, contact_id)
    REFERENCES contacts (workspace_id, id);
ALTER TABLE contact_emails ADD CONSTRAINT fk_020
    FOREIGN KEY (workspace_id, email_address_id)
    REFERENCES email_addresses (workspace_id, id);
ALTER TABLE communication_bases ADD CONSTRAINT fk_021
    FOREIGN KEY (workspace_id, email_address_id)
    REFERENCES email_addresses (workspace_id, id);
ALTER TABLE communication_bases ADD CONSTRAINT fk_022
    FOREIGN KEY (workspace_id, reviewed_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE agent_runs ADD CONSTRAINT fk_023
    FOREIGN KEY (workspace_id, principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE agent_runs ADD CONSTRAINT fk_024
    FOREIGN KEY (workspace_id, requested_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE agent_runs ADD CONSTRAINT fk_025
    FOREIGN KEY (workspace_id, source_credential_id)
    REFERENCES service_credentials (workspace_id, id);
ALTER TABLE lead_proposals ADD CONSTRAINT fk_026
    FOREIGN KEY (workspace_id, agent_run_id)
    REFERENCES agent_runs (workspace_id, id);
ALTER TABLE lead_proposals ADD CONSTRAINT fk_027
    FOREIGN KEY (workspace_id, proposed_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE lead_proposals ADD CONSTRAINT fk_028
    FOREIGN KEY (workspace_id, accepted_company_id)
    REFERENCES companies (workspace_id, id);
ALTER TABLE lead_proposals ADD CONSTRAINT fk_029
    FOREIGN KEY (workspace_id, reviewed_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE research_evidence ADD CONSTRAINT fk_030
    FOREIGN KEY (workspace_id, company_id)
    REFERENCES companies (workspace_id, id);
ALTER TABLE research_evidence ADD CONSTRAINT fk_031
    FOREIGN KEY (workspace_id, contact_id)
    REFERENCES contacts (workspace_id, id);
ALTER TABLE research_evidence ADD CONSTRAINT fk_032
    FOREIGN KEY (workspace_id, lead_proposal_id)
    REFERENCES lead_proposals (workspace_id, id);
ALTER TABLE research_evidence ADD CONSTRAINT fk_033
    FOREIGN KEY (workspace_id, created_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE research_evidence ADD CONSTRAINT fk_034
    FOREIGN KEY (workspace_id, reviewed_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE email_templates ADD CONSTRAINT fk_035
    FOREIGN KEY (workspace_id, created_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE template_versions ADD CONSTRAINT fk_036
    FOREIGN KEY (workspace_id, template_id)
    REFERENCES email_templates (workspace_id, id);
ALTER TABLE template_versions ADD CONSTRAINT fk_037
    FOREIGN KEY (workspace_id, created_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE campaigns ADD CONSTRAINT fk_038
    FOREIGN KEY (workspace_id, mail_account_id)
    REFERENCES mail_accounts (workspace_id, id);
ALTER TABLE campaigns ADD CONSTRAINT fk_039
    FOREIGN KEY (workspace_id, owner_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE campaign_revisions ADD CONSTRAINT fk_040
    FOREIGN KEY (workspace_id, mail_account_id, campaign_id)
    REFERENCES campaigns (workspace_id, mail_account_id, id);
ALTER TABLE campaign_revisions ADD CONSTRAINT fk_041
    FOREIGN KEY (workspace_id, mail_account_id, delivery_channel_id)
    REFERENCES delivery_channels (workspace_id, mail_account_id, id);
ALTER TABLE campaign_revisions ADD CONSTRAINT fk_042
    FOREIGN KEY (workspace_id, created_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE campaign_steps ADD CONSTRAINT fk_043
    FOREIGN KEY (workspace_id, revision_id)
    REFERENCES campaign_revisions (workspace_id, id);
ALTER TABLE campaign_steps ADD CONSTRAINT fk_044
    FOREIGN KEY (workspace_id, template_version_id)
    REFERENCES template_versions (workspace_id, id);
ALTER TABLE campaign_launches ADD CONSTRAINT fk_045
    FOREIGN KEY (workspace_id, revision_id, delivery_channel_id)
    REFERENCES campaign_revisions (workspace_id, id, delivery_channel_id);
ALTER TABLE campaign_launches ADD CONSTRAINT fk_046
    FOREIGN KEY (workspace_id, requested_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE campaign_enrollments ADD CONSTRAINT fk_047
    FOREIGN KEY (workspace_id, revision_id, launch_id)
    REFERENCES campaign_launches (workspace_id, revision_id, id);
ALTER TABLE campaign_enrollments ADD CONSTRAINT fk_048
    FOREIGN KEY (workspace_id, revision_id, current_step_id)
    REFERENCES campaign_steps (workspace_id, revision_id, id);
ALTER TABLE campaign_enrollments ADD CONSTRAINT fk_049
    FOREIGN KEY (workspace_id, email_address_id)
    REFERENCES email_addresses (workspace_id, id);
ALTER TABLE campaign_enrollments ADD CONSTRAINT fk_050
    FOREIGN KEY (workspace_id, contact_id)
    REFERENCES contacts (workspace_id, id);
ALTER TABLE email_threads ADD CONSTRAINT fk_051
    FOREIGN KEY (workspace_id, mail_account_id)
    REFERENCES mail_accounts (workspace_id, id);
ALTER TABLE email_threads ADD CONSTRAINT fk_052
    FOREIGN KEY (workspace_id, contact_id)
    REFERENCES contacts (workspace_id, id);
ALTER TABLE email_threads ADD CONSTRAINT fk_053
    FOREIGN KEY (workspace_id, assigned_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE message_drafts ADD CONSTRAINT fk_054
    FOREIGN KEY (workspace_id, mail_account_id)
    REFERENCES mail_accounts (workspace_id, id);
ALTER TABLE message_drafts ADD CONSTRAINT fk_055
    FOREIGN KEY (workspace_id, owner_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE message_drafts ADD CONSTRAINT fk_056
    FOREIGN KEY (workspace_id, to_email_address_id)
    REFERENCES email_addresses (workspace_id, id);
ALTER TABLE message_drafts ADD CONSTRAINT fk_057
    FOREIGN KEY (workspace_id, template_version_id)
    REFERENCES template_versions (workspace_id, id);
ALTER TABLE message_drafts ADD CONSTRAINT fk_058
    FOREIGN KEY (workspace_id, mail_account_id, thread_id)
    REFERENCES email_threads (workspace_id, mail_account_id, id);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_059
    FOREIGN KEY (workspace_id, mail_account_id, delivery_channel_id)
    REFERENCES delivery_channels (workspace_id, mail_account_id, id);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_060
    FOREIGN KEY (workspace_id, delivery_channel_id, sender_identity_id)
    REFERENCES delivery_channels (workspace_id, id, sender_identity_id);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_061
    FOREIGN KEY (workspace_id, launch_id, delivery_channel_id)
    REFERENCES campaign_launches (workspace_id, id, delivery_channel_id);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_062
    FOREIGN KEY (workspace_id, launch_id, revision_id, enrollment_id)
    REFERENCES campaign_enrollments (workspace_id, launch_id, revision_id, id);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_063
    FOREIGN KEY (workspace_id, revision_id, step_id)
    REFERENCES campaign_steps (workspace_id, revision_id, id);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_064
    FOREIGN KEY (workspace_id, mail_account_id, thread_id)
    REFERENCES email_threads (workspace_id, mail_account_id, id);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_065
    FOREIGN KEY (workspace_id, to_email_address_id)
    REFERENCES email_addresses (workspace_id, id);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_066
    FOREIGN KEY (workspace_id, communication_basis_id)
    REFERENCES communication_bases (workspace_id, id);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_067
    FOREIGN KEY (workspace_id, source_draft_id)
    REFERENCES message_drafts (workspace_id, id);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_068
    FOREIGN KEY (workspace_id, requested_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_069
    FOREIGN KEY (workspace_id, source_credential_id)
    REFERENCES service_credentials (workspace_id, id);
ALTER TABLE email_messages ADD CONSTRAINT fk_070
    FOREIGN KEY (workspace_id, mail_account_id, thread_id)
    REFERENCES email_threads (workspace_id, mail_account_id, id);
ALTER TABLE email_messages ADD CONSTRAINT fk_071
    FOREIGN KEY (workspace_id, mail_account_id, outbound_message_id)
    REFERENCES outbound_messages (workspace_id, mail_account_id, id);
ALTER TABLE email_messages ADD CONSTRAINT fk_072
    FOREIGN KEY (workspace_id, classification_reviewed_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE email_threads ADD CONSTRAINT fk_073
    FOREIGN KEY (workspace_id, mail_account_id, id, last_inbound_message_id)
    REFERENCES email_messages (workspace_id, mail_account_id, thread_id, id);
ALTER TABLE message_drafts ADD CONSTRAINT fk_074
    FOREIGN KEY (workspace_id, mail_account_id, thread_id, reply_to_message_id)
    REFERENCES email_messages (workspace_id, mail_account_id, thread_id, id);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_075
    FOREIGN KEY (workspace_id, mail_account_id, thread_id, reply_to_message_id)
    REFERENCES email_messages (workspace_id, mail_account_id, thread_id, id);
ALTER TABLE message_drafts ADD CONSTRAINT fk_076
    FOREIGN KEY (workspace_id, mail_account_id, thread_id, expected_last_inbound_id)
    REFERENCES email_messages (workspace_id, mail_account_id, thread_id, id);
ALTER TABLE message_participants ADD CONSTRAINT fk_077
    FOREIGN KEY (workspace_id, email_message_id)
    REFERENCES email_messages (workspace_id, id);
ALTER TABLE message_participants ADD CONSTRAINT fk_078
    FOREIGN KEY (workspace_id, email_address_id)
    REFERENCES email_addresses (workspace_id, id);
ALTER TABLE thread_read_states ADD CONSTRAINT fk_079
    FOREIGN KEY (workspace_id, thread_id)
    REFERENCES email_threads (workspace_id, id);
ALTER TABLE thread_read_states ADD CONSTRAINT fk_080
    FOREIGN KEY (workspace_id, principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE send_attempts ADD CONSTRAINT fk_081
    FOREIGN KEY (workspace_id, outbound_message_id)
    REFERENCES outbound_messages (workspace_id, id);
ALTER TABLE reply_claims ADD CONSTRAINT fk_082
    FOREIGN KEY (workspace_id, mail_account_id, thread_id, inbound_message_id)
    REFERENCES email_messages (workspace_id, mail_account_id, thread_id, id);
ALTER TABLE reply_claims ADD CONSTRAINT fk_083
    FOREIGN KEY (workspace_id, mail_account_id, outbound_message_id)
    REFERENCES outbound_messages (workspace_id, mail_account_id, id);
ALTER TABLE delivery_events ADD CONSTRAINT fk_084
    FOREIGN KEY (workspace_id, outbound_message_id)
    REFERENCES outbound_messages (workspace_id, id);
ALTER TABLE suppressions ADD CONSTRAINT fk_085
    FOREIGN KEY (workspace_id, email_address_id)
    REFERENCES email_addresses (workspace_id, id);
ALTER TABLE suppressions ADD CONSTRAINT fk_086
    FOREIGN KEY (workspace_id, source_outbound_id)
    REFERENCES outbound_messages (workspace_id, id);
ALTER TABLE suppressions ADD CONSTRAINT fk_087
    FOREIGN KEY (workspace_id, created_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE suppressions ADD CONSTRAINT fk_088
    FOREIGN KEY (workspace_id, released_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE suppression_events ADD CONSTRAINT fk_089
    FOREIGN KEY (workspace_id, suppression_id)
    REFERENCES suppressions (workspace_id, id);
ALTER TABLE suppression_events ADD CONSTRAINT fk_090
    FOREIGN KEY (workspace_id, actor_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE unsubscribe_tokens ADD CONSTRAINT fk_091
    FOREIGN KEY (workspace_id, email_address_id)
    REFERENCES email_addresses (workspace_id, id);
ALTER TABLE unsubscribe_tokens ADD CONSTRAINT fk_092
    FOREIGN KEY (workspace_id, outbound_message_id)
    REFERENCES outbound_messages (workspace_id, id);
ALTER TABLE calendar_connections ADD CONSTRAINT fk_093
    FOREIGN KEY (workspace_id, host_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE calendar_connections ADD CONSTRAINT fk_094
    FOREIGN KEY (workspace_id, connection_id)
    REFERENCES integration_connections (workspace_id, id);
ALTER TABLE availability_rules ADD CONSTRAINT fk_095
    FOREIGN KEY (workspace_id, host_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE availability_exceptions ADD CONSTRAINT fk_096
    FOREIGN KEY (workspace_id, host_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE meeting_types ADD CONSTRAINT fk_097
    FOREIGN KEY (workspace_id, host_principal_id, calendar_connection_id)
    REFERENCES calendar_connections (workspace_id, host_principal_id, id);
ALTER TABLE booking_links ADD CONSTRAINT fk_098
    FOREIGN KEY (workspace_id, host_principal_id, meeting_type_id)
    REFERENCES meeting_types (workspace_id, host_principal_id, id);
ALTER TABLE booking_links ADD CONSTRAINT fk_099
    FOREIGN KEY (workspace_id, bound_email_address_id)
    REFERENCES email_addresses (workspace_id, id);
ALTER TABLE booking_links ADD CONSTRAINT fk_100
    FOREIGN KEY (workspace_id, campaign_enrollment_id)
    REFERENCES campaign_enrollments (workspace_id, id);
ALTER TABLE bookings ADD CONSTRAINT fk_101
    FOREIGN KEY (workspace_id, host_principal_id, booking_link_id)
    REFERENCES booking_links (workspace_id, host_principal_id, id);
ALTER TABLE bookings ADD CONSTRAINT fk_102
    FOREIGN KEY (workspace_id, guest_email_address_id)
    REFERENCES email_addresses (workspace_id, id);
ALTER TABLE booking_changes ADD CONSTRAINT fk_103
    FOREIGN KEY (workspace_id, host_principal_id, booking_id)
    REFERENCES bookings (workspace_id, host_principal_id, id);
ALTER TABLE booking_changes ADD CONSTRAINT fk_104
    FOREIGN KEY (workspace_id, host_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE booking_changes ADD CONSTRAINT fk_105
    FOREIGN KEY (workspace_id, requested_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE booking_slot_holds ADD CONSTRAINT fk_106
    FOREIGN KEY (workspace_id, host_principal_id, booking_id)
    REFERENCES bookings (workspace_id, host_principal_id, id);
ALTER TABLE booking_slot_holds ADD CONSTRAINT fk_107
    FOREIGN KEY (workspace_id, booking_change_id)
    REFERENCES booking_changes (workspace_id, id);
ALTER TABLE booking_slot_holds ADD CONSTRAINT fk_108
    FOREIGN KEY (workspace_id, host_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE booking_manage_tokens ADD CONSTRAINT fk_109
    FOREIGN KEY (workspace_id, booking_id)
    REFERENCES bookings (workspace_id, id);
ALTER TABLE approval_requests ADD CONSTRAINT fk_110
    FOREIGN KEY (workspace_id, requested_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE approval_requests ADD CONSTRAINT fk_111
    FOREIGN KEY (workspace_id, launch_id)
    REFERENCES campaign_launches (workspace_id, id);
ALTER TABLE approval_requests ADD CONSTRAINT fk_112
    FOREIGN KEY (workspace_id, outbound_message_id)
    REFERENCES outbound_messages (workspace_id, id);
ALTER TABLE approval_requests ADD CONSTRAINT fk_113
    FOREIGN KEY (workspace_id, booking_change_id)
    REFERENCES booking_changes (workspace_id, id);
ALTER TABLE approval_requests ADD CONSTRAINT fk_114
    FOREIGN KEY (workspace_id, suppression_id)
    REFERENCES suppressions (workspace_id, id);
ALTER TABLE approval_decisions ADD CONSTRAINT fk_115
    FOREIGN KEY (workspace_id, approval_request_id)
    REFERENCES approval_requests (workspace_id, id);
ALTER TABLE approval_decisions ADD CONSTRAINT fk_116
    FOREIGN KEY (workspace_id, decided_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE agent_actions ADD CONSTRAINT fk_117
    FOREIGN KEY (workspace_id, agent_run_id)
    REFERENCES agent_runs (workspace_id, id);
ALTER TABLE agent_actions ADD CONSTRAINT fk_118
    FOREIGN KEY (workspace_id, approval_request_id)
    REFERENCES approval_requests (workspace_id, id);
ALTER TABLE notes ADD CONSTRAINT fk_119
    FOREIGN KEY (workspace_id, company_id)
    REFERENCES companies (workspace_id, id);
ALTER TABLE notes ADD CONSTRAINT fk_120
    FOREIGN KEY (workspace_id, contact_id)
    REFERENCES contacts (workspace_id, id);
ALTER TABLE notes ADD CONSTRAINT fk_121
    FOREIGN KEY (workspace_id, created_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE notes ADD CONSTRAINT fk_122
    FOREIGN KEY (workspace_id, mail_account_id, thread_id)
    REFERENCES email_threads (workspace_id, mail_account_id, id);
ALTER TABLE tasks ADD CONSTRAINT fk_123
    FOREIGN KEY (workspace_id, owner_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE tasks ADD CONSTRAINT fk_124
    FOREIGN KEY (workspace_id, company_id)
    REFERENCES companies (workspace_id, id);
ALTER TABLE tasks ADD CONSTRAINT fk_125
    FOREIGN KEY (workspace_id, contact_id)
    REFERENCES contacts (workspace_id, id);
ALTER TABLE tasks ADD CONSTRAINT fk_126
    FOREIGN KEY (workspace_id, thread_id)
    REFERENCES email_threads (workspace_id, id);
ALTER TABLE activities ADD CONSTRAINT fk_127
    FOREIGN KEY (workspace_id, company_id)
    REFERENCES companies (workspace_id, id);
ALTER TABLE activities ADD CONSTRAINT fk_128
    FOREIGN KEY (workspace_id, contact_id)
    REFERENCES contacts (workspace_id, id);
ALTER TABLE activities ADD CONSTRAINT fk_129
    FOREIGN KEY (workspace_id, booking_id)
    REFERENCES bookings (workspace_id, id);
ALTER TABLE activities ADD CONSTRAINT fk_130
    FOREIGN KEY (workspace_id, actor_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE activities ADD CONSTRAINT fk_131
    FOREIGN KEY (workspace_id, mail_account_id)
    REFERENCES mail_accounts (workspace_id, id);
ALTER TABLE activities ADD CONSTRAINT fk_132
    FOREIGN KEY (workspace_id, mail_account_id, thread_id)
    REFERENCES email_threads (workspace_id, mail_account_id, id);
ALTER TABLE stored_objects ADD CONSTRAINT fk_133
    FOREIGN KEY (workspace_id, created_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE message_attachments ADD CONSTRAINT fk_134
    FOREIGN KEY (workspace_id, email_message_id)
    REFERENCES email_messages (workspace_id, id);
ALTER TABLE message_attachments ADD CONSTRAINT fk_135
    FOREIGN KEY (workspace_id, stored_object_id)
    REFERENCES stored_objects (workspace_id, id);
ALTER TABLE import_jobs ADD CONSTRAINT fk_136
    FOREIGN KEY (workspace_id, stored_object_id)
    REFERENCES stored_objects (workspace_id, id);
ALTER TABLE import_jobs ADD CONSTRAINT fk_137
    FOREIGN KEY (workspace_id, requested_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE import_rows ADD CONSTRAINT fk_138
    FOREIGN KEY (workspace_id, import_job_id)
    REFERENCES import_jobs (workspace_id, id);
ALTER TABLE import_rows ADD CONSTRAINT fk_139
    FOREIGN KEY (workspace_id, company_id)
    REFERENCES companies (workspace_id, id);
ALTER TABLE import_rows ADD CONSTRAINT fk_140
    FOREIGN KEY (workspace_id, contact_id)
    REFERENCES contacts (workspace_id, id);
ALTER TABLE webhook_events ADD CONSTRAINT fk_141
    FOREIGN KEY (workspace_id, connection_id)
    REFERENCES integration_connections (workspace_id, id);
ALTER TABLE sync_cursors ADD CONSTRAINT fk_142
    FOREIGN KEY (workspace_id, mail_account_id)
    REFERENCES mail_accounts (workspace_id, id);
ALTER TABLE jobs ADD CONSTRAINT fk_143
    FOREIGN KEY (workspace_id, requested_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE jobs ADD CONSTRAINT fk_144
    FOREIGN KEY (workspace_id, source_credential_id)
    REFERENCES service_credentials (workspace_id, id);
ALTER TABLE processed_events ADD CONSTRAINT fk_145
    FOREIGN KEY (workspace_id, event_id)
    REFERENCES outbox_events (workspace_id, id);
ALTER TABLE audit_logs ADD CONSTRAINT fk_146
    FOREIGN KEY (workspace_id, actor_principal_id)
    REFERENCES principals (workspace_id, id);
ALTER TABLE audit_logs ADD CONSTRAINT fk_147
    FOREIGN KEY (workspace_id, requested_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE audit_logs ADD CONSTRAINT fk_148
    FOREIGN KEY (workspace_id, approved_by)
    REFERENCES principals (workspace_id, id);
ALTER TABLE audit_logs ADD CONSTRAINT fk_149
    FOREIGN KEY (workspace_id, job_id)
    REFERENCES jobs (workspace_id, id);
ALTER TABLE mail_accounts ADD CONSTRAINT fk_150
    FOREIGN KEY (workspace_id, connection_id, provider)
    REFERENCES integration_connections (workspace_id, id, provider);
ALTER TABLE delivery_channels ADD CONSTRAINT fk_151
    FOREIGN KEY (workspace_id, connection_id, provider)
    REFERENCES integration_connections (workspace_id, id, provider);
ALTER TABLE calendar_connections ADD CONSTRAINT fk_152
    FOREIGN KEY (workspace_id, connection_id, provider)
    REFERENCES integration_connections (workspace_id, id, provider);
ALTER TABLE campaign_revisions ADD CONSTRAINT fk_153
    FOREIGN KEY (workspace_id, delivery_channel_id, purpose)
    REFERENCES delivery_channels (workspace_id, id, purpose);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_154
    FOREIGN KEY (workspace_id, delivery_channel_id, purpose)
    REFERENCES delivery_channels (workspace_id, id, purpose);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_155
    FOREIGN KEY (workspace_id, to_email_address_id, purpose, communication_basis_id)
    REFERENCES communication_bases (workspace_id, email_address_id, purpose, id);
ALTER TABLE unsubscribe_tokens ADD CONSTRAINT fk_156
    FOREIGN KEY (workspace_id, email_address_id, outbound_message_id)
    REFERENCES outbound_messages (workspace_id, to_email_address_id, id);
ALTER TABLE suppressions ADD CONSTRAINT fk_157
    FOREIGN KEY (workspace_id, email_address_id, source_outbound_id)
    REFERENCES outbound_messages (workspace_id, to_email_address_id, id);
ALTER TABLE booking_slot_holds ADD CONSTRAINT fk_158
    FOREIGN KEY (workspace_id, host_principal_id, booking_change_id)
    REFERENCES booking_changes (workspace_id, host_principal_id, id);
ALTER TABLE principals ADD CONSTRAINT fk_159
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE member_invitations ADD CONSTRAINT fk_160
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE user_sessions ADD CONSTRAINT fk_161
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE service_credentials ADD CONSTRAINT fk_162
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE integration_connections ADD CONSTRAINT fk_163
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE mail_accounts ADD CONSTRAINT fk_164
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE sender_identities ADD CONSTRAINT fk_165
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE mail_account_grants ADD CONSTRAINT fk_166
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE delivery_channels ADD CONSTRAINT fk_167
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE channel_reviews ADD CONSTRAINT fk_168
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE companies ADD CONSTRAINT fk_169
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE contacts ADD CONSTRAINT fk_170
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE email_addresses ADD CONSTRAINT fk_171
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE contact_emails ADD CONSTRAINT fk_172
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE communication_bases ADD CONSTRAINT fk_173
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE agent_runs ADD CONSTRAINT fk_174
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE lead_proposals ADD CONSTRAINT fk_175
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE research_evidence ADD CONSTRAINT fk_176
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE email_templates ADD CONSTRAINT fk_177
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE template_versions ADD CONSTRAINT fk_178
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE campaigns ADD CONSTRAINT fk_179
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE campaign_revisions ADD CONSTRAINT fk_180
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE campaign_steps ADD CONSTRAINT fk_181
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE campaign_launches ADD CONSTRAINT fk_182
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE campaign_enrollments ADD CONSTRAINT fk_183
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE email_threads ADD CONSTRAINT fk_184
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE message_drafts ADD CONSTRAINT fk_185
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE outbound_messages ADD CONSTRAINT fk_186
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE email_messages ADD CONSTRAINT fk_187
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE message_participants ADD CONSTRAINT fk_188
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE thread_read_states ADD CONSTRAINT fk_189
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE send_attempts ADD CONSTRAINT fk_190
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE reply_claims ADD CONSTRAINT fk_191
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE delivery_events ADD CONSTRAINT fk_192
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE suppressions ADD CONSTRAINT fk_193
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE suppression_events ADD CONSTRAINT fk_194
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE unsubscribe_tokens ADD CONSTRAINT fk_195
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE calendar_connections ADD CONSTRAINT fk_196
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE availability_rules ADD CONSTRAINT fk_197
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE availability_exceptions ADD CONSTRAINT fk_198
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE meeting_types ADD CONSTRAINT fk_199
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE booking_links ADD CONSTRAINT fk_200
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE bookings ADD CONSTRAINT fk_201
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE booking_changes ADD CONSTRAINT fk_202
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE booking_slot_holds ADD CONSTRAINT fk_203
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE booking_manage_tokens ADD CONSTRAINT fk_204
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE approval_requests ADD CONSTRAINT fk_205
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE approval_decisions ADD CONSTRAINT fk_206
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE agent_actions ADD CONSTRAINT fk_207
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE notes ADD CONSTRAINT fk_208
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE tasks ADD CONSTRAINT fk_209
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE activities ADD CONSTRAINT fk_210
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE stored_objects ADD CONSTRAINT fk_211
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE message_attachments ADD CONSTRAINT fk_212
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE import_jobs ADD CONSTRAINT fk_213
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE import_rows ADD CONSTRAINT fk_214
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE webhook_events ADD CONSTRAINT fk_215
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE sync_cursors ADD CONSTRAINT fk_216
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE jobs ADD CONSTRAINT fk_217
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE outbox_events ADD CONSTRAINT fk_218
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE processed_events ADD CONSTRAINT fk_219
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE idempotency_records ADD CONSTRAINT fk_220
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);
ALTER TABLE audit_logs ADD CONSTRAINT fk_221
    FOREIGN KEY (workspace_id)
    REFERENCES workspaces (id);

-- 조회·중복 방지 인덱스
CREATE UNIQUE INDEX contact_primary_email_uq ON contact_emails(workspace_id, contact_id) WHERE is_primary;
CREATE INDEX companies_domain_idx ON companies(workspace_id, domain) WHERE archived_at IS NULL;
CREATE INDEX companies_owner_idx ON companies(workspace_id, owner_principal_id, created_at DESC, id DESC) WHERE archived_at IS NULL;
CREATE INDEX contacts_company_idx ON contacts(workspace_id, company_id, id);
CREATE INDEX contact_emails_address_idx ON contact_emails(workspace_id, email_address_id);
CREATE INDEX communication_bases_lookup_idx ON communication_bases(workspace_id, email_address_id, purpose, status);
CREATE UNIQUE INDEX active_campaign_address_uq ON campaign_enrollments(workspace_id, email_address_id) WHERE status = 'active';
CREATE INDEX enrollment_due_idx ON campaign_enrollments(workspace_id, next_send_at) WHERE status = 'active';
CREATE UNIQUE INDEX outbound_step_uq ON outbound_messages(workspace_id, enrollment_id, step_id) WHERE enrollment_id IS NOT NULL;
CREATE UNIQUE INDEX outbound_source_draft_uq ON outbound_messages(workspace_id, source_draft_id) WHERE source_draft_id IS NOT NULL;
CREATE UNIQUE INDEX approval_launch_active_uq ON approval_requests(workspace_id, launch_id) WHERE launch_id IS NOT NULL AND status IN ('pending','approved');
CREATE UNIQUE INDEX approval_outbound_active_uq ON approval_requests(workspace_id, outbound_message_id) WHERE outbound_message_id IS NOT NULL AND status IN ('pending','approved');
CREATE UNIQUE INDEX approval_booking_active_uq ON approval_requests(workspace_id, booking_change_id) WHERE booking_change_id IS NOT NULL AND status IN ('pending','approved');
CREATE UNIQUE INDEX approval_suppression_active_uq ON approval_requests(workspace_id, suppression_id) WHERE suppression_id IS NOT NULL AND status IN ('pending','approved');
CREATE INDEX outbound_due_idx ON outbound_messages(workspace_id, not_before, id) WHERE status = 'queued';
CREATE UNIQUE INDEX provider_thread_uq ON email_threads(workspace_id, mail_account_id, provider_thread_id) WHERE provider_thread_id IS NOT NULL;
CREATE UNIQUE INDEX provider_message_uq ON email_messages(workspace_id, mail_account_id, provider_message_id) WHERE provider_message_id IS NOT NULL;
CREATE INDEX message_thread_time_idx ON email_messages(workspace_id, thread_id, message_at DESC, id DESC);
CREATE INDEX thread_inbox_idx ON email_threads(workspace_id, mail_account_id, status, updated_at DESC, id DESC);
CREATE UNIQUE INDEX reply_claim_active_uq ON reply_claims(workspace_id, mail_account_id, inbound_message_id) WHERE state IN ('reserved','submitted','accepted');
CREATE UNIQUE INDEX suppression_active_reason_uq ON suppressions(workspace_id, email_address_id, purpose_scope, reason) WHERE released_at IS NULL;
CREATE INDEX suppression_lookup_idx ON suppressions(workspace_id, email_address_id) WHERE released_at IS NULL;
CREATE INDEX pending_approval_idx ON approval_requests(workspace_id, expires_at, id) WHERE status = 'pending';
CREATE INDEX active_booking_idx ON bookings(workspace_id, host_principal_id, starts_at) WHERE status IN ('held','confirming','confirmed','canceling','unknown');
CREATE UNIQUE INDEX booking_active_hold_uq ON booking_slot_holds(workspace_id, booking_id) WHERE booking_id IS NOT NULL AND status IN ('held','confirming','confirmed','unknown');
CREATE UNIQUE INDEX booking_change_active_hold_uq ON booking_slot_holds(workspace_id, booking_change_id) WHERE booking_change_id IS NOT NULL AND status IN ('held','confirming','confirmed','unknown');
CREATE INDEX job_due_idx ON jobs(workspace_id, available_at, id) WHERE state IN ('queued','retry_wait');
CREATE INDEX job_lease_idx ON jobs(lease_expires_at) WHERE state = 'running';
CREATE INDEX outbox_pending_idx ON outbox_events(workspace_id, created_at, id) WHERE published_at IS NULL;
CREATE INDEX audit_time_idx ON audit_logs(workspace_id, occurred_at DESC, id DESC);
CREATE INDEX audit_target_idx ON audit_logs(workspace_id, target_type, target_id, occurred_at DESC);
CREATE INDEX research_company_idx ON research_evidence(workspace_id, company_id, observed_at DESC);
CREATE INDEX import_rows_status_idx ON import_rows(workspace_id, import_job_id, status, row_no);
ALTER TABLE booking_slot_holds ADD CONSTRAINT booking_slots_no_overlap
    EXCLUDE USING gist (
        workspace_id WITH =,
        host_principal_id WITH =,
        tstzrange(starts_at, ends_at, '[)') WITH &&
    ) WHERE (status IN ('held','confirming','confirmed','unknown'))
    DEFERRABLE INITIALLY IMMEDIATE;

-- runtime이 임의 수정할 수 없는 내용 스냅샷.
CREATE FUNCTION reject_template_version_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'template versions are immutable; create a new version'
        USING ERRCODE = '23514';
END;
$$;
CREATE TRIGGER template_versions_immutable
    BEFORE UPDATE ON template_versions
    FOR EACH ROW EXECUTE FUNCTION reject_template_version_change();

CREATE FUNCTION protect_outbound_snapshot() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF (to_jsonb(NEW) - ARRAY['status','not_before','accepted_at','provider_message_id','failure_code','version'])
       IS DISTINCT FROM
       (to_jsonb(OLD) - ARRAY['status','not_before','accepted_at','provider_message_id','failure_code','version']) THEN
        RAISE EXCEPTION 'outbound payload is immutable; create a new message'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER outbound_snapshot_immutable
    BEFORE UPDATE ON outbound_messages
    FOR EACH ROW EXECUTE FUNCTION protect_outbound_snapshot();

CREATE FUNCTION reject_self_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    req approval_requests%ROWTYPE;
    actor principals%ROWTYPE;
BEGIN
    SELECT * INTO req FROM approval_requests
      WHERE workspace_id = NEW.workspace_id AND id = NEW.approval_request_id
      FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'approval request not found' USING ERRCODE = '23503';
    END IF;
    SELECT * INTO actor FROM principals
      WHERE workspace_id = NEW.workspace_id AND id = NEW.decided_by;
    IF actor.id IS NULL OR actor.kind <> 'human' OR actor.status <> 'active'
       OR actor.role NOT IN ('admin','manager') THEN
        RAISE EXCEPTION 'active human approver required' USING ERRCODE = '23514';
    END IF;
    IF NEW.decided_by = req.requested_by THEN
        RAISE EXCEPTION 'self approval is forbidden' USING ERRCODE = '23514';
    END IF;
    IF req.status <> 'pending' OR req.expires_at <= clock_timestamp()
       OR req.snapshot_hash IS DISTINCT FROM NEW.snapshot_hash
       OR req.version <> NEW.version_at_decision THEN
        RAISE EXCEPTION 'stale or invalid approval' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER approval_decision_guard
    BEFORE INSERT ON approval_decisions
    FOR EACH ROW EXECUTE FUNCTION reject_self_approval();


-- Workspace RLS. runtime role에는 별도 최소 GRANT가 필요하다.

CREATE FUNCTION app_workspace_id() RETURNS UUID
LANGUAGE sql STABLE AS $$
    SELECT NULLIF(current_setting('app.workspace_id', true), '')::UUID
$$;

ALTER TABLE principals ENABLE ROW LEVEL SECURITY;
ALTER TABLE principals FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON principals
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE member_invitations ENABLE ROW LEVEL SECURITY;
ALTER TABLE member_invitations FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON member_invitations
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE user_sessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE user_sessions FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON user_sessions
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE service_credentials ENABLE ROW LEVEL SECURITY;
ALTER TABLE service_credentials FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON service_credentials
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE integration_connections ENABLE ROW LEVEL SECURITY;
ALTER TABLE integration_connections FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON integration_connections
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE mail_accounts ENABLE ROW LEVEL SECURITY;
ALTER TABLE mail_accounts FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON mail_accounts
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE sender_identities ENABLE ROW LEVEL SECURITY;
ALTER TABLE sender_identities FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON sender_identities
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE mail_account_grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE mail_account_grants FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON mail_account_grants
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE delivery_channels ENABLE ROW LEVEL SECURITY;
ALTER TABLE delivery_channels FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON delivery_channels
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE channel_reviews ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel_reviews FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON channel_reviews
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE companies ENABLE ROW LEVEL SECURITY;
ALTER TABLE companies FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON companies
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE contacts ENABLE ROW LEVEL SECURITY;
ALTER TABLE contacts FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON contacts
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE email_addresses ENABLE ROW LEVEL SECURITY;
ALTER TABLE email_addresses FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON email_addresses
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE contact_emails ENABLE ROW LEVEL SECURITY;
ALTER TABLE contact_emails FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON contact_emails
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE communication_bases ENABLE ROW LEVEL SECURITY;
ALTER TABLE communication_bases FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON communication_bases
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE agent_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE agent_runs FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON agent_runs
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE lead_proposals ENABLE ROW LEVEL SECURITY;
ALTER TABLE lead_proposals FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON lead_proposals
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE research_evidence ENABLE ROW LEVEL SECURITY;
ALTER TABLE research_evidence FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON research_evidence
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE email_templates ENABLE ROW LEVEL SECURITY;
ALTER TABLE email_templates FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON email_templates
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE template_versions ENABLE ROW LEVEL SECURITY;
ALTER TABLE template_versions FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON template_versions
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE campaigns ENABLE ROW LEVEL SECURITY;
ALTER TABLE campaigns FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON campaigns
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE campaign_revisions ENABLE ROW LEVEL SECURITY;
ALTER TABLE campaign_revisions FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON campaign_revisions
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE campaign_steps ENABLE ROW LEVEL SECURITY;
ALTER TABLE campaign_steps FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON campaign_steps
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE campaign_launches ENABLE ROW LEVEL SECURITY;
ALTER TABLE campaign_launches FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON campaign_launches
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE campaign_enrollments ENABLE ROW LEVEL SECURITY;
ALTER TABLE campaign_enrollments FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON campaign_enrollments
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE email_threads ENABLE ROW LEVEL SECURITY;
ALTER TABLE email_threads FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON email_threads
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE message_drafts ENABLE ROW LEVEL SECURITY;
ALTER TABLE message_drafts FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON message_drafts
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE outbound_messages ENABLE ROW LEVEL SECURITY;
ALTER TABLE outbound_messages FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON outbound_messages
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE email_messages ENABLE ROW LEVEL SECURITY;
ALTER TABLE email_messages FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON email_messages
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE message_participants ENABLE ROW LEVEL SECURITY;
ALTER TABLE message_participants FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON message_participants
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE thread_read_states ENABLE ROW LEVEL SECURITY;
ALTER TABLE thread_read_states FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON thread_read_states
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE send_attempts ENABLE ROW LEVEL SECURITY;
ALTER TABLE send_attempts FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON send_attempts
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE reply_claims ENABLE ROW LEVEL SECURITY;
ALTER TABLE reply_claims FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON reply_claims
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE delivery_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE delivery_events FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON delivery_events
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE suppressions ENABLE ROW LEVEL SECURITY;
ALTER TABLE suppressions FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON suppressions
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE suppression_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE suppression_events FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON suppression_events
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE unsubscribe_tokens ENABLE ROW LEVEL SECURITY;
ALTER TABLE unsubscribe_tokens FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON unsubscribe_tokens
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE calendar_connections ENABLE ROW LEVEL SECURITY;
ALTER TABLE calendar_connections FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON calendar_connections
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE availability_rules ENABLE ROW LEVEL SECURITY;
ALTER TABLE availability_rules FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON availability_rules
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE availability_exceptions ENABLE ROW LEVEL SECURITY;
ALTER TABLE availability_exceptions FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON availability_exceptions
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE meeting_types ENABLE ROW LEVEL SECURITY;
ALTER TABLE meeting_types FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON meeting_types
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE booking_links ENABLE ROW LEVEL SECURITY;
ALTER TABLE booking_links FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON booking_links
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE bookings ENABLE ROW LEVEL SECURITY;
ALTER TABLE bookings FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON bookings
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE booking_changes ENABLE ROW LEVEL SECURITY;
ALTER TABLE booking_changes FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON booking_changes
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE booking_slot_holds ENABLE ROW LEVEL SECURITY;
ALTER TABLE booking_slot_holds FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON booking_slot_holds
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE booking_manage_tokens ENABLE ROW LEVEL SECURITY;
ALTER TABLE booking_manage_tokens FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON booking_manage_tokens
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE approval_requests ENABLE ROW LEVEL SECURITY;
ALTER TABLE approval_requests FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON approval_requests
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE approval_decisions ENABLE ROW LEVEL SECURITY;
ALTER TABLE approval_decisions FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON approval_decisions
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE agent_actions ENABLE ROW LEVEL SECURITY;
ALTER TABLE agent_actions FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON agent_actions
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
ALTER TABLE notes FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON notes
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE tasks ENABLE ROW LEVEL SECURITY;
ALTER TABLE tasks FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON tasks
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE activities ENABLE ROW LEVEL SECURITY;
ALTER TABLE activities FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON activities
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE stored_objects ENABLE ROW LEVEL SECURITY;
ALTER TABLE stored_objects FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON stored_objects
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE message_attachments ENABLE ROW LEVEL SECURITY;
ALTER TABLE message_attachments FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON message_attachments
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE import_jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE import_jobs FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON import_jobs
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE import_rows ENABLE ROW LEVEL SECURITY;
ALTER TABLE import_rows FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON import_rows
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE webhook_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE webhook_events FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON webhook_events
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE sync_cursors ENABLE ROW LEVEL SECURITY;
ALTER TABLE sync_cursors FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON sync_cursors
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE jobs FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON jobs
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE outbox_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE outbox_events FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON outbox_events
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE processed_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE processed_events FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON processed_events
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE idempotency_records ENABLE ROW LEVEL SECURITY;
ALTER TABLE idempotency_records FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON idempotency_records
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
ALTER TABLE audit_logs ENABLE ROW LEVEL SECURITY;
ALTER TABLE audit_logs FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON audit_logs
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());
COMMIT;
```

## A.5 RLS·DB role 배포 계약

이 DDL은 runtime용 DB 사용자 생성이나 광범위한 GRANT를 자동 수행하지 않는다. 역할은 `migration_owner`, `api_runtime`, `worker_runtime`, `report_reader`, `retention_operator`로 나누고 실제 배포 계정에서 필요한 권한만 부여한다. runtime 계정은 schema CREATE·임의 함수 생성·BYPASSRLS·테이블 소유권을 갖지 않는다.

`users`는 로그인 bootstrap을 위한 별도 저장소 경로에서만 조회하며 공용 CRUD로 노출하지 않는다. MVP는 단일 Workspace이므로 서버 배포 설정의 `DEFAULT_WORKSPACE_ID`를 사용해 세션·공개 토큰 검증용 트랜잭션의 Workspace를 정할 수 있다. 이 값을 클라이언트가 지정하도록 하지 않는다. 다중 Workspace를 실제 출시하려면 인증 전 membership·opaque token lookup 경계를 별도 최소권한 함수 또는 registry로 설계해야 한다. **현재 RLS 컬럼만 있다고 SaaS 다중 조직 운영이 구현된 것으로 보지 않는다.**

트랜잭션 경계의 예시는 다음과 같다.

```sql
BEGIN;
SELECT set_config('app.workspace_id', $1::text, true);
-- $1은 검증된 인증 컨텍스트 또는 서버 고정 Workspace다.
SELECT id, name, version
  FROM companies
 WHERE workspace_id = $1 AND id = $2 AND archived_at IS NULL;
COMMIT;
```

위의 `$1`, `$2`는 SQLx bind parameter이며 문자열 연결로 SQL을 만들지 않는다. 연결 풀에 session-level `SET`이 남지 않게 반드시 트랜잭션 local 설정을 사용한다. Worker도 한 작업의 Workspace를 검증해 설정하며 전체 Workspace를 무조건 보는 슈퍼 계정으로 동작하지 않는다.

`audit_logs`, `approval_decisions`, `suppression_events`, `send_attempts`의 완료 기록은 일반 runtime에서 DELETE·임의 UPDATE를 허용하지 않는다. 개인정보 보존기간 만료 삭제는 별도의 감사된 유지보수 경로로 수행한다. 보안상 제한된 기록을 API의 일반 삭제 endpoint로 노출하지 않는다.

## A.6 SQL 제약 외에 필수인 트랜잭션 규칙

### A.6.1 회사·담당자 갱신

`UPDATE ... WHERE workspace_id = ? AND id = ? AND version = ?`로 낙관적 잠금을 적용한다. 변경 행이 0이면 먼저 권한 내에서 리소스 존재 여부를 구분한 뒤 404 또는 412를 반환한다. Member 소유자 조건을 쿼리에 포함한다. owner 변경은 별도 권한 명령으로만 처리한다.

### A.6.2 캠페인 동결

revision과 launch 행을 잠그고 `building` 상태를 확인한다. 모든 대상·개인화 내용·단계 발송 스냅샷이 생성된 상태에서 manifest를 계산한다. 해당 트랜잭션에서 revision을 frozen, launch를 frozen으로 전환하고 approval request·audit·outbox를 기록한다. `frozen` 부모의 step INSERT·UPDATE·DELETE, enrollment 대상 변경, launch manifest 변경은 저장소 계층과 DB 제한 함수/추가 guard trigger로 거절해야 한다. 위 DDL에 있는 outbound·template trigger만으로 부모 구조 동결까지 완성됐다고 간주하지 않는다.

금지되는 변경에는 recipient 이메일 교체, launch의 요청자 교체, approval target·snapshot hash 교체도 포함된다. 승인 거절 후 수정은 동일 approval 행을 재사용하지 않고 새 revision·launch·request를 만든다. 상태 전이에 필요한 변경만 전용 Use Case에서 허용한다.

### A.6.3 승인

승인 request를 `FOR UPDATE`로 잠근다. pending·만료·version·snapshot hash·요청자와 승인자 다름을 확인한다. 사람/역할 검사는 DDL trigger에도 있지만, 메일함 `can_approve`, 채널·발송 근거·현재 principal 상태·권한 scope는 Application Service에서 추가 검사한다. decision INSERT, request 상태 변경, launch 또는 outbound 활성화, jobs·audit·outbox 기록을 같은 트랜잭션에 커밋한다. 이 중 하나라도 실패하면 승인 결과만 남기지 않는다.

한 번의 승인 결정이 campaign의 모든 미래 revision이나 모든 새 메시지를 승인하지 않는다. `approval_requests.snapshot_hash`와 실제 실행 manifest가 일치해야 한다.

### A.6.4 발송 허가와 수신 거부

같은 `(workspace, email_address_id)`에 대해 일관된 잠금 순서를 사용한다. 단일 주소 행의 `FOR UPDATE` 또는 통일된 advisory lock 전략을 선택한다. 다른 테이블을 먼저 잠그는 경로를 섞어 교착을 만들지 않는다.

발송 worker는 잠금 아래 활성 suppression·communication basis·principal 권한·승인·캠페인 상태를 검사하고, send attempt를 시작 상태로 기록하면서 `submitting` 경계를 커밋한다. 수신 거부는 같은 잠금 규약으로 suppression·event를 기록한다. 메시지의 외부 제출과 DB commit은 분산 원자적 트랜잭션이 아니므로 이미 허가된 in-flight 제출은 별도로 추적한다.

수신 거부가 큰 캠페인의 모든 recipient 행을 갱신할 때까지 응답을 지연시키지 않는다. suppression 커밋을 먼저 내구성 있게 완료하고 후속 일괄 상태 갱신은 job으로 수행한다. 미갱신 recipient도 최종 suppression 체크에서 차단돼야 한다.

### A.6.5 공동 Inbox 회신

thread를 잠그고 `expected_last_inbound_id`를 확인한 뒤 활성 reply claim을 생성한다. claim·outbound·job·audit가 한 번에 기록돼야 한다. 이전 claim이 accepted이면 같은 수신 메일에 대한 자동 중복 회신은 막는다. 추가 안내가 정말 필요한 경우에는 기존 발송 내용을 확인하고 명시적으로 별도 검토를 거친 신규 메시지로 처리한다. 임의 retry로 claim을 해제하지 않는다.

### A.6.6 예약 점유

**가용 슬롯 계산과 충돌의 원본은 `booking_slot_holds`다.** `bookings.occupied_start/end`는 화면·정합성 확인용 스냅샷이며 별도 원본이 아니다. 최초 예약에서는 booking과 버퍼 포함 hold를 같은 트랜잭션에 삽입한다. 예약 상태 변경 시 hold 상태도 함께 변경한다. 미래 예약을 `bookings`에만 넣고 hold 생성에 실패한 채 커밋하는 경로를 금지한다.

일정 변경에서 새 범위가 기존 자기 예약과 **겹치지 않으면** `booking_change_id`에 묶인 새 hold를 먼저 잡는다. 자기 예약과 **겹치면** 기존 hold를 두 범위를 포함하는 하나의 합집합 구간으로 확대해 확보하고 원래 범위를 booking change에 보관한다. 자신을 상대로 EXCLUDE 충돌이 나는 두 개의 겹친 hold를 만들지 않는다. 외부 수정 성공 후 새 범위로 축소하고, 확실한 실패이면 원래 범위로 복원한다. unknown이면 보수적으로 점유를 유지한다. 다른 예약과 겹치면 확대 또는 새 hold INSERT가 실패한다.

두 hold를 교체해야 하는 경우 같은 트랜잭션 안에서 제약을 일시 지연시킨 뒤 최종 상태에서 겹치지 않게 커밋할 수 있다. 사용 중인 제약의 `DEFERRABLE` 동작은 실제 PostgreSQL 테스트로 검증한다. 임의로 제약을 끈 채 영구 충돌을 남기지 않는다.

동일 booking의 동시 reschedule/cancel은 booking의 `version`·행 잠금으로 직렬화한다. host는 활성 human이어야 하고, Calendar 연결·미팅 유형·링크의 host가 같아야 한다. DB FK로 확인되는 관계 외에 human 여부·사용 가능 상태를 서버에서 검사한다.

### A.6.7 Job 획득

```sql
BEGIN;
SELECT set_config('app.workspace_id', $1::text, true);
WITH candidates AS (
    SELECT id
      FROM jobs
     WHERE workspace_id = $1
       AND state IN ('queued', 'retry_wait')
       AND available_at <= now()
     ORDER BY available_at, id
     LIMIT 20
     FOR UPDATE SKIP LOCKED
)
UPDATE jobs AS j
   SET state = 'running',
       lease_owner = $2,
       lease_token = gen_random_uuid(),
       lease_expires_at = now() + interval '60 seconds',
       attempt_count = attempt_count + 1
  FROM candidates AS c
 WHERE j.workspace_id = $1 AND j.id = c.id
RETURNING j.*;
COMMIT;
```

리스 시간 60초·배치 20건은 설계 예시다. 작업 길이와 heartbeat에 맞게 조정한다. 위 쿼리는 네트워크 호출 이전의 작업 획득만 처리하며, `submitting` 메일이 포함된 job을 리스 만료만으로 다시 보내는 쿼리가 아니다. 완료 UPDATE에는 `WHERE lease_token = 획득한_token`을 추가한다.

## A.7 초기 seed와 관리 정책

최초 Workspace·Admin·Worker principal만 보안된 bootstrap으로 생성한다. `emergency_stop=true`, 발송 채널 `enabled=false`로 시작한다. Admin에 공용 메일함 can_read를 자동 부여하지 않고 연결 과정에서 명시적으로 부여·감사한다. 테스트 Agent는 CRM 조회·후보 제안·초안 생성 scope만 가진다.

last active Admin을 비활성화하거나 자기 권한만 남은 상태에서 조직 운영이 잠기는 변경은 거절한다. 이 규칙은 동일 Workspace의 principal 변경 트랜잭션 안에서 검사한다. 퇴사자 owner·thread·meeting host의 재배정 및 대기 job 처리를 완료한 후 비활성화할 수 있는 관리 화면을 제공한다.

## A.8 추가 DB 테스트

FK 정적 검사 외에 실제 DB에서 다음을 검증한다: 빈 DB 마이그레이션·rollback/reapply, 모든 FK 위반 입력, RLS 설정 없음·다른 Workspace·owner/role, 승인 guard의 동시 INSERT, 큰 approval manifest 동결, 같은 주소 활성 campaign 경쟁, EXCLUDE와 DST·buffer·reschedule, worker lease fencing, 누락된 index에 따른 실행 계획, 백업 복원 후 unknown 조정.


---

<a id="sources"></a>
# 부록 C. 용어·공식 출처

## C.1 용어

| 용어 | 이 문서에서의 의미 |
|---|---|
| Workspace | 한 발신 조직의 내부 업무·권한 경계 |
| Principal | 직원·에이전트·시스템을 함께 나타내는 작업 주체 |
| Mail account | 답장을 수신하고 대화 이력을 동기화하는 팀 공용 메일함 |
| Delivery channel | 발송 목적·공급자·발신 주소·허용 정책이 묶인 전송 경로 |
| Communication basis | 특정 주소에 특정 목적으로 연락할 근거와 그 증빙 |
| Suppression | 수신 거부·불만·반송 등으로 발송을 막는 정책 기록 |
| Draft | 아직 수정할 수 있는 내용 |
| Snapshot | 실제 실행 범위를 고정한 내용·대상·설정 |
| Campaign revision | 특정 캠페인 구성의 버전 |
| Campaign launch | 수신자·내용을 동결하고 승인하는 한 번의 실행 묶음 |
| Enrollment | 한 실행 묶음에 참여한 고유 이메일 주소와 진행 상태 |
| Accepted | 공급자가 제출 요청을 수락했다고 확인된 상태 |
| Delivered | 공급자가 실제 전달 이벤트를 제공한 경우의 관측 상태; 읽음과 다름 |
| Unknown | 외부 영향이 있었는지 확정할 수 없어 대조가 필요한 상태 |
| Idempotency | 동일 명령의 재호출이 같은 결과를 참조하도록 하는 성질 |
| Outbox | 업무 변경과 같은 DB 트랜잭션에 기록하는 후속 처리 이벤트 |
| Fencing token | 오래된 워커가 새 실행 상태를 덮어쓰지 못하게 하는 실행 식별자 |
| MCP | 에이전트에 도구·리소스를 제공하는 통신 규약; 자동 조사 엔진 그 자체는 아님 |
| Preflight | 실행 가능성의 현재 사전 점검; 실제 발송 허가증은 아님 |
| Release gate | 실계정·계약·정책·테스트 증거를 확인한 뒤 활성화하는 출시 조건 |

## C.2 출처 사용 원칙

아래 자료는 모두 **2026-10-09에 조회한 공식 공급자 문서·표준·공식 법령**이다. 문서·정책은 이후 변경될 수 있다. 버전을 명시한 표준은 본 프로젝트의 호환성 기준이며 최신 버전이라고 단정하지 않는다. 구현 시 선택한 버전과 공급자 계정에서 재확인한다.

제품 범위, 기본값, 상태 모델, 테이블·API 구조, 운영 목표와 개발 순서는 이 프로젝트의 설계 결정이다. 공식 문서가 이 설계 전체를 승인하거나 특정 발송 대상의 적법성을 인증한 것은 아니다.

<a id="s01"></a>
### S01. Zoho Mail Usage Policy

- 공식 문서: <https://www.zoho.com/mail/help/usage-policy.html>
- 조회한 페이지의 업데이트 표기: 2026-09-02.
- 사용 근거: 정상 업무 메일과 마케팅·자동화·트랜잭션 발송의 사용 정책 구분. 자동 캠페인 발송 경로를 그대로 Zoho Mail로 확정하지 않는 근거.

<a id="s02"></a>
### S02. Zoho Mail — Send an Email

- 공식 문서: <https://www.zoho.com/mail/help/api/post-send-an-email.html>
- 사용 근거: 계정 단위 발송 endpoint, OAuth scope, 인증 계정에 대응하는 발신 주소와 메시지 필드. API 존재가 사용 목적 허용을 의미하지 않음.

<a id="s03"></a>
### S03. Zoho Mail — Email Messages API

- 공식 문서: <https://www.zoho.com/mail/help/api/email-api.html>
- 사용 근거: 메시지 조회·검색·헤더·내용·첨부·회신 기능의 공식 API 목록.

<a id="s04"></a>
### S04. Zoho Mail — Webhooks

- 공식 문서: <https://www.zoho.com/mail/help/dev-platform/webhook.html>
- 사용 근거: 수신 메일 관련 외부 알림과 서명 검증. 세부 payload·재전송·이벤트 범위는 실제 계정 검증 대상으로 분리.

<a id="s05"></a>
### S05. Zoho Calendar — Free/Busy API

- 공식 문서: <https://www.zoho.com/calendar/help/api/freebusy-api.html>
- 사용 근거: 사용자·참석자 가용성 조회 기능. 실제 조회 범위는 캘린더 권한·공유 설정에 따라 검증.

<a id="s06"></a>
### S06. Zoho Calendar — Create a New Event

- 공식 문서: <https://www.zoho.com/calendar/help/api/post-create-event.html>
- 사용 근거: Calendar 이벤트 생성, 참석자·시간대·알림 관련 필드.

<a id="s07"></a>
### S07. PostgreSQL 17 — Row Security Policies

- 공식 문서: <https://www.postgresql.org/docs/17/ddl-rowsecurity.html>
- 사용 근거: RLS 정책, 테이블 소유자·우회 권한, FORCE ROW LEVEL SECURITY 고려.

<a id="s08"></a>
### S08. PostgreSQL 17 — Range Types

- 공식 문서: <https://www.postgresql.org/docs/17/rangetypes.html>
- 사용 근거: timestamp 범위와 exclusion constraint를 이용한 겹침 방지 설계.

<a id="s09"></a>
### S09. PostgreSQL 17 — SELECT

- 공식 문서: <https://www.postgresql.org/docs/17/sql-select.html>
- 사용 근거: 행 잠금·SKIP LOCKED를 이용한 큐 소비 패턴과 일관성 제한.

<a id="s10"></a>
### S10. RFC 8058 — Signaling One-Click Functionality for List Email Headers

- 공식 표준: <https://www.rfc-editor.org/rfc/rfc8058.html>
- 사용 근거: 본문 링크와 구분되는 one-click POST, List-Unsubscribe 계열 헤더, 서명·인증 전제.

<a id="s11"></a>
### S11. RFC 9457 — Problem Details for HTTP APIs

- 공식 표준: <https://www.rfc-editor.org/rfc/rfc9457.html>
- 사용 근거: HTTP 오류 응답의 기본 구조와 확장 필드.

<a id="s12"></a>
### S12. MCP 2025-11-25 — Authorization

- 공식 명세: <https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization>
- 사용 근거: 원격 MCP OAuth 기반 인증, protected resource metadata, discovery와 scope. 이 버전은 호환성 기준으로 지정한 것이다.

<a id="s13"></a>
### S13. MCP 2025-11-25 — Tools

- 공식 명세: <https://modelcontextprotocol.io/specification/2025-11-25/server/tools>
- 사용 근거: inputSchema·outputSchema, structuredContent, tool annotations, 업무 오류 표현.

<a id="s14"></a>
### S14. MCP 2025-11-25 — Transports

- 공식 명세: <https://modelcontextprotocol.io/specification/2025-11-25/basic/transports>
- 사용 근거: Streamable HTTP·stdio, Origin 검증, 세션·연결과 작업 완료의 구분.

<a id="s15"></a>
### S15. MCP — Security Best Practices

- 공식 문서: <https://modelcontextprotocol.io/docs/2026-07-28/tutorials/security/security_best_practices>
- 사용 근거: 인증·토큰·대리 권한·외부 콘텐츠가 결합될 때의 보안 경계 검토.

<a id="s16"></a>
### S16. Next.js — Authentication

- 공식 문서: <https://nextjs.org/docs/app/guides/authentication>
- 사용 근거: UI 차원의 분기와 서버 데이터 접근·변경 권한 검사를 구분하는 인증·인가 설계.

<a id="s17"></a>
### S17. 국가법령정보센터 — 정보통신망법 제50조

- 공식 법령: <https://www.law.go.kr/LSW/lsLawLinkInfo.do?chrClsCd=010202&lsId=000030&lsJoLnkSeq=1000688185&print=print>
- 조회 페이지 시행 표기: 2026-10-02.
- 사용 근거: 광고성 정보 사전 동의·예외, 수신 거부·철회, 표기·처리 및 자동 연락처 관련 제한. 구체적 적용은 발송 대상·내용·관할에 따라 검토.

<a id="s18"></a>
### S18. Axum 공식 crate 문서

- 공식 문서: <https://docs.rs/axum/latest/axum/>
- 사용 근거: Rust HTTP 프레임워크 선택의 참고. 실제 프로젝트 버전은 호환성 검사 후 lockfile에 고정.

---

## C.3 문서와 실제 구현의 경계

이 문서에서 완료한 것은 **요구사항 정리, 설계 선택, 데이터 관계·기준 DDL, 화면·권한·API·MCP 계약, 예외·운영·테스트·출시 조건의 명세화**다.

아직 수행하지 않은 것은 서비스 코드 구현·빌드, PostgreSQL 실제 migration·동시성·RLS 실행 테스트, Zoho 실계정 연결·발송·캘린더 테스트, 공급자 계약 승인, 법률 검토, 운영 성능 측정·백업 복원이다. 이 항목들은 각각의 검증 단계와 출시 게이트를 통과해야 한다.

