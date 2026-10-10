# Outreach OS — REST API·MCP 상세 명세 v1.0

> 2026-10-09 / 전체 설계서 부록 B의 독립 분리판. 요청·응답은 합성 예시이며 실제 API 호출 결과가 아니다.

---

<a id="contract-spec"></a>
# 부록 B. REST API·MCP 상세 계약

## B.1 공통 타입과 규칙

이 부록의 UUID·이메일·기업명·URL은 **설계 예시**다. 실제 계정·실제 고객·실제 승인 결과를 나타내지 않는다. 경로의 `{id}` 등은 path parameter다. 모든 내부 경로에는 `/api/v1`을 붙이고, 공개 경로는 `/public/v1` 그대로 사용한다.

### B.1.1 공통 필드

| 타입 | 형식·제약 |
|---|---|
| `ResourceId` | UUID 문자열 |
| `ProviderId` | 공급자가 준 문자열 그대로; JSON 숫자 변환 금지 |
| `Timestamp` | RFC 3339 UTC, 예: `2026-10-12T01:00:00Z` |
| `Timezone` | 유효한 IANA timezone, 예: `Asia/Seoul` |
| `Hash` | 응답은 SHA-256 hex 64자; DB 저장은 BYTEA |
| `Version` | 양의 정수, ETag는 문자열 `"3"` |
| `Purpose` | `correspondence`, `marketing`, `transactional` |
| `Page` | `next_cursor: string|null`, `has_more: boolean` |
| `AllowedActions` | 현재 UI에 표시 가능한 action 문자열 목록 |

전역 입력 제한 제안은 JSON 본문 1MB, 단일 문자열 본문 200KB, 목록 limit 최대 200, 리서치 후보 배치 100건, CSV 20MB·10만 행이다. 공급자 첨부 한도와 앱 한도는 작은 쪽을 사용한다. 이러한 값은 정책 설정과 테스트를 통해 조정하며 공급자의 실제 한도라고 표시하지 않는다.

서버는 unknown field를 거절하는 DTO를 기본으로 한다. 시스템 소유 필드인 `workspace_id`, `created_by`, `approved_by`, `status=accepted`, `provider_message_id`, `is_verified`를 생성 요청에서 받지 않는다. 패치는 문서화된 필드만 허용한다.

### B.1.2 성공·오류

```json
{
  "data": {
    "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    "version": 3
  },
  "meta": {
    "request_id": "req_01_demo"
  }
}
```

```json
{
  "type": "urn:outreach:problem:recipient-suppressed",
  "title": "Recipient is suppressed",
  "status": 422,
  "code": "RECIPIENT_SUPPRESSED",
  "detail": "해당 주소는 조직 전체 마케팅 발송이 차단되어 있습니다.",
  "instance": "/api/v1/outbound-messages/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa/submit",
  "request_id": "req_02_demo",
  "retryable": false,
  "violations": [
    {
      "field": "to_email_address_id",
      "code": "WORKSPACE_MARKETING_SUPPRESSION"
    }
  ]
}
```

### B.1.3 멱등성 계약

`Idempotency-Key`는 클라이언트가 생성한 추측 불가 문자열을 권장하며 128자 이내로 제한한다. 적용 범위는 `(workspace, 인증 주체 또는 공개 capability, method+canonical path, key)`다. URI의 공개 원문 토큰은 scope나 로그에 그대로 남기지 않는다.

동일 키·동일 의미 요청은 기존 리소스를 반환한다. 동일 키·다른 의미 요청은 `409 IDEMPOTENCY_KEY_REUSED`다. 처리 중 중복은 원 작업 ID가 있으면 202, 아직 안전하게 응답할 수 없으면 `409 REQUEST_IN_PROGRESS`와 조회 경로를 반환한다. 네트워크 재시도 시 새 키를 만들지 않는다.

제안 보존 기간은 일반 응답 72시간 이상이다. 만료 후에도 발송의 `(enrollment, step)` 및 source draft, booking 등의 도메인 중복 방지는 유지한다. 단건 초안의 중복 freeze는 source draft 부분 UNIQUE와 트랜잭션 검사로 막는다. 민감 토큰이 담긴 응답을 멱등성 저장소에 보관해야 하는 경우 `response_body`는 암호화된 envelope로 저장하고 로그에 남기지 않는다. 원문 secret을 일반 JSON으로 저장하는 예외를 만들지 않는다.

### B.1.4 낙관적 잠금과 부분 변경

가변 엔티티 GET은 `ETag: "3"`을 반환한다. PATCH·grant 변경·승인 결정·예약 변경은 `If-Match: "3"`가 필요하다. 헤더가 없으면 428, 현재 버전이 다르면 412다. 주소·template version·outbound 스냅샷에는 범용 PATCH를 제공하지 않는다.

단조 증가하는 읽음 상태 PUT, 반복 수신 거부 POST, Idempotency-Key로 보호되는 신규 리소스 생성은 별도 version precondition 없이 처리할 수 있다. 단, 클라이언트가 임의로 과거 읽음 시각을 써서 다른 변경을 덮어쓰지 못하게 한다.

### B.1.5 페이지네이션

cursor에는 정렬 필드·ID·필터 hash·권한 범위를 바인딩하고 서명한다. 첫 페이지 이후 필터나 sort를 바꾸면 cursor를 재사용하지 않는다. 기본 정렬은 `(created_at DESC, id DESC)`이며 Inbox는 `(updated_at DESC, id DESC)`를 사용한다. 실시간 목록은 페이지 사이 변경이 있을 수 있으므로 완전한 고정 스냅샷이 필요한 export·일괄 작업은 별도 job을 만든다.

---

## B.2 전체 내부 엔드포인트 목록

`R`은 읽기, `W`는 변경, `E`는 외부 효과 요청을 뜻한다. 표의 권한은 최소 범주이며 실제로는 본문 05장의 역할·scope·리소스 ACL·업무 정책의 교집합을 적용한다.

### 조직·연동

