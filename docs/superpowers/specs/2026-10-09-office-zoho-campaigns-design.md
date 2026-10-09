# Office — Zoho Campaigns 마케팅 연동 설계

상태: 사용자 승인 — 2026-10-09. 이 문서는 구현 완료나 실제 계정 검증을 의미하지 않는다.

확인된 계정 조건: `https://campaigns.zoho.com/`, Campaigns 무료 플랜,
Zoho Mail 유료 플랜. 첫 연결은 US Accounts/Campaigns 호스트를 사용하며 예약·
웹훅·다단계 자동화는 활성화하지 않는다. Zoho Mail의 유료 기능을 Campaigns의
기능으로 간주하지 않는다. Mail의 실제 수신/회신 연동은 이번 변경으로 검증되지 않는다.

## 1. 목적과 성공 기준

Office에서 수신 동의가 확인된 CRM 연락처를 Zoho Campaigns로 동기화하고,
Office의 템플릿과 승인 흐름을 사용해 마케팅 캠페인을 발송하며 결과를 조회한다.
사용자가 요청한 서비스는 Zoho **Campaigns**이며 Zoho Mail SMTP/API와 구분한다.
기존 개인 메일함의 회신·일반 업무 메일은 기존 경로를 유지한다.

첫 구현 범위는 단일 단계 마케팅 캠페인의 생성·검토·즉시 발송·결과 동기화다.
예약은 해당 계정의 기능 확인 후 제공한다. 무료 플랜에서 예약·웹훅·자동화가
가능하다고 가정하지 않는다. 기존 다단계 시퀀스를 Zoho 워크플로로 임의 변환하지
않으며, 미지원 조합은 화면과 서버에서 명확하게 차단한다.

완료 기준은 다음과 같다.

- 설정에서 관리자가 OAuth 연결, 연결 확인, 재연결과 해제를 할 수 있다.
- 마케팅 캠페인에서 연결 계정·검증된 발신자·주제(topic)를 선택한다.
- 동의·수신거부 검사 후 확정한 수신자만 전용 목록에 동기화한다.
- Office 승인과 Zoho 검토 상태를 모두 확인한 캠페인만 발송 요청할 수 있다.
- 발송 불명 상태에서 자동 재발송하지 않고 공급자 상태를 조회한다.
- 수신거부·반송·발송 결과를 CRM에 반영하고 다음 캠페인에서 다시 검사한다.
- 모의 HTTP 통합 테스트와 브라우저 검증, 실제 계정 검증 결과를 구분해 기록한다.

## 2. 공식 문서에서 확인한 제약

확인 날짜: 2026-10-09, Asia/Seoul.

| 항목 | 확인 내용 | 설계 결정 |
|---|---|---|
| 인증 | API v1.1, OAuth 2.0, 액세스/리프레시 토큰 | 백엔드에서 교환·갱신하고 암호화 저장 |
| 캠페인 생성 | 제목·발신자·목록과 `content_url`, 계정에 따라 `topicId` 필수 | 계정별 topic을 선택하고 서버가 불변 HTML을 제공 |
| 발송 | 캠페인 키를 사용하는 별도 POST | 수신자별 기존 send 함수에 bulk API를 끼워 넣지 않음 |
| 응답 | HTTP 성공과 별개인 문자열/숫자 `code`, 일부 응답은 `response`로 감싸짐 | 엔드포인트별 성공 코드와 업무 오류를 명시적으로 파싱 |
| 보고서 | 캠페인 요약 통계와 상세 항목 | 집계 수치를 개별 수신자의 배달·열람으로 추정하지 않음 |
| 무료 플랜 | Developer API 지원, 예약·웹훅은 비교표에서 미지원 | 기본은 즉시 발송과 폴링, 한도는 실제 계정 확인 후 설정 |
| 구독 API | 목록 설정에 따라 확인 메일을 보내며, 성공 응답이 구독 완료와 다를 수 있음 | 동기화 요청 수와 실제 구독 완료 수를 분리 |

공식 근거:

