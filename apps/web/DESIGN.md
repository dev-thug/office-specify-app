# Outreach OS Web — Design System

`src/app/globals.css`의 토큰 정의가 단일 진실이다. 이 문서는 레이어 구조와 규칙을
설명한다. Specify 디자인 시스템을 포팅했으나 이 파일은 손으로 유지한다.

## 토큰 레이어

`globals.css`는 세 층으로 나뉜다.

1. **Foundation** (`:root` / `.dark` 실제 값 레이어) — `gray-0..12`, `white`,
   역할 색(`teal`, `amber`, `rose` 등), `brand`(graphite 신호색)을 정의한다.
   컴포넌트는 이 층을 직접 참조하지 않는다.
2. **Semantic** — `background`, `foreground`, `card`, `popover`, `primary`,
   `secondary`, `muted`, `accent`, `destructive`, `border`, `input`, `ring`,
   `brand`, `success`, `info`, `teal`, `amber`, `rose` + 각 `*-foreground`·
   `*-on-subtle`, `sidebar-*`, `app-shell`, `app-panel`, `app-panel-muted`,
   `app-border-subtle`, `glass`, `glass-border`, `scrim`. 다크 모드는 `.dark`
   스코프에서 같은 이름을 다시 정의한다.
3. **Tailwind 노출** (`@theme inline`) — 시맨틱 변수를 `bg-*`/`text-*`/
   `border-*` 유틸로 연결한다. 커스텀 크기: `text-2xs`(11px), `text-ui`(13px),
   반경 `rounded-xs..4xl`, `ease-standard/emphasized/exit`, `blur-glass`.

타이포는 Pretendard Variable(동적 서브셋 92파일) + `Outreach Residual`
(서브셋 범위 밖 글리프 보완 face — OFL Reserved Font Name 때문에 별도 이름).
둘 다 `public/fonts/pretendard/` + `OFL.txt`를 참조한다.

## 규칙

- **보라는 로고 이미지에만.** 프라이머리 액션은 잉크/그래파이트 `primary`이다.
  `brand`는 포커스 링·링크·선택 상태의 신호색이다.
- 하드코딩 금지: 컴포넌트 ts/tsx에서 `#hex`, `rgb()/hsl()/oklch()`, Tailwind
  기본 팔레트(`bg-red-500`, `text-gray-*`, `text-white` 등)를 쓰지 않는다.
- 형태: 컨트롤·배지·탭·입력 `rounded-full`, 리스트 행 `rounded-lg`,
  카드·플로팅 `rounded-2xl`, 모달 `rounded-3xl`.
- 그림자는 오버레이/모달에만. 호버 리프트 금지.
- UI 텍스트는 13–14px(`text-ui`/`text-sm`), 보조 텍스트 `text-muted-foreground`.
- 상태는 색 단독으로 전달하지 않는다 — 아이콘 + 텍스트 병기.
- `prefers-reduced-motion`: globals.css 미디어 쿼리 + `MotionConfig
  reducedMotion="user"`(루트 `MotionProvider`)로 JS 애니메이션 변형도 생략.

## 앱 셸

- 테두리 없는 260px 사이드바가 `app-shell` 면 위에 놓인다(`w-[260px]`,
  `hidden md:flex`). 헤더 52px(`h-13`), 탐색 행 36px(`h-9`, `rounded-lg`).
- 메인은 8px 떠 있는 인셋 패널: `py-2 pr-2` + `rounded-2xl border
  bg-app-panel shadow-xs`, 내부 스크롤 `panel-scroll-subtle`.
- 52px 탑바: 현재 영역 이름 + ThemeToggle + UserMenu. **md 미만**에서는 메뉴
  버튼이 `SidebarNavContent`를 담은 왼쪽 `Sheet`를 연다.
- `/app/design`은 개발 전용 카탈로그(`NODE_ENV !== "development"`면 404)로
  두 테마를 나란히 렌더한다. 일반 탐색에는 없다.

## 프리미티브

`src/components/ui/` — Specify 포팅: accordion, alert-dialog, alert, avatar,
badge, button, card, checkbox, collapsible, command, dialog, dropdown-menu,
input, label, popover, scroll-area, select, separator, sheet, skeleton, sonner,
spinner, switch, tabs, textarea, tooltip.

- `Button`: cva `variant`(default/destructive/outline/secondary/ghost/link) ×
  `size`(xs/sm/default/lg/xl/icon-xs/icon-sm/icon/icon-lg). 모든 크기 pill.