| Method | Path | 필수 권한 | 요청 핵심 / 결과 |
|---|---|---|---|
| GET | `/me` | 인증 | principal, role, scopes, 접근 가능한 메일함·행동 |
| GET | `/principals` | 조직 조회 | 직원·Agent의 상태·역할 목록 |
| POST | `/member-invitations` | Admin | email, role → 201 초대; 알림은 허용된 채널만 |
| PATCH | `/principals/{id}` | Admin | role, status + If-Match → 갱신 |
| POST | `/principals/{id}/disable-requests` | Admin | 재배정·대기 작업 정책 → 202 job |
| POST | `/agents` | Admin | 이름·허용 scopes → 201 agent principal |
| POST | `/agents/{id}/credentials` | Admin | scopes, expiry → 201 일회성 secret |
| POST | `/credentials/{id}/revoke` | Admin | 대기 작업 중단 → 200/202 |
| GET | `/mail-accounts` | 인증·grant | 접근 가능한 메일함 메타데이터 |
| POST | `/mail-accounts/connection-requests` | Admin | Zoho 지역·허용 redirect → OAuth 시작 정보 |
| GET | `/integrations/zoho/callback` | OAuth state | 교환·계정 확인; 브라우저 redirect |
| POST | `/mail-accounts/{id}/reconnect` | Admin | 재인증 시작 |
| GET | `/mail-accounts/{id}/grants` | Admin | 메일함 grant 목록 |
| PUT | `/mail-accounts/{id}/grants/{principal_id}` | Admin | grant booleans + If-Match |
| GET | `/delivery-channels` | 메일함 접근 | 목적·허용 상태·capability |
| POST | `/delivery-channels` | Admin | provider connection·mailbox·sender·purpose |
| POST | `/delivery-channels/{id}/reviews` | Admin·정책 검토 | 증빙·결론 → append-only review |
| PATCH | `/delivery-channels/{id}` | Admin | 검증된 범위의 enabled·내부 한도 + If-Match |
| GET | `/calendar-connections` | 본인·운영 권한 | 연결 상태·host별 가시 범위 |
| POST | `/calendar-connections/connection-requests` | host 또는 Admin | 별도 Calendar OAuth 연결 |

### CRM·리서치·가져오기

| Method | Path | 필수 권한 | 요청 핵심 / 결과 |
|---|---|---|---|
| GET/POST | `/companies` | crm.read / crm.write | 필터 조회 / 이름·도메인·담당 → 201 |
| GET/PATCH | `/companies/{id}` | crm.read / 소유권·쓰기 | 상세 / allowlist 필드·If-Match |
| POST | `/companies/{id}/archive` | CRM 관리 | archive, 관련 작업 검토 |
| POST | `/companies/{id}/merge-requests` | Manager/Admin | source·target·필드 선택 → 202 |
| GET | `/companies/{id}/activities` | CRM+원본 ACL | 허용 활동만 cursor 조회 |
| GET/POST | `/contacts` | crm.read / crm.write | 회사·직함·담당·연락 관계 |
| GET/PATCH | `/contacts/{id}` | CRM·소유권 | 상세·동시성 있는 변경 |
| POST | `/contacts/{id}/emails` | CRM 쓰기 | address, label, primary → 주소 연결 |
| PUT | `/contacts/{id}/primary-email` | CRM 쓰기 | email_address_id → 주 주소 변경 |
| POST | `/notes` | 대상 쓰기·메일 ACL | 대상 1개·본문 → 201 |
| GET/POST | `/tasks` | 업무 scope | 담당자·기한·연관 대상 |
| PATCH | `/tasks/{id}` | 담당·Manager | 상태·기한·If-Match |
| POST | `/lead-proposals` | research.propose | 후보 배열·증빙 → 201 |
| GET | `/lead-proposals` | 연구 검토 | pending·중복 후보 목록 |
| POST | `/lead-proposals/{id}/decisions` | 인간 CRM 검토 | accept/reject·병합 선택·If-Match |
| POST | `/research-evidence` | research.write | 단일 대상·필드·출처·관측값 |
| POST | `/research-evidence/{id}/reviews` | 인간 검토 | verified/rejected·이유 |
| POST | `/imports/upload-requests` | crm.import | 이름·MIME·크기 → 비공개 업로드 정보 |
| POST | `/imports` | crm.import | object_id·mapping → 202 validate job |
| GET | `/imports/{id}` | 요청자·Manager | 검증·중복·오류·진행 |
| GET | `/imports/{id}/rows` | 요청자·Manager | 행별 오류·결과 |
| POST | `/imports/{id}/commit` | crm.import | validation_hash·중복 정책 → 202 |
| POST | `/exports` | crm.export + 원본 ACL | 필터·필드 → 202 export job |

### 템플릿·캠페인·메일·승인

| Method | Path | 필수 권한 | 요청 핵심 / 결과 |
|---|---|---|---|
| GET/POST | `/email-templates` | templates.read/write | 목록 / 논리 이름 생성 |
| GET | `/email-templates/{id}/versions` | template 읽기 | 불변 버전 목록 |
| POST | `/email-templates/{id}/versions` | template 쓰기 | 제목·본문·변수 Schema → 201 |
| POST | `/template-versions/{id}/render` | compose | sample variables → preview, 경고 |
| GET/POST | `/campaigns` | compose grant | 목록 / campaign·초기 revision |
| GET | `/campaigns/{id}` | 메일함 접근 | 상태·revision·launch·성과 |
| POST | `/campaigns/{id}/revisions` | compose | 변경된 구성·단계 → 새 revision |
| POST | `/campaigns/{id}/preflight` | compose | revision·대상 목록 → 비구속 검증 결과 |
| POST | `/campaigns/{id}/launches` | compose | revision·대상·개인화 → 202 building/freeze job |
| GET | `/campaign-launches/{id}` | 메일함 접근 | frozen manifest·실제 발송 내용 |
| POST | `/campaign-launches/{id}/approval-requests` | compose | expected hash·사유 → 201 승인 요청 |
| POST | `/campaign-launches/{id}/pause` | 담당·pause scope | 이유·If-Match → 200/202 |
| POST | `/campaign-launches/{id}/cancel` | 담당·cancel scope | 미실행 취소, in-flight 별도 표시 |
| GET | `/approvals` | 승인자·본인 요청 | pending·종류별 목록 |
| GET | `/approvals/{id}` | 대상 접근 | 동결 스냅샷·policy·권한 |
| POST | `/approvals/{id}/decisions` | 인간 approver | decision·hash·이유·If-Match |
| POST | `/approvals/{id}/revoke` | 승인자·Admin | 아직 미실행인 허가 철회 |
| GET/POST | `/message-drafts` | compose grant | private/공유 초안 목록·생성 |
| GET/PATCH | `/message-drafts/{id}` | 초안 소유·grant | 상세·본문 수정·If-Match |
| POST | `/message-drafts/{id}/freeze` | compose | 채널·sender·expected version → outbound snapshot |
| POST | `/outbound-messages/{id}/preflight` | 대상 grant | 현재 실행 가능성·필요 승인 |
| POST | `/outbound-messages/{id}/approval-requests` | compose | Agent 또는 승인 필요 단건의 요청 |
| POST | `/outbound-messages/{id}/submit` | 인간 mail.send·정책 | 정상 업무 회신 또는 유효 승인 → 202 |
| GET | `/outbound-messages/{id}` | 메일함 read | 상태·시도·unknown 설명 |
| POST | `/outbound-messages/{id}/cancel` | 담당·정책 | 미실행 취소·in-flight 경계 |
| GET | `/email-threads` | mail.read | 허용 메일함·배정·상태·검색 |
| GET | `/email-threads/{id}` | mail.read | 메시지·CRM·thread version |
| PUT | `/email-threads/{id}/assignment` | mail.assign | 담당 principal·If-Match |
| PUT | `/email-threads/{id}/read-state` | mail.read | 본인의 읽음 위치만 |
| PATCH | `/email-threads/{id}` | mail.read·처리 권한 | status·If-Match |
| POST | `/email-threads/{id}/reply-drafts` | compose | 기대 last_inbound·본문/템플릿 → 초안 |
| POST | `/email-messages/{id}/classification-reviews` | 인간 mail.read | 분류·이유 → 검토·안전 중단 |
| POST | `/message-attachments/{id}/download-requests` | mail.read | 검증 후 짧은 다운로드 URL |