- [API 인증·권한](https://www.zoho.com/campaigns/help/developers/access-token.html)
- [캠페인 생성](https://www.zoho.com/campaigns/help/developers/create-campaign.html)
- [캠페인 발송](https://www.zoho.com/campaigns/help/developers/send-campaign.html)
- [예약 발송](https://www.zoho.com/campaigns/help/developers/schedule-campaign.html)
- [목록 조회](https://www.zoho.com/campaigns/help/developers/get-mailing-lists.html)
- [구독 동기화](https://www.zoho.com/campaigns/help/developers/contact-subscribe.html)
- [수신거부 반영](https://www.zoho.com/campaigns/help/developers/contact-unsubscribe.html)
- [캠페인 보고서](https://www.zoho.com/campaigns/help/developers/campaign-reports.html)
- [플랜 비교](https://www.zoho.com/campaigns/comparison.html)
- [OAuth 데이터센터](https://www.zoho.com/developer/oauth/multi-dc-support.html)

문서로 확정하지 못한 기능은 추정한 API 경로나 요청 파라미터로 구현하지 않는다.
목록 생성, topic/발신자 조회, 공급자 검토 요청, 개인별 이벤트 조회, merge field
매핑은 구현 시 해당 공식 계약을 추가 확인한다. API로 검증할 수 없는 상태는
관리자가 Zoho에서 확인하는 작업으로 표시하며 검증된 기능처럼 활성화하지 않는다.

## 3. 현재 구조와 통합 방식

현재 `DeliveryProvider::send`는 하나의 수신자와 이미 렌더링된 메시지를 받는다.
Zoho의 `sendcampaign`은 하나의 목록 또는 세그먼트 전체를 처리하므로 이 함수를
그대로 구현하면 수신자 수만큼 같은 캠페인을 재발송할 위험이 있다.

새 `MarketingCampaignProvider` 경계를 도입하고 기존 `DeliveryProvider`와 분리한다.
공급자 계정과 Office 캠페인 launch/step을 영속적으로 매핑하고, 하나의 승인된
스냅샷에 대해 한 번만 공급자 발송을 요청한다. 기존 수신자별 dispatch와 Zoho
bulk dispatch가 동시에 생성되지 않도록 경로 선택을 트랜잭션에서 고정한다.

`zoho_campaigns` 공급자 식별자를 연결/채널 제약에 추가하되 marketing 전용으로
허용한다. 기존 `zoho_mail`의 마케팅 금지는 그대로 유지한다. 기존 공용 제약과
불변 감사·승인 규칙을 우회하지 않는다. 적용된 SQL 마이그레이션은 수정하지 않는다.

## 4. 화면 흐름

### 연결 설정

설정/연동에 Zoho Campaigns 영역을 추가한다. 관리자만 OAuth 연결을 시작할 수
있으며 서버 설정이 없는 경우 필요한 설정과 callback 주소를 표시한다. 계정의
데이터센터·연결 상태·마지막 확인 시각·지원 기능을 표시하고 토큰은 표시하지 않는다.

서버 운영자가 client ID/secret과 고정 redirect URI를 서버 환경 또는 SecretStore에
설정한다. 사용자는 Zoho 인증 화면에서 계정 접근을 승인한다. 채팅이나 일반 화면에
client secret/refresh token을 붙여 넣게 하지 않는다.

### 캠페인

마케팅 목적을 선택하면 Zoho Campaigns 연결, 발신자, topic을 선택할 수 있다.
Office의 HTML 편집기를 유지하고 다음 순서로 진행한다.

1. 템플릿과 대상 CRM 연락처를 선택한다.
2. 동의 상태, 수신거부, 중복 주소, 계정 한도와 지원 변수를 사전 검사한다.
3. 콘텐츠·수신자·필드 매핑·발신자·topic·요청 시간을 불변 스냅샷으로 확정한다.
4. 목록과 초안을 준비하고 동기화 결과·Zoho 검토 상태를 표시한다.
5. 실제 대상 수와 제외 사유, 발신자, 제목/본문을 사람이 확인하고 승인한다.
6. 승인된 키에 대해 즉시 발송을 요청한다. 예약 지원 계정에서는 예약을 추가한다.
7. 공급자 수락·검토 대기·진행 중·완료·실패·불명 상태와 갱신 시각을 표시한다.

Office 승인이 Zoho의 콘텐츠 검토를 대체하지 않는다. 공급자에서 내용/수신자가
수정되면 기존 Office 승인을 재사용하지 않는다. 이미 발송된 캠페인의 변경은 새
스냅샷으로 처리한다. 공급자가 허용하지 않는 취소를 성공으로 표시하지 않는다.

## 5. 인증과 계정 기능

OAuth state는 워크스페이스·관리자·목적·만료에 바인딩하고 일회성으로 소비한다.
callback에서 로그인한 관리자와 state 소유자를 재확인한다. 코드·토큰을 감사 기록,
idempotency 응답 또는 URL 로그에 저장하지 않는다. 액세스 토큰 갱신은 연결별로
직렬화하고 토큰 교체와 기존 refresh token 보존 규칙을 테스트한다.

데이터센터는 서버 allowlist의 정확한 Accounts/Campaigns 호스트 쌍으로 처리한다.
현재 레거시 `accounts.zoho.<region>` 문자열 생성은 모든 데이터센터에 정확하지
않으므로 새 연결에 재사용하지 않는다. 미검증 지역은 명시적으로 비활성화한다.
임의 호스트나 callback의 URL을 신뢰하지 않으며, 공급자 요청의 리다이렉트도 막는다.

필요 권한은 공식 엔드포인트에 맞춘 `campaign.CREATE/READ/UPDATE`,
`contact.READ/UPDATE`이며 목록 생성이 필요한 경우에만 `contact.CREATE`를 추가한다.
DELETE/ALL 권한을 편의상 요청하지 않는다. 플랜 한도와 예약·웹훅 기능은 별도
검증 상태로 저장한다. 무료 플랜 숫자를 모든 계정에 하드코딩하지 않는다.

## 6. 수신자, 개인화, 동의

공유 목록의 현재 구성에 그대로 발송하지 않는다. Office launch 전용 목록과
로컬 대상 스냅샷을 매핑하고 실제 구독 완료 대상이 승인된 집합과 일치하는지
확인한다. 중복 이메일은 한 번만 포함한다. 기존 Zoho 수신거부 연락처를 동기화
명목으로 재구독시키지 않는다. 확인 메일이 필요한 연락처는 확인 대기로 남긴다.

`company.name`, `contact.name`, `sender.display_name`은 검증된 Zoho 필드/merge tag
매핑만 허용한다. 지원되지 않는 변수를 빈 문자열로 바꾸지 않고 사전 검사에서
차단한다. 수신자별 예약 URL을 지원하려면 서버에서 수신자별 링크를 발급하고
전용 필드 매핑·동시성 격리를 검증해야 한다. 그 검증 전에는 예약 URL을 포함한
Zoho bulk 캠페인을 활성화하지 않는다.

서로 다른 진행 중 캠페인이 같은 연락처의 개인화 필드를 덮어쓰지 못하도록
매핑 격리 또는 연락처 단위 활성 캠페인 잠금을 사용한다. 공급자 merge 결과를
검증할 수 없으면 대상/내용이 정확히 일치한다고 보고하지 않는다.

수신거부는 양방향으로 반영한다. Office에서 거부된 주소는 다음 동기화/발송 전에
Zoho에서 제외하고, Zoho의 거부·불만·영구 반송을 Office suppression에 반영한다.
확정 전과 실제 발송 직전에 동의/거부를 다시 검사한다. 동기화 지연이나 일부 실패는
표시하고 관련 발송을 대기시킨다. `compliance.unsubscribe_url`은 기존 Office 토큰을
전체 목록에 공유하지 않으며 Zoho의 수신자별 unsubscribe 매핑을 검증해서 사용한다.
one-click 헤더 지원도 실제 검증 전에는 지원한다고 표시하지 않는다.

## 7. 콘텐츠 전달

공식 생성 API의 `content_url`을 위해 서버가 승인된 HTML 스냅샷만 제공하는
HTTPS endpoint를 둔다. 개인화된 CRM 원문과 토큰을 공개 본문에 넣지 않는다.
Zoho가 해석할 merge tag가 포함된 콘텐츠와 승인된 일반 마케팅 본문만 제공한다.

URL은 고엔트로피 capability로 보호하고 DB에는 해시를 저장한다. 만료·폐기,
워크스페이스·스냅샷 바인딩, 접근 범위와 길이를 검증한다. 로그에서 capability를
마스킹하며 응답에는 no-store, noindex와 CSP를 적용한다. 콘텐츠를 수정하면 새
스냅샷/URL을 만든다. 만료 기간은 공급자의 검토·재수집 요구를 확인해 설정한다.
Zoho가 반복 조회할 수 있어 조회 한 번으로 무조건 토큰을 소비하지 않는다.

localhost는 외부 Zoho가 가져갈 수 없다. 공개 HTTPS origin이 없으면 연결과
목록 조회는 가능하더라도 Office 콘텐츠 업로드와 실제 발송 준비는 차단한다.
테스트용 HTTP 서버는 어댑터 테스트에서만 허용하고 운영 설정으로 노출하지 않는다.

## 8. 영속 상태와 작업

모든 새 테이블은 workspace 복합 키/FK, RLS, api/worker 최소 권한을 적용한다.
연결 비밀은 기존 SecretStore에 저장하고 DB에는 참조만 남긴다.

- 연결 설정: 공급자 계정 식별자, DC, 기능 확인 상태, sender/topic 매핑, 확인 시각.
- export/snapshot: launch·step·템플릿 hash·수신자 hash·공급자 목록/캠페인 키.
- recipient mapping: 정규화 이메일 ID·동기화/구독 상태·필드 매핑 버전.
- operation journal: 외부 작업 종류·correlation key·processing/submitting/unknown 상태.
- report checkpoint: 조회 범위·커서·마지막 성공·집계와 실제 개인별 이벤트 구분.

작업 payload와 outbox에는 ID/버전만 포함한다. 외부 작업 전 의도를 커밋하고 작업별
lease를 fence한다. 공급자 키와 결과를 저장하기 전 프로세스가 종료되면 불명 상태로
격리한다. 생성/발송 타임아웃을 무조건 재실행하지 않는다. 동일 correlation에 대해
고유 제약으로 중복 작업을 막되, 공급자의 idempotency가 문서화돼 있지 않다면 이를
지원한다고 가정하지 않는다. 불명 상태는 조회로만 조정하며 근거 부족 시 수동 검토한다.

발송 요청 시 정책, 비상 정지, 복원 잠금, 승인 hash, 연결 상태, 수신거부 동기화,
공급자 검토 상태와 계정 한도를 모두 확인한다. 에이전트는 기존처럼 승인·최종 발송을
할 수 없고 초안 작성/사전 검사/결과 조회 범위만 사용한다.

## 9. 검증 계획과 실제 계정 준비물

모의 HTTP 서버로 OAuth state 재사용/만료/워크스페이스 공격, 토큰 갱신, 비밀정보
비노출, form encoding, 응답 변형, 코드별 실패, 401/429/5xx/타임아웃을 검증한다.
DB/API 테스트로 대상 집합·중복 주소·동의/거부·승인 변경·bulk/개별 이중 dispatch
차단·작업 중단 복구·집계와 개별 이벤트 구분·권한 경계를 확인한다.
브라우저에서는 설정→연결 상태→대상 선택→미리보기→승인→상태 조회를 검증한다.

실제 계정 검증에는 Zoho Campaigns 계정/DC, OAuth 클라이언트, 등록한 callback,
검증된 발신 주소/도메인, topic, 계정의 플랜/한도, 외부에서 접근 가능한 HTTPS
주소가 필요하다. 우선 테스트 수신자만 사용하는 별도 목록으로 검증한다. 실제
연락처 동기화와 발송은 사용자가 UI에서 해당 대상과 내용을 확인해 실행한다.

이 설계는 기존 fake-only 결정 X4를 Zoho Campaigns 범위에서 확장한다. 공급자
연동 코드·모의 테스트 통과가 실제 계정 검증이나 다른 운영/보안/법무 게이트의
자동 승인을 뜻하지 않는다. 본문·수신자·정책의 최종 승인 주체는 계속 사람이다.