- `Badge`: solid + `*-subtle`(success/info/teal/amber/rose/brand) 변형.
- `Card`: `default` | `glass`(오버레이 반투명).
- `Dialog`/`AlertDialog`/`Sheet`: **controlled mode(`open` prop)에서는
  forceMount + Framer Motion + open-state context** — Radix `Presence`가
  CSS `animationend`에 의존해 탐색 중 오버레이가 DOM에 잔류하는 portal
  orphaning을 막는다. uncontrolled(trigger만) 모드는 CSS 애니메이션 유지.
- 시트/다이얼로그 닫기 버튼은 sr-only 라벨을 `common.close` 메시지로 렌더한다.

## Outreach 패턴 (`src/components/patterns/`)

- **StatusBadge** — 도메인 상태 문자열 → `{tone, icon, final}`. 톤:
  `success` `info` `teal` `amber` `rose` `neutral` → Badge의 solid-subtle
  매핑. `final: false` 상태(`unknown`, `confirming`, `pending`…)는 점선
  테두리로 비확정을 표현하고, 매핑에 없는 문자열은 neutral + `HelpCircle`로
  떨어진다. 라벨은 `status.*` 메시지, `label` prop으로 재정의 가능.
  작업 큐 상태는 DDL CHECK 집합 `queued/running/retry_wait/completed/dead/
  canceled/unknown`을 전부 커버한다.
- **DataTable** — 컬럼 정의 + 커서 "더 보기" + 행 선택. `RowSelection`은
  `rows`(선택한 행)와 `allFiltered`(현재 필터 전체)를 구분하고 선택 배너에
  건수를 명시한다. 로딩 스켈레톤·빈 상태·`ErrorState` 내장.
- **EmptyState / ErrorState** — ErrorState는 `ApiError`의 code·`request_id`를
  항상 표시한다.
- **ConflictDiff** — 412 `VERSION_MISMATCH` 시 base/서버/내 값 3-way 비교,
  필드별 라디오 선택.
- **ConfirmIrreversible** — 되돌릴 수 없는 작업(발송·승인) 확인: 수신자 수·
  발신 주소·목적·내용 미리보기 + 확인 문구 타이핑 요구.
- **ScreenStub** — §7.3 화면 카피로 PageHeader + EmptyState를 그린다.
- **PageHeader** — 제목 + 설명 + 우측 액션 슬롯.

## Do / Don't

| Do | Don't |
|---|---|
| `bg-primary`, `text-muted-foreground`, `border-border` 같은 시맨틱 | `bg-blue-600`, `text-white`, `#121218` 직접 지정 |
| 상태는 `StatusBadge`(아이콘+텍스트) | 색 점만으로 상태 표현 |
| 오버레이·모달에만 `shadow-*` | 카드·행 호버에 리프트/그림자 |
| 컨트롤은 `rounded-full` | 모서리 임의 px 값 |
| 문자열은 `messages/{ko,en}/<ns>.json` 키 | 컴포넌트에 UI 문구 하드코딩 |
| 확인이 필요한 파괴 작업은 `ConfirmIrreversible` | 단순 `confirm()` 대화상자 |

## lint:design

```
pnpm --dir apps/web lint:design
```

`scripts/lint-design.mjs`가 `src/**/*.{ts,tsx}`를 스캔해 hex·색 함수·Tailwind
기본 팔레트·white/black 유틸을 실패시킨다(`globals.css`는 스캔 대상이 아님 —
토큰 정의처). 위반 시 `파일:행:열 + 규칙 + 힌트`를 출력하고 exit 1.

## API usage

- 브라우저는 `apiFetch`/`apiClient`로 **같은 출처**만 호출한다 —
  `/api/v1/*`는 `next.config.ts` rewrite가 Rust API로 전달한다.
- 비-GET에는 `outreach_csrf` 쿠키 값을 `X-CSRF-Token`에 자동으로 싣는다
  (이미 설정돼 있으면 건드리지 않는다).
- **멱등 키는 논리 작업당 하나.** `login`/`acceptInvitation` 등은 호출자가
  넘긴 `idempotencyKey`를 쓴다. 폼은 `useRef`에 마운트 시 발급하고,
  성공·재시도 불가(`retryable !== true`이면서 `<500`) 응답 후에만 재발급한다 —
  네트워크 실패·5xx 재시도는 같은 키를 유지해 서버 중복 처리를 막는다.
  `withIdempotency(key)`/`ifMatch(version)` 헬퍼로 헤더를 만든다.
- 오류는 `application/problem+json` → `ApiError`(`problem.status`/`code`/
  `requestId`). 서버 `fetch`는 `serverApiFetch`(쿠키 전달 + `no-store`).
- `gen:api`(`openapi-typescript`)로 `schema.d.ts`를 재생성한다 — 빌드에
  포함되지 않는다.