### 예약·준수·운영

| Method | Path | 필수 권한 | 요청 핵심 / 결과 |
|---|---|---|---|
| GET/POST | `/meeting-types` | 본인 host·Manager | 미팅 유형·시간·버퍼 |
| PATCH | `/meeting-types/{id}` | host·Manager | 가용 설정·If-Match; host 교체는 신규 생성 |
| GET/PUT | `/hosts/{id}/availability` | 본인·Manager | 규칙·예외; 타인의 상세 일정 비공개 |
| POST | `/booking-links` | host/담당·정책 | 유형·대상·만료 → 링크 |
| GET | `/bookings` | 본인·일정 관리 | 기간·상태·host 목록 |
| GET | `/bookings/{id}` | 일정 접근 | 예약·외부 sync·CRM |
| POST | `/booking-change-proposals` | Agent/직원 제한 scope | 생성·변경·취소 제안; 외부 영향 없음 |
| POST | `/bookings/{id}/reschedule` | host·정책 | 새 슬롯·If-Match → 202 |
| POST | `/bookings/{id}/cancel` | host·정책 | 취소 요청·If-Match → 202 |
| GET/POST | `/communication-bases` | 준수 조회/검토 | 근거·목적·관할·증빙; 신규는 pending |
| POST | `/communication-bases/{id}/reviews` | 인간 검토 | valid/rejected 판단, 유효 기간 |
| POST | `/communication-bases/{id}/revoke` | 준수 관리 | 철회·관련 발송 중단 |
| GET/POST | `/suppressions` | 준수 scope | 제한된 검색 / 주소·목적·이유 → 차단 |
| POST | `/suppressions/{id}/release-requests` | 인간 관리 | 증빙·이유 → 승인 요청; 직접 해제 아님 |
| GET/POST | `/agent-runs` | agent 관리·run 요청 | 목적·budget·대상 범위 |
| GET | `/agent-runs/{id}` | 본인 run·관리 | action·진행·오류·승인 |
| POST | `/agent-runs/{id}/cancel` | 요청자·관리 | 새 도구 실행 중단, 외부 in-flight 별도 |
| GET | `/jobs/{id}` | 본인·리소스 접근 | 진행·에러·재시도 가능 여부 |
| POST | `/jobs/{id}/retry-requests` | 운영 | 안전한 작업만, unknown은 대조 필요 |
| GET | `/audit-logs` | 제한된 감사 권한 | redacted 로그·cursor |
| GET | `/dashboard/summary` | 범위 내 읽기 | KPI 정의·cohort·freshness 포함 |
| PUT | `/operations/emergency-stop` | Admin | enabled·이유·If-Match → 즉시 신규 제출 중지 |

---

## B.3 주요 REST 요청·응답 예시

### B.3.1 현재 사용자와 실제 가능한 행동

`GET /api/v1/me` → `200 OK`

```json
{
  "data": {
    "principal_id": "10000000-0000-4000-8000-000000000001",
    "kind": "human",
    "role": "manager",
    "workspace_id": "10000000-0000-4000-8000-000000000000",
    "display_name": "영업 담당자 B",
    "permission_version": 7,
    "mail_accounts": [
      {
        "id": "20000000-0000-4000-8000-000000000001",
        "address": "sales@example.com",
        "allowed_actions": ["mail.read", "mail.compose", "mail.send", "mail.approve", "mail.assign"]
      }
    ]
  },
  "meta": {"request_id": "req_me_demo"}
}
```

`allowed_actions`가 있어도 실행 시 정책과 grant를 다시 확인한다. Admin이 볼 수 있는 계정 관리 목록과 실제 본문 접근 가능한 목록을 구분한다.

### B.3.2 공용 메일함 grant 변경

```http
PUT /api/v1/mail-accounts/20000000-0000-4000-8000-000000000001/grants/10000000-0000-4000-8000-000000000002
If-Match: "2"
Content-Type: application/json
```

```json
{
  "can_read": true,
  "can_compose": true,
  "can_send": false,
  "can_approve": false,
  "can_assign": true
}
```

`200 OK`, `ETag: "3"`

```json
{
  "data": {
    "mail_account_id": "20000000-0000-4000-8000-000000000001",
    "principal_id": "10000000-0000-4000-8000-000000000002",
    "can_read": true,
    "can_compose": true,
    "can_send": false,
    "can_approve": false,
    "can_assign": true,
    "version": 3,
    "permission_version": 8,
    "pending_actions_revalidation_requested": true
  },
  "meta": {"request_id": "req_grant_demo"}
}
```

첫 grant 생성은 `If-None-Match: *`로 명시한다. 다른 작업자가 먼저 생성하면 412다. Agent에게 `can_send=true`는 승인 제출의 mailbox ACL을 충족할 때만 허용하고, `can_approve=true`는 거절한다. Agent의 직접 REST 제출과 `mail.send`는 계속 금지한다.

### B.3.3 기업 생성·중복 조회

`POST /api/v1/companies`, `Idempotency-Key` 사용, `201 Created`.

```json
{
  "name": "예시 제조기업",
  "domain": "example.com",
  "website_url": "https://example.com",
  "industry": "manufacturing",
  "country_code": "KR",
  "owner_principal_id": "10000000-0000-4000-8000-000000000002"
}
```

