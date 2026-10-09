# Pretendard web font assets

## 공식 동적 서브셋

`woff2-dynamic-subset/`과 `src/app/pretendard-variable-dynamic-subset.css`는 공식 Pretendard
저장소의 `v1.3.9` 태그(`5c41199ea0024a9e0b2cb31735265056e5472d76`)에서 가져왔다.

- 원본 저장소: https://github.com/orioncactus/pretendard
- 원본 CSS: `packages/pretendard/dist/web/variable/pretendardvariable-dynamic-subset.css`
- 원본 CSS SHA-256: `2973bcae80262dcb630cfb793fbf6af29bd986c769ee54953fb3e5b3e32323ca`
- 원본 CSS의 92개 face와 Unicode 범위는 그대로 유지하고, 92개 폰트 URL만 로컬 경로로 바꿨다.

## 원본 cmap 잔여 서브셋

공식 CSS 범위와 원본 전체 폰트 cmap의 합집합을 비교해 원본에서 지원하던 2,307개 잔여 글리프를
세 파일로 나눴다. 출력 cmap은 각 입력 codepoint 집합과 일치하며, 세 집합은 서로 겹치지 않는다.

| 파생 face | 글리프 수 | 크기 |
|---|---:|---:|
| 공통 UI 기호 | 7 | 2,836 B |
| CJK 잔여 문자 | 69 | 9,732 B |
| 나머지 잔여 문자 | 2,231 | 205,972 B |

세 파일은 FontTools 4.60.2로 WOFF2 variable flavor를 유지해 만들었다. 원본 `wght` axis는
45–930 범위로 보존됐고, CSS 공개 범위는 기존 동작과 같이 45–920이다. 파생 폰트의 내부 가족명은
OFL의 Reserved Font Name 조건을 지키기 위해 `Specify Residual`로 변경했다.

분할 기준은 테스트에 고정돼 있다. 공통 UI face는 `U+B5`, `U+2212`, `U+2318`, `U+26A0`,
`U+2713`, `U+27FA`, `U+E002`를 담는다. CJK face는 잔여 cmap 중 `U+2E80–U+9FFF`,
`U+F900–U+FAFF`, `U+FF00–U+FFEF`에 속하는 글리프를 담고, 나머지 잔여 글리프는 세 번째 face에 둔다.

원본 전체 파일은 `packages/pretendard/dist/web/variable/woff2/PretendardVariable.woff2`이며
SHA-256은 `9599f12fd42fc0bce1cd50b47a0c022e108d7aa64dd0d1bb0ed44f3282d900b4`다. cmap은
14,336개 codepoint로 확인했다. 검증용 압축 cmap 범위는
`scripts/test-fixtures/pretendard-v1.3.9-full-cmap-ranges.txt`에 보관한다. 전체 2 MB 파일은
검증을 마친 뒤 `public/`에서 제거해 배포 경로에서 요청되지 않게 했다.

## 라이선스와 무결성

`OFL.txt`는 같은 태그의 `packages/pretendard/dist/LICENSE.txt` 원본에서 줄 끝 공백만 제거했다.
원본 바이트 SHA-256은 `b04538c9abec39a3db75108cf0af0fd9c77032fe8aa2cf38345b4d250e98e`,
저장한 라이선스 SHA-256은 `85fce85e25260b03777bf10373d3bd9363b9da96d9e0ca86a280dd37ed7667a0`이다.
SIL Open Font License 1.1의 문구는 바꾸지 않았다. 공식 동적
서브셋 92개와 파생 잔여 서브셋 3개의 SHA-256은 `SHA256SUMS`에 기록했다.

글로벌 CSS는 동적 서브셋 CSS를 빌드 시 로컬 import한다. CSS CDN 요청 없이 필요한 WOFF2 파일만
`/fonts/pretendard/` 경로에서 직접 가져온다. 자산 테스트는 로컬 URL, 원본 CSS와 Unicode 범위,
전체 cmap과 잔여 face의 정확한 합집합, 겹치지 않는 범위, 라이선스와 체크섬을 검증한다.