## 기능 작업자 컨벤션 (Feature worker conventions)

기능 영역(area)은 다음 네 경로만 소유한다. 다른 작업자와 충돌하지 않도록
소유 범위 밖 파일은 절대 수정하지 않는다.

- `src/app/(internal)/app/<area>/**` — 라우트·페이지 (`booking`은
  `src/app/(public)/public/**` 예약 경로가 소유 범위다)
- `src/features/<area>/**` — 기능 모듈(컴포넌트·훅·상태)
- `src/lib/api/<area>.ts` — 영역 API 래퍼·DTO
- `messages/{ko,en}/<area>.json` — 영역 문자열

영역: `dashboard` `operations` `settings` `companies` `contacts` `inbox`
`templates` `campaigns` `approvals` `meetings` `booking` `compliance`
`agents` `imports` `research` — `src/i18n/namespaces.ts`의 목록이 진실이다.

### 공유 파일 편집 금지

- `src/components/ui/**`·`src/components/patterns/**` — 프리미티브·패턴.
  부족한 변형이 필요하면 복사본을 만들지 말고 작업 조정자에게 요청한다.
- `src/components/layout/**`(앱 셸)·`src/lib/nav.ts`·`src/proxy.ts`·providers.
- `src/i18n/**`와 `messages` 로더(`namespaces.ts`, `request.ts`) —
  네임스페이스는 이미 전부 선언돼 있다.
- `src/lib/api/client.ts`·`server.ts`·`problem.ts`·`mutate.ts`·
  `use-cursor-list.ts` — 공통 규약(CSRF·문제+json·envelope)은 이 파일들이
  담당한다.

### 문자열 추가

- 자기 영역 문자열은 `messages/ko/<area>.json`과 `messages/en/<area>.json`
  양쪽에 **같은 키**로 추가한다 — `messages.test.ts`가 네임스페이스별로
  ko/en 키 패리티를 검사해 한쪽만 있으면 실패한다.
- 컴포넌트에서는 `useTranslations("<area>")`로 묶어 쓴다. 기존 공통 키
  (`common.*`, `status.*`, `screens.*`)는 그대로 재사용한다.
- `nav`/`common`/`status` 등 공유 네임스페이스에 새 키가 필요하면 직접
  넣지 말고 작업 조정자에게 요청한다.

### DTO 타이핑

`pnpm gen:api`가 `schema.d.ts`를 채우기 전까지 DTO는 소스 문서 부록 B의
contracts 정의를 보고 `src/lib/api/<area>.ts`에 손으로 타이핑한다 —
`auth.ts`의 `MeData`처럼 인터페이스 + 경로 래퍼 형태. 스키마가 생성되면
`paths` 타입으로 갈아끼우되 호출부는 이 래퍼를 유지해 경로 문자열에
의존하지 않는다. 목록은 `useCursorList<T>({queryKey, path, params, limit?})`
(기본 `limit=50`, `{data, page:{next_cursor, has_more}}` envelope를 자동
페이지네이션), 비-GET은 `mutate(path, {method, json}, {etag, idempotencyKey})`를
쓴다 — 저장해 둔 ETag를 If-Match로 넣고 논리 작업당 멱등 키를 싣는다
(GET 응답의 ETag는 `readEtag(result)`로 꺼낸다).

## Deviations (Specify 대비·초기 구현 대비)

- `src/middleware.ts` → **`src/proxy.ts`** — Next 16이 middleware 컨벤션을
  proxy로 대체했다.
- **next-intl 미들웨어 미사용** — `createMiddleware`는 내부적으로 `/ko/...`
  리라이트로 `[locale]` 세그먼트를 요구한다. URL에 로케일을 넣지 않는 요구
  사항을 위해 `proxy.ts`가 `outreach_locale` 쿠키(→Accept-Language→기본 ko)를
  직접 협상하고, `i18n/request.ts`가 같은 규칙으로 해석한다.
- Specify 전용 규칙 미이식: 에디터(TipTap/Yjs)·랜딩·DaeguBukseongro/MaruBuri
  face. 잔여 글리프 face는 `Specify Residual` → `Outreach Residual`로 개명.
- `framer-motion@12.43.0` 고정 — Specify lockfile이 해석한 동일 버전(12.x, 2026-07-28 릴리스).
- 작업 큐 상태: DDL CHECK(`queued/running/retry_wait/completed/dead/canceled/
  unknown`) 기준. 초기 구현의 `succeeded`는 DDL에 없어 제거하고 `discarded`를
  추가했다.
- 로그인 리다이렉트 `next`는 `/app` 접두 + 같은 출처만 허용
  (`safeNextPath`).