```json
{
  "data": {
    "id": "30000000-0000-4000-8000-000000000001",
    "name": "예시 제조기업",
    "domain": "example.com",
    "lifecycle_stage": "prospect",
    "version": 1,
    "duplicate_candidates": []
  },
  "meta": {"request_id": "req_company_demo"}
}
```

같은 도메인을 가진 회사가 있으면 저장 전 경고·후보 비교를 제공하되 도메인만으로 강제 병합하지 않는다. Member가 다른 직원을 owner로 지정하려면 재배정 권한이 필요하다. Agent의 직접 회사 생성은 기본 거절하고 `/lead-proposals`를 사용한다.

### B.3.4 주소 연결과 발송 가능성

`POST /api/v1/contacts/{id}/emails`:

```json
{
  "address": "contact@example.com",
  "label": "업무용",
  "is_primary": true
}
```

```json
{
  "data": {
    "email_address_id": "40000000-0000-4000-8000-000000000001",
    "address": "contact@example.com",
    "verification_status": "unknown",
    "marketing_eligibility": {
      "eligible": false,
      "reason_codes": ["SEND_BASIS_UNVERIFIED"]
    }
  },
  "meta": {"request_id": "req_address_demo"}
}
```

형식이 맞는 주소 생성만으로 `verified`나 `consented`로 저장하지 않는다. 이미 있는 주소는 동일 email ID에 연결하되 다른 사람과의 연결 충돌을 사용자에게 설명한다.

### B.3.5 템플릿 새 버전

`POST /api/v1/email-templates/{id}/versions`:

```json
{
  "subject": "{{company.name}} 미팅 관련 안내",
  "body_text": "안녕하세요 {{contact.name}}님. 요청하신 미팅 일정은 {{meeting.booking_url}}에서 확인할 수 있습니다.",
  "body_html": null,
  "variable_schema": {
    "type": "object",
    "properties": {
      "company.name": {"type": "string", "maxLength": 300},
      "contact.name": {"type": "string", "maxLength": 300},
      "meeting.booking_url": {"type": "string", "format": "uri"}
    },
    "required": ["company.name", "contact.name", "meeting.booking_url"],
    "additionalProperties": false
  }
}
```

응답은 `201`과 `template_version_id`, `version_no`, `content_hash`를 제공한다. 예제의 메시지 목적은 내용과 실제 요청 이력으로 검토하며 이름만 보고 자동으로 transactional 또는 correspondence로 분류하지 않는다. 시스템 변수 URL은 렌더링 시 서버가 주입한다.

### B.3.6 캠페인 생성

`POST /api/v1/campaigns`:

```json
{
  "name": "검토된 기업 소개 캠페인",
  "objective": "사전 확인된 대상의 상담 미팅 연결",
  "mail_account_id": "20000000-0000-4000-8000-000000000001",
  "delivery_channel_id": "20000000-0000-4000-8000-000000000010",
  "timezone": "Asia/Seoul",
  "schedule_rules": {
    "weekdays": [1, 2, 3, 4, 5],
    "window_start": "09:00",
    "window_end": "17:00",
    "internal_daily_cap": 20,
    "minimum_gap_seconds": 120,
    "launch_not_before": "2026-10-12T01:00:00Z",
    "launch_expires_at": "2026-11-12T08:00:00Z"
  },
  "steps": [
    {
      "step_order": 1,
      "template_version_id": "50000000-0000-4000-8000-000000000001",
      "delay_hours": 0
    },
    {
      "step_order": 2,
      "template_version_id": "50000000-0000-4000-8000-000000000002",
      "delay_hours": 72
    }
  ],
  "stop_on_reply": true,
  "stop_on_booking": true
}
```

```json
{
  "data": {
    "campaign_id": "60000000-0000-4000-8000-000000000001",
    "revision_id": "60000000-0000-4000-8000-000000000002",
    "status": "draft",
    "revision_no": 1,
    "version": 1,
    "channel_ready": false,
    "blocking_reasons": ["PROVIDER_POLICY_UNVERIFIED"]
  },
  "meta": {"request_id": "req_campaign_demo"}
}
```

20건·120초는 앱 내부 정책의 예시일 뿐 공급자 허용량이 아니다. 제한값을 낮췄다고 발송 목적의 금지 상태가 해제되지 않는다.

### B.3.7 대상 preflight

`POST /api/v1/campaigns/{id}/preflight`:

```json
{
  "revision_id": "60000000-0000-4000-8000-000000000002",
  "email_address_ids": [
    "40000000-0000-4000-8000-000000000001",
    "40000000-0000-4000-8000-000000000002"
  ]
}
```

```json
{
  "data": {
    "checked_at": "2026-10-09T04:00:00Z",
    "is_execution_authorization": false,
    "global_blocking_reasons": ["PROVIDER_POLICY_UNVERIFIED"],
    "recipients": [
      {
        "email_address_id": "40000000-0000-4000-8000-000000000001",
        "recipient_checks_passed": true,
        "reason_codes": []
      },
      {
        "email_address_id": "40000000-0000-4000-8000-000000000002",
        "recipient_checks_passed": false,
        "reason_codes": ["RECIPIENT_SUPPRESSED"]
      }
    ]
  },
  "meta": {"request_id": "req_preflight_demo"}
}
```

수신자 검사가 통과해도 global block이 있으면 발송 불가다. preflight는 최종 실행 허가가 아니며, 동결·승인·실행 시 다시 평가한다.

### B.3.8 launch 동결·승인 요청

`POST /api/v1/campaigns/{id}/launches`는 revision ID, 명시적 주소 목록, 허용된 개인화 값을 받고 `202 building`과 job ID를 반환한다. 작업이 끝나면 GET launch에 `frozen`, manifest hash, 고정 대상 수, 단계 수, 각 메시지 snapshot을 반환한다. 대량 콘텐츠는 cursor로 조회한다.

`POST /api/v1/campaign-launches/{id}/approval-requests`:

```json
{
  "expected_manifest_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "reason": "수신 대상과 실제 발송 내용을 검토했습니다."
}
```

```json
{
  "data": {
    "approval_request_id": "70000000-0000-4000-8000-000000000001",
    "status": "pending",
    "expires_at": "2026-10-16T04:00:00Z",
    "version": 1,
    "launch_id": "60000000-0000-4000-8000-000000000003"
  },
  "meta": {"request_id": "req_approval_request_demo"}
}
```

공급자 정책 미확인 등 global block이 남아 있으면 운영 발송을 위한 승인 요청은 422로 거절한다. 검토용 캠페인·스냅샷은 저장할 수 있지만 “발송 준비 완료”라고 표시하지 않는다. 위 성공 응답은 해당 검사가 통과한 테스트 예시다.

### B.3.9 승인 결정

`POST /api/v1/approvals/{id}/decisions`, `If-Match: "1"`, `Idempotency-Key`:

```json
{
  "decision": "approved",
  "expected_snapshot_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "reason": "대상·내용·채널·발송 근거를 확인했습니다."
}
```

```json
{
  "data": {
    "approval_request_id": "70000000-0000-4000-8000-000000000001",
    "approval_status": "approved",
    "decision_id": "70000000-0000-4000-8000-000000000002",
    "execution_status": "queued",
    "job_id": "90000000-0000-4000-8000-000000000001",
    "provider_accepted_count": 0,
    "version": 2
  },
  "meta": {"request_id": "req_decision_demo"}
}
```

응답은 `200 OK`로 승인 기록 완료를 나타낸다. `execution_status`는 후속 실행이 비동기임을 나타낸다. 이 응답을 메일 발송 완료로 해석하지 않는다. `SELF_APPROVAL_FORBIDDEN`, `APPROVAL_EXPIRED`, `SNAPSHOT_MISMATCH`, `MAILBOX_APPROVE_DENIED`를 구분한다.

### B.3.10 회신 초안과 단건 제출

`POST /api/v1/email-threads/{id}/reply-drafts`:

```json
{
  "expected_last_inbound_id": "80000000-0000-4000-8000-000000000001",
  "body_text": "문의 감사합니다. 요청하신 내용은 내부에서 확인한 후 안내드리겠습니다.",
  "body_html": null,
  "visibility": "private"
}
```

서버는 원본 메시지·수신자를 확인하고 reply draft를 생성한다. 외부 메일의 임의 `Reply-To`를 무조건 따라가지 않으며 수신 주소를 사용자에게 보여준다. Agent가 생성해도 이 단계는 발송하지 않는다.

freeze 이후 생성된 outbound에 대해 `POST /api/v1/outbound-messages/{id}/submit`:

```json
{
  "expected_payload_hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "not_before": "2026-10-12T01:00:00Z"
}
```

```json
{
  "data": {
    "outbound_message_id": "80000000-0000-4000-8000-000000000010",
    "status": "queued",
    "job_id": "90000000-0000-4000-8000-000000000002",
    "provider_accepted": false
  },
  "meta": {"request_id": "req_submit_demo"}
}
```

응답은 `202 Accepted`. 인간 mail.send 권한·업무 회신 근거·공급자 허용·차단·reply claim을 충족해야 한다. Agent는 이 endpoint를 호출하면 403이며, 메시지 승인 요청 경로를 사용한다. 마케팅 메시지는 인간 단건 호출이어도 마케팅 정책과 승인 규칙을 우회할 수 없다.

### B.3.11 제출 결과 unknown

`GET /api/v1/outbound-messages/{id}`:

```json
{
  "data": {
    "id": "80000000-0000-4000-8000-000000000010",
    "status": "unknown",
    "failure_code": "PROVIDER_SUBMISSION_OUTCOME_UNKNOWN",
    "provider_accepted": null,
    "delivery_status": "unknown",
    "automatic_resubmit_allowed": false,
    "next_action": "reconcile_with_provider",
    "last_attempt": {
      "attempt_no": 1,
      "started_at": "2026-10-12T01:00:02Z",
      "finished_at": "2026-10-12T01:00:32Z"
    }
  },
  "meta": {"request_id": "req_unknown_demo"}
}
```

`provider_accepted=null`은 알 수 없음을 의미한다. false로 내려서 에이전트가 미발송으로 추정하게 하지 않는다. accepted인 경우에도 `delivery_status`는 공급자 근거가 없으면 unknown이다.

### B.3.12 리드 후보 배치

`POST /api/v1/lead-proposals`:

```json
{
  "agent_run_id": "a0000000-0000-4000-8000-000000000001",
  "proposals": [
    {
      "external_key": "research-example-001",
      "company_name": "예시 제조기업",
      "domain": "example.com",
      "proposed_data": {"industry": "manufacturing"},
      "evidence": [
        {
          "field_path": "industry",
          "source_url": "https://example.com/about",
          "observed_at": "2026-10-09T04:00:00Z",
          "value": "manufacturing",
          "evidence_type": "observed",
          "excerpt": "제조 분야를 소개하는 회사 설명을 확인했다는 합성 예시입니다."
        }
      ]
    }
  ]
}
```

응답은 각 항목의 proposal ID, `pending`, 기존 회사 중복 후보 ID 목록, 경고를 반환한다. evidence는 같은 트랜잭션에서 저장한다. 일부 항목만 유효한 배치는 기본적으로 전체 요청을 422로 거절하며, 대량 부분 성공은 명시된 import job을 사용한다. API와 CSV의 부분 성공 정책을 섞지 않는다.

### B.3.13 CSV 검증·commit

업로드 후 `POST /api/v1/imports`에 `stored_object_id`, `mapping`, `duplicate_strategy=review`를 전달한다. validation job은 파싱·크기·헤더·정규화·기존 회사·주소 중복을 검사한다.

```json
{
  "data": {
    "import_id": "b0000000-0000-4000-8000-000000000001",
    "status": "ready",
    "validation_hash": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    "summary": {
      "total_rows": 100,
      "valid_rows": 88,
      "duplicate_rows": 7,
      "invalid_rows": 5
    }
  },
  "meta": {"request_id": "req_import_demo"}
}
```

commit 요청에는 위 validation hash와 `invalid_row_policy=skip` 또는 `reject`를 명시한다. `skip`을 선택한 경우 결과에 실제 성공·건너뜀·오류 수를 표시한다. CRM에 새로 등록된 주소에 마케팅 동의를 일괄 부여하지 않는다.

---

## B.4 공개 API

### B.4.1 endpoint 목록

| Method | 경로 | 동작 |
|---|---|---|
| GET | `/public/v1/booking-links/{token}` | 공개 미팅 설명·호스트 이름·제한된 규칙 |
| GET | `/public/v1/booking-links/{token}/slots` | `from`, `to`, `timezone`에 따른 슬롯 |
| POST | `/public/v1/booking-links/{token}/bookings` | 이름·이메일·시작시각 → 202 예약 처리 |
| GET | `/public/v1/bookings/{manage_token}` | 본인 예약 진행·관리 화면 데이터 |
| POST | `/public/v1/bookings/{manage_token}/reschedule` | 새 슬롯·expected version → 202 |
| POST | `/public/v1/bookings/{manage_token}/cancel` | 취소 요청·expected version → 202 |
| GET | `/public/v1/unsubscribe/{token}` | 상태를 바꾸지 않는 확인 데이터 |
| POST | `/public/v1/unsubscribe/{token}` | 마케팅 차단 → 200 |
| POST | `/public/v1/unsubscribe/{token}/one-click` | 지원 채널의 RFC 8058 요청 처리 |

공개 경로는 API bearer·직원 쿠키가 아니라 목적이 제한된 token을 검증한다. 토큰 검증 전 내부 고객·발신 계정·현재 차단 여부를 외부에 노출하지 않는다. rate limit은 endpoint별로 다르게 적용하고, 예약 CAPTCHA 정책을 수신 거부에 그대로 적용하지 않는다.

### B.4.2 슬롯 조회

`GET /public/v1/booking-links/{token}/slots?from=2026-10-12&to=2026-10-16&timezone=Asia%2FSeoul`

```json
{
  "data": {
    "timezone": "Asia/Seoul",
    "availability_checked_at": "2026-10-09T04:00:00Z",
    "slots": [
      {
        "starts_at": "2026-10-15T05:00:00Z",
        "ends_at": "2026-10-15T05:30:00Z"
      }
    ],
    "reservation_guaranteed": false
  },
  "meta": {"request_id": "req_slots_demo"}
}
```

슬롯은 조회 시점의 후보이며 최종 예약을 보장하지 않는다. 외부 busy 정보가 stale하고 갱신이 실패하면 해당 상태를 숨기고 빈 슬롯으로 처리하지 않고, `503 CALENDAR_AVAILABILITY_UNAVAILABLE`을 반환한다.

### B.4.3 예약 생성

`POST /public/v1/booking-links/{token}/bookings`, `Idempotency-Key`:

```json
{
  "guest_name": "김담당",
  "guest_email": "contact@example.com",
  "starts_at": "2026-10-15T05:00:00Z",
  "timezone": "Asia/Seoul",
  "privacy_notice_version": "booking-privacy-v1"
}
```

```json
{
  "data": {
    "status": "confirming",
    "starts_at": "2026-10-15T05:00:00Z",
    "ends_at": "2026-10-15T05:30:00Z",
    "timezone": "Asia/Seoul",
    "manage_url": "https://booking.example.invalid/public/booking/demo-management-token",
    "status_url": "/public/v1/bookings/demo-management-token",
    "message": "예약 요청을 접수했습니다. 캘린더 확정 결과를 확인해 주세요."
  },
  "meta": {"request_id": "req_booking_demo"}
}
```

`202 Accepted`. `demo-management-token`은 문서용 표기이며 실제 구현에서는 충분히 긴 임의 토큰을 사용한다. 응답의 공개 링크는 사용자의 현재 capability 범위에만 접근한다. guest 이메일의 소유 확인 상태를 따로 저장·평가하며, 잘못 입력한 이메일을 기존 연락처에 자동 병합하지 않는다.

충돌은 `409 SLOT_UNAVAILABLE`, 링크 만료는 `410 BOOKING_LINK_EXPIRED`, 이메일 바인딩 불일치는 `422 BOOKING_RECIPIENT_MISMATCH`, 오래된 변경 요청은 412다. 취소·변경 API도 외부 캘린더 성공 전에는 완료로 응답하지 않는다.

### B.4.4 수신 거부

`POST /public/v1/unsubscribe/{token}`의 본문은 빈 객체 또는 빈 body다. 유효 토큰의 반복 요청은 아래와 동일한 성공 응답이다.

```json
{
  "data": {
    "status": "unsubscribed",
    "scope": "workspace_marketing",
    "message": "해당 발신 조직의 마케팅 이메일 수신 거부가 처리되었습니다."
  },
  "meta": {"request_id": "req_unsubscribe_demo"}
}
```

알 수 없는 토큰은 일반화된 404 응답으로 처리하며 주소 존재 여부를 드러내지 않는다. 실제로 처리하지 않았는데 “처리 완료”라고 응답하지 않는다. 이미 처리된 유효 토큰은 200이다.

one-click 경로는 `application/x-www-form-urlencoded`와 `List-Unsubscribe=One-Click`를 처리한다. 정상 형식·토큰 검증 후 동일한 suppression Use Case를 호출한다. GET이나 HEAD에는 차단 부작용이 없다.[S10](#s10)

---

## B.5 안정적인 오류 코드

| 코드 | HTTP | 자동 재시도 | 클라이언트/에이전트 처리 |
|---|---|---|---|
| `AUTHENTICATION_REQUIRED` | 401 | 재인증 후 | 로그인·credential 확인 |
| `PRINCIPAL_DISABLED` | 403 | 아니오 | 계정 관리자 안내 |
| `MAILBOX_ACCESS_DENIED` | 403/404 | 아니오 | 해당 객체 표시 중단 |
| `AGENT_DIRECT_SEND_FORBIDDEN` | 403 | 아니오 | 승인 요청 도구 사용 |
| `SELF_APPROVAL_FORBIDDEN` | 403 | 아니오 | 다른 승인자 필요 |
| `VERSION_PRECONDITION_REQUIRED` | 428 | 조회 후 | ETag 포함 |
| `VERSION_MISMATCH` | 412 | 검토 후 | 최신 값과 변경 비교 |
| `IDEMPOTENCY_KEY_REUSED` | 409 | 아니오 | 기존 명령 확인, 의도 다른 작업만 새 키 |
| `REQUEST_IN_PROGRESS` | 409 | 상태 조회 | 중복 새 작업 생성 금지 |
| `SNAPSHOT_MISMATCH` | 409 | 아니오 | 새 스냅샷 검토·승인 |
| `APPROVAL_EXPIRED` | 409 | 아니오 | 새 승인 요청 |
| `RECIPIENT_SUPPRESSED` | 422 | 아니오 | 대상 제외·우회 금지 |
| `SEND_BASIS_UNVERIFIED` | 422 | 아니오 | 적법한 근거 검토 |
| `PROVIDER_POLICY_BLOCKED` | 422 | 아니오 | 해당 목적 발송 중지 |
| `PROVIDER_POLICY_UNVERIFIED` | 422 | 아니오 | 계약·공급자 검증 |
| `TEMPLATE_VARIABLE_MISSING` | 422 | 입력 수정 후 | 필수 변수 보완 |
| `REPLY_CONTEXT_CHANGED` | 409 | 검토 후 | 최신 메시지 확인 |
| `REPLY_ALREADY_SUBMITTED` | 409 | 아니오 | 기존 회신 확인 |
| `RECIPIENT_ACTIVE_ELSEWHERE` | 409 | 아니오 | 기존 캠페인 확인 |
| `SLOT_UNAVAILABLE` | 409 | 다른 슬롯 | 최신 가용성 조회 |
| `BOOKING_LINK_EXPIRED` | 410 | 아니오 | 새 링크 안내 |
| `BOOKING_RECIPIENT_MISMATCH` | 422 | 입력 확인 | 다른 주소로 임의 확정 금지 |
| `RATE_LIMITED` | 429 | 지정 시각 이후 | Retry-After 준수 |
| `INTEGRATION_REAUTH_REQUIRED` | 503 | 재연결 후 | 연결 관리자 알림 |
| `CALENDAR_AVAILABILITY_UNAVAILABLE` | 503 | 안전한 조회만 | 확정 요청 남발 금지 |
| `PROVIDER_SUBMISSION_OUTCOME_UNKNOWN` | 상태 리소스 | 아니오 | 대조 job·운영 검토 |
| `WORKSPACE_EMERGENCY_STOP` | 409 | 아니오 | 명시적 재개 전 제출 중단 |

동기 endpoint가 외부 제출 자체를 수행하지 않으므로 unknown은 주로 `GET outbound`의 업무 상태로 나타난다. 500 응답이 왔다는 이유만으로 새 멱등성 키로 외부 작업을 다시 만들지 않는다.

---

## B.6 MCP 도구 계약

### B.6.1 공통 컨텍스트

`workspace_id`, `principal_id`, `scopes`, `credential_id`, `request_id`는 MCP 인증 미들웨어가 주입한다. 도구 인자에 임의 principal을 받지 않는다. `agent_run_id`를 받는 경우 해당 Agent 또는 위임된 범위의 run인지 확인한다.

변경 도구는 `idempotency_key`를 명시적으로 받는다. HTTP Idempotency-Key에 의존하지 않는 MCP transport에서도 동일 Use Case에 키가 전달돼야 한다. 출력은 `structuredContent`와 그에 대응하는 짧은 text content를 함께 제공한다.[S13](#s13)

도구 결과의 공통 업무 envelope는 `ok`, `data`, `error`, `request_id`다. 성공은 data 객체와 error=null, 실패는 data=null과 안정적인 오류 객체를 반환한다. 프로토콜 오류와 업무 오류를 구분한다. 업무 거절은 `isError=true`로 표시하고 에이전트가 우회 실행하지 않게 처리 방향을 제공한다.

### B.6.2 도구별 필수 입력·출력

| Tool | 입력 | 출력 | 자동 실행 범위 |
|---|---|---|---|
| `crm_search_companies` | query, filters, cursor, limit | company summaries, next_cursor | 읽기 |
| `crm_get_company` | company_id, requested_sections | 허용 필드·출처·요약 | 읽기; 메일 ACL 별도 |
| `crm_get_contact` | contact_id | 주소별 상태·근거 요약 | 읽기 |
| `mail_get_thread` | thread_id, cursor, limit | 정제된 본문·메타데이터 | grant 범위 읽기 |
| `research_propose_leads` | run_id, idempotency_key, proposals | pending IDs·중복 후보 | 후보 영역 쓰기 |
| `research_add_evidence` | 대상·필드·출처·관측시각·값 | evidence ID·pending | 검증 상태 변경 불가 |
| `research_get_run` | run_id | 진행·검토·오류 | 자신의 run |
| `crm_propose_update` | target, expected_version, field_changes, evidence | proposal ID·diff | 정식 CRM 자동 덮어쓰기 아님 |
| `crm_create_note` | target, text, idempotency_key | note ID | 허용 대상 내부 메모 |
| `mail_create_draft` | mailbox, recipient, purpose, content/template, reply context | draft ID·version·필요 승인 | 발송 없음 |
| `templates_create_draft_version` | template_id, subject, body, variable_schema | 새 버전 ID | 기존 버전 수정 불가 |
| `campaign_create_draft` | mailbox, channel, objective, steps, rules | campaign·revision | 실행 없음 |
| `campaign_preflight` | revision_id, recipients | 검사 결과·block codes | 허가증 아님 |
| `campaign_request_launch` | frozen launch ID, expected hash, reason, key | approval ID·pending | 승인 요청만 |
| `campaign_pause` | launch_id, reason, expected_version | 미실행 중단·in-flight 수 | 별도 pause scope 필요 |
| `booking_list_slots` | meeting_type 또는 허용 link ID, time range, timezone | slots·freshness | 일정 내용 제외 |
| `booking_propose` | meeting_type, guest, desired time, purpose, key | proposal·approval request | 외부 이벤트 생성 없음 |
| `booking_get` | booking_id | 허용 예약 상태·sync | 접근 범위만 |
| `operations_get_job` | job_id | state·retryability·result refs | 관련 리소스 ACL |
| `approvals_get_request` | approval_request_id | 상태·요청자·다음 행동 | 요청자·허용 범위 |
| `agents_cancel_run` | run_id, reason | 취소 상태·in-flight 별도 | 자신의 run·관리 위임 |

`crm_propose_update`는 새 테이블을 반드시 요구하지 않는다. v1.0은 `lead_proposals.proposed_data`에 기존 target·expected version·field diff를 담고, 증빙을 연결하는 동일 제안 검토 모듈로 구현한다. 수락 시 필드 allowlist·현재 version·소유권을 재검증한다.

### B.6.3 실제 도구 정의 예시

```json
{
  "name": "research_propose_leads",
  "title": "출처가 있는 잠재 고객 후보 등록",
  "description": "조사한 회사를 검토 대기 후보로 등록합니다. 정식 CRM 등록, 수신 동의 부여, 메일 발송은 수행하지 않습니다. URL과 관측 시각이 있는 사실만 observed로 입력하고 추론은 inferred로 구분하세요.",
  "inputSchema": {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "type": "object",
    "additionalProperties": false,
    "required": ["agent_run_id", "idempotency_key", "proposals"],
    "properties": {
      "agent_run_id": {"type": "string", "format": "uuid"},
      "idempotency_key": {"type": "string", "minLength": 16, "maxLength": 128},
      "proposals": {
        "type": "array",
        "minItems": 1,
        "maxItems": 100,
        "items": {
          "type": "object",
          "additionalProperties": false,
          "required": ["external_key", "company_name", "domain", "evidence"],
          "properties": {
            "external_key": {"type": "string", "minLength": 1, "maxLength": 200},
            "company_name": {"type": "string", "minLength": 1, "maxLength": 300},
            "domain": {"type": "string", "minLength": 1, "maxLength": 253},
            "evidence": {
              "type": "array",
              "minItems": 1,
              "maxItems": 20,
              "items": {
                "type": "object",
                "additionalProperties": false,
                "required": ["field_path", "source_url", "observed_at", "value", "evidence_type"],
                "properties": {
                  "field_path": {"type": "string", "maxLength": 100},
                  "source_url": {"type": "string", "format": "uri", "maxLength": 2048},
                  "observed_at": {"type": "string", "format": "date-time"},
                  "value": {"type": "string", "maxLength": 2000},
                  "evidence_type": {"enum": ["observed", "inferred"]},
                  "excerpt": {"type": "string", "maxLength": 2000}
                }
              }
            }
          }
        }
      }
    }
  },
  "outputSchema": {
    "type": "object",
    "additionalProperties": false,
    "required": ["ok", "data", "error", "request_id"],
    "properties": {
      "ok": {"type": "boolean"},
      "data": {
        "type": ["object", "null"],
        "properties": {
          "proposal_ids": {"type": "array", "items": {"type": "string", "format": "uuid"}},
          "status": {"enum": ["pending"]}
        },
        "required": ["proposal_ids", "status"],
        "additionalProperties": false
      },
      "error": {
        "type": ["object", "null"],
        "properties": {
          "code": {"type": "string"},
          "message": {"type": "string"},
          "retryable": {"type": "boolean"}
        },
        "required": ["code", "message", "retryable"],
        "additionalProperties": false
      },
      "request_id": {"type": "string"}
    },
    "oneOf": [
      {"properties": {"ok": {"const": true}, "data": {"type": "object"}, "error": {"type": "null"}}},
      {"properties": {"ok": {"const": false}, "data": {"type": "null"}, "error": {"type": "object"}}}
    ]
  },
  "annotations": {
    "readOnlyHint": false,
    "destructiveHint": false,
    "idempotentHint": true,
    "openWorldHint": false
  }
}
```

이 도구는 출처 URL을 저장할 뿐 자체 fetch하지 않는다. 그래서 위 예시의 `openWorldHint=false`다. URL 검증 작업을 추가해 외부 접근을 수행하게 되면 annotation·권한·SSRF 방어·도구 설명도 함께 변경한다. 도구 hint는 클라이언트를 위한 안내이며 보안 통제의 대체물이 아니다.[S13](#s13)

### B.6.4 성공·실패 결과

```json
{
  "jsonrpc": "2.0",
  "id": 12,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\"ok\":true,\"data\":{\"proposal_ids\":[\"a0000000-0000-4000-8000-000000000010\"],\"status\":\"pending\"},\"error\":null,\"request_id\":\"req_mcp_demo\"}"
      }
    ],
    "structuredContent": {
      "ok": true,
      "data": {
        "proposal_ids": ["a0000000-0000-4000-8000-000000000010"],
        "status": "pending"
      },
      "error": null,
      "request_id": "req_mcp_demo"
    },
    "isError": false
  }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": 13,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "후보 제출 권한이 없습니다. 관리자에게 허용 scope를 확인해야 하며 다른 도구로 우회해서는 안 됩니다."
      }
    ],
    "structuredContent": {
      "ok": false,
      "data": null,
      "error": {
        "code": "SCOPE_DENIED",
        "message": "research.propose scope가 필요합니다.",
        "retryable": false
      },
      "request_id": "req_mcp_denied_demo"
    },
    "isError": true
  }
}
```

### B.6.5 장기 작업·상태 조회

리서치·캠페인 렌더링·CSV는 초기 도구 응답에서 `job_id` 또는 `run_id`를 반환하고 상태 조회 도구를 사용한다. MCP 연결을 장시간 유지하는 것으로 작업 내구성을 확보하지 않는다. 연결 종료만으로 예약한 캠페인이 취소됐다고 가정하지 않으며 `pause`, `cancel` 명령을 명시적으로 사용한다.

resources 기능을 제공할 경우 `outreach://companies/{id}` 같은 자체 URI를 사용할 수 있지만 URI를 아는 것만으로 접근할 수 없다. resources/read도 같은 ACL을 검사한다. v1.0은 도구 중심이며 prompts·sampling·임의 remote code 실행을 필수로 두지 않는다.

---

## B.7 구현 타입·Use Case 계약

아래는 구조를 보여 주는 Rust 계약 예시다. 전체 서비스 구현이나 컴파일 검증된 crate가 아니다. 권한 검사를 하지 않고 `Ok(())`를 반환하는 가짜 authorize 구현은 제공하지 않는다.

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ActorContext {
    pub workspace_id: Uuid,
    pub principal_id: Uuid,
    pub credential_id: Option<Uuid>,
    pub scopes: Vec<String>,
    pub request_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitMessageRequest {
    pub expected_payload_hash: String,
    pub not_before: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundStatus {
    Draft,
    Queued,
    Claimed,
    Submitting,
    Accepted,
    Failed,
    Unknown,
    Canceled,
}

#[derive(Debug, Serialize)]
pub struct OutboundResult {
    pub outbound_message_id: Uuid,
    pub status: OutboundStatus,
    pub job_id: Option<Uuid>,
    pub provider_accepted: Option<bool>,
}
```

핵심 Use Case의 입력은 `ActorContext + DTO + IdempotencyKey + ExpectedVersion`이며 출력은 domain result다. REST는 이를 HTTP envelope로, MCP는 structuredContent로 변환한다. Use Case 내부에서 `Authorize → Load/Lock → Validate Policy → Write State/Audit/Outbox → Commit`을 수행하고 외부 I/O가 필요한 작업은 durable job으로 넘긴다.

유효성 검사는 DTO 형식 → 리소스 존재·소속 → 권한 → 업무 불변식 → 최신 정책 순으로 분리하되, 존재 유출을 막아야 하는 객체는 권한 없는 상세 오류를 반환하지 않는다. 오류 코드와 retryability는 adapter가 임의로 바꾸지 않는다.

## B.8 계약 검증

모든 JSON 예시는 파서로 검증하고, MCP input/output Schema는 JSON Schema 2020-12 validator로 검사한다. OpenAPI 생성 결과와 Next.js 타입·MCP 도구의 주요 enum·필드 이름을 CI에서 비교한다. 경로 parameter 누락, 시간대 처리, 202 후 polling, one-click form body, ETag 충돌, 민감 토큰의 응답·로그 처리에 대한 contract test를 필수로 둔다.


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

