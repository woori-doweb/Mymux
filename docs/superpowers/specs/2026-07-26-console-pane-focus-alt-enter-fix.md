# 콘솔 분할 패인 포커스 고착 + Alt+Enter 줄바꿈 수정

날짜: 2026-07-26 · 브랜치: `feature/mymux-server-console`
대상: `crates/mymux-server/static/app.js` (+ 데스크톱 `crates/mycli-desktop/frontend/app.js` Alt+Enter 동일 가드)

## 증상
1. 상단 툴바(⬌/⬍)로 분할한 뒤 포커스가 계속 한쪽 패인으로만 감 — 다른 패인을
   클릭해도 입력이 안 들어가거나 원래 패인으로 되돌아감.
2. Alt+Enter를 눌러도 터미널(Claude Code 등)에서 줄내림(소프트 개행)이 안 됨.

## 원인 (3중)

### 1. onData 포커스 추적이 터미널 생성 데이터에 반응
`term.onData`의 `if (focused !== pane) focusPane(pane)`는 "타이핑하면 그 패인을
포커스로 동기화"하려는 코드지만, onData에는 사용자 입력만 오지 않는다. xterm이
**스스로 생성한 데이터**도 통과한다:
- focus report `CSI I`/`CSI O` — **mode 1004, Claude Code가 켬**
- DA/DSR 질의 응답, SGR 마우스 리포트(1002/1006)

claude 패인에서 다른 패인을 클릭하면 claude 패인 textarea가 blur →
xterm이 `\x1b[O`를 emit → onData가 `focusPane(claude패인)`을 되불러
포커스 전환이 중첩 취소된다. Chromium 실측: `.focused` 클래스는 새 패인인데
`document.activeElement`는 `<body>`로 떨어져 **키보드가 죽은 상태**가 됨.
(데스크톱 app.js의 onData 주석에 이미 문서화된 동일 함정.)

### 2. 패인 크롬(테두리/여백) 클릭 시 네이티브 blur
포커스 불가능한 div를 클릭하면 브라우저 기본 mousedown 동작이 포커스를
`<body>`로 옮긴다. mousedown 핸들러 안에서 동기적으로 `term.focus()`를 해도
기본 동작이 그 뒤에 실행되어 무효화됨 → **다음 tick으로 미뤄 재확정**해야 함.

### 3. Alt+Enter가 IME/키보드 배열에 따라 무시됨
xterm은 keydown의 `key === "Enter"`일 때만 `ESC CR`(`\x1b\r`)을 만든다.
- 한글 IME가 켜져 있으면 `key: "Process"`(keyCode 229)로 도착 → 아무것도 전송 안 됨
- 한국어 배열의 오른쪽 Alt(한자키)가 `AltGraph`로 잡히면 third-level shift로
  취급되어 역시 무시됨

`\x1b\r`(Meta+Enter)은 Claude Code·codex류 TUI가 "제출 없이 줄바꿈"으로
해석하는 시퀀스다.

## 수정 (crates/mymux-server/static/app.js)

1. **onData 포커스 추적을 ESC 비시작 데이터로 제한**
   `if (focused !== pane && d.charCodeAt(0) !== 27) focusPane(pane)`.
   ESC로 시작하는 실제 키(화살표 등)는 DOM 포커스된 패인에서만 오므로
   그때는 이미 `focused === pane`이라 손실 없음.
2. **입력 라우팅을 패인 자신의 소켓으로** — 기존 `sendRaw(d)`는 `focused.ws`로
   보냈다. 1번 수정 후 백그라운드 패인의 자동 응답(`\x1b[O`, DSR)이 **다른
   패인의 PTY에 주입**될 수 있어 `pane.ws.send(...)`로 변경. `ctrlArmed`
   변환(모바일 Ctrl 버튼)은 `focused === pane`일 때만 적용.
   ※ 불변식: **onData(pane X) → 반드시 pty X**. `sendRaw`는 autocomplete/
   keybar/saved-command처럼 "포커스된 패인에 보내는" 경로 전용.
3. **mousedown을 capture 단계 + 다음 tick 포커스 재확정** — 마우스 추적 TUI가
   이벤트를 삼켜도(capture는 target보다 먼저 실행) 상태가 갱신되고,
   `setTimeout(0)`으로 브라우저 기본 동작 이후 `pane.term.focus()`를 재확정.
   중클릭 붙여넣기는 bubble에 남김(추적 TUI가 소비하면 raw 중클릭 유지).
4. **keyHandler에서 Alt+Enter 명시 처리** — `e.altKey && !ctrl && !meta &&
   (key==='Enter' || code==='Enter' || code==='NumpadEnter')` → preventDefault
   후 `focused.term.input('\x1b\r')`. `term.input()`은 onData를 경유하므로
   위 라우팅과 일관.

데스크톱(`crates/mycli-desktop/frontend/app.js` attachCustomKeyEventHandler)에도
동일한 Alt+Enter 가드 추가 — `term.input()` 경유라 탭 브로드캐스트
(Ctrl+Shift+B) 경로 유지. 데스크톱의 포커스 로직은 별도 구조(포커스 키퍼)라
1~3번은 해당 없음.

## 검증 (실서버 + headless Chromium E2E)

로컬 검증 절차(재사용 가능):
1. `cargo build -p mymux-server`, `/tmp` 임시 config(`cookie_secure=false`,
   `bind=127.0.0.1:17070`, `allowed_client_cidrs=["127.0.0.0/8"]`)
2. `mymux-server user create`(rpassword는 TTY 필요 — pty로 입력)
3. chromium headless로 로그인 → 탭 생성 → 분할 → pane0에
   `printf '\e[?1004h\e[?1002h\e[?1006h'`로 claude와 동일한 모드 재현
4. WebSocket.prototype.send 래핑으로 input 프레임 검사

| 시나리오 | 수정 전 | 수정 후 |
|---|---|---|
| 추적 TUI 패인 ↔ 일반 패인 클릭 왕복 | OK | OK |
| 패인 테두리(코너) 클릭 | ❌ `.focused`≠실포커스, activeElement=BODY, 타이핑 무전송 | ✅ textarea 포커스, `x` 프레임 정상 |
| Alt+Enter (실키) | 환경 의존 | ✅ `\u001b\r` 전송 |
| Alt+Enter (IME `Process`) | ❌ 프레임 없음 | ✅ `\u001b\r` 전송 |
| 터치 탭 전환 (CDP touch emulation) | OK | OK |

## 다음 작업 시 참고
- 콘솔에서 "포커스가 이상하다" 계열 버그는 먼저 **onData에 뭐가 흘러들어오는지**
  의심할 것 — mode 1004(focus report), 1002/1006(마우스), DSR 응답이 단골.
- 패인별 입력은 반드시 `pane.ws`로. `focused` 기준 전송을 onData에 되돌리면
  cross-PTY 주입이 재발한다.
- Shift+Enter 줄바꿈은 이번 범위에서 제외(셸 호환성 검토 필요 — kitty/foot은
  CSI u를 보냄). 요구가 생기면 Alt+Enter와 같은 자리(keyHandler)에서 처리.

## 후속 (같은 날): "다중 분할에서 첫번째 터미널로 포커스가 계속 옮겨감"

### 원인 1 — 배포/캐시 갭 (가장 컸음)
- 오전 수정이 **배포되지 않은 상태**였다. 실서비스는 `/opt/mymux-console/static`
  (7/19자)을 서빙 중 → `deploy/deploy.sh`(static-only, 무중단)로 반영.
- 접속 경로는 nginx가 아니라 **Tailscale Serve**:
  `https://localhost-0.tail2ca93e.ts.net:9443 → 127.0.0.1:7070` (중간 캐시 없음).
- mymux-server가 `Last-Modified`만 보내고 `Cache-Control`이 없어 브라우저
  **휴리스틱 캐시**(수정시각 이후 경과의 10%)가 적용됨 — 7일 묵은 app.js면
  최대 ~17시간 재검증 없이 캐시 재사용. 게다가 SPA 탭은 스스로 리로드하지
  않으므로 열려 있는 탭은 영원히 옛 코드로 돈다.
- 수정: `routes.rs`에 `Cache-Control: no-cache`(재사용 전 재검증, 304로 저렴)
  전역 레이어 추가. **바이너리 교체 + 서비스 재시작 필요** — 재시작은 서비스
  cgroup 아래의 라이브 터미널(bash/claude/gjc)을 전부 죽이므로 한가한 시점에
  `deploy/deploy.sh --build`로 반영할 것. (이번엔 static만 배포함.)

### 원인 2 — closePane이 항상 첫 패인으로 포커스
- `closePane`이 `firstPane(t.root)`로 포커스를 보내서, 3중 분할에서 어떤
  패인을 닫아도 **1번 터미널로 점프**했다.
- 수정: 닫힌 패인의 공간을 물려받는 **형제 서브트리의 firstPane**으로 포커스
  (`firstPane(sibling) || firstPane(t.root)`). tmux 동작과 일치.
- E2E: A|(B|C)에서 C 닫기 → B 포커스, 가운데 패인 닫기 → 자리 물려받은 패인
  포커스, 이후 클릭 왕복 정상 (headless Chromium 확인).

### 배포 상태 체크리스트 (다음에 또 "여전히 안 됨"이 나오면)
1. `/opt/mymux-console/static/app.js` mtime vs repo — `deploy/deploy.sh --dry-run`
2. 사용자 브라우저가 새 JS를 받았는지: Cache-Control 반영 전이면 **Ctrl+F5** 필요
3. 열려 있는 콘솔 탭은 리로드 전까지 옛 코드 — 반드시 새로고침 안내

## 후속 2 (같은 날): "Win11 + Brave에서 Alt 누르면 + 커서, 줄바꿈 안 됨"

- **+ (십자) 커서 = xterm의 Alt+클릭 선택 모드 표시** — Alt가 페이지까지
  도달한다는 증거일 뿐 오동작이 아니다.
- **실제 claude TUI로 종단 검증 완료**: 로컬 테스트 인스턴스에서 `claude`
  (2.1.220)를 패인에 띄우고 headless Chromium 실키로 Alt+Enter/Shift+Enter
  입력 → 입력창이 `hello / world-alt / third-shift` 3줄로 늘어나는 것을
  스크린샷으로 확인. **ESC CR → claude 줄바꿈 해석은 이 서버에서 실증됨.**
- 따라서 "여전히 안 됨"의 잔여 원인은 사용자 Brave 탭의 **옛 app.js**
  (Cache-Control 미반영 상태의 휴리스틱 캐시 + SPA 탭은 자체 리로드 없음).
  journald에는 HTTP 접근 로그가 없어(TraceLayer=debug) 클라이언트가 새 JS를
  받았는지 서버에서 확인 불가 — 사용자에게 **Ctrl+F5** 안내가 유일한 확인로.
- **Shift+Enter도 줄바꿈으로 추가** (`(e.altKey || e.shiftKey) + Enter` →
  `\x1b\r`): 채팅앱 습관 + Claude Code `/terminal-setup` 관례와 일치.
  참고: bash 프롬프트에서 Shift+Enter는 이제 ESC CR이 되어 즉시 실행이 아닐
  수 있음(readline M-RET) — claude 중심 사용에선 이득이 더 크다고 판단.
- 항상 동작하는 폴백: `\` 입력 후 Enter (claude 자체 기능, 프론트 무관).
- **사용법 명세**: 수정자 키를 **누른 채** Enter를 눌러야 한다 (Enter keydown
  시점에 `altKey`/`shiftKey`가 true — Ctrl+C와 같은 원리). Alt를 탭했다 떼고
  Enter는 일반 제출. Alt를 누른 동안 커서가 +(십자)로 바뀌는 건 xterm의
  Alt+드래그 선택 표시로 정상.
- **UI에 사용법 노출 + 모바일 구멍 해결**: 웰컴 화면에 줄바꿈 키 안내 추가,
  모바일 키바에 ⏎ 버튼(`KEY_SEQ.nl = '\x1b\r'`) 추가 — 폰에는 Alt가 없어
  줄바꿈 방법이 아예 없었다. 터치 탭으로 `\u001b\r` 프레임 전송 확인.
- E2E 시 주의: puppeteer `setViewport({isMobile})`는 **페이지를 리로드**한다 —
  WebSocket.prototype 래퍼가 사라지므로 뷰포트 변경 후 다시 감쌀 것.

## 후속 3 (같은 날): "여전히 안 됨" → 라이브 스택 직접 검증으로 종결

- **사용자와 동일한 접근 경로로 라이브 E2E 완료**: 임시 계정(gjc-e2e, 검증 후
  disable)을 라이브 DB에 만들고 headless Chromium으로
  `https://localhost-0.tail2ca93e.ts.net:9443` (Tailscale Serve → 7070,
  cookie_secure=true, CIDR 100.64/10) 경유 로그인 → 라이브가 서빙하는
  app.js에 수정 포함 확인 → 라이브 패인에서 claude 실행 → Alt+Enter /
  Shift+Enter로 `line1 / line2-alt / line3-shift` 3줄 입력 스크린샷 확보.
  **배포 환경 자체는 정상 동작이 증명됨.**
- 남는 변수는 클라이언트 탭이 실행 중인 JS뿐 → **상단바 빌드 태그** 추가:
  `MYMUX_BUILD` 상수를 `#whoami` 옆에 표시 (`user (role) · 2026-07-26.4`).
  원격 지원 시 "상단에 2026-07-26.4 보이나요?"로 즉시 판별.
  **규칙: static 배포 때마다 MYMUX_BUILD를 올릴 것** (deploy.sh의 cmp 검증
  때문에 배포 후 sed 스탬핑은 불가 — 소스에서 올리고 배포).
- 라이브 테스트 계정 정리: 터미널 종료·탭 닫기(DELETE) 후
  `user disable --username gjc-e2e` (세션 1개 회수 확인).
- 참고: 127.0.0.1:7070 직접 접속은 login/API가 CIDR(100.64/10)에 걸릴 수
  있으므로 라이브 검증은 반드시 ts.net URL로.

## 후속 4 (같은 날): 대상 TUI는 claude가 아니라 **gjc** — 줄바꿈 바이트 교체

- 사용자 정정: 줄바꿈이 안 되는 건 **gjc 입력창**. gjc TUI 소스
  (`@gajae-code/tui/src/components/editor.ts`) 분석 + 실측 결과:
  - `\x1b\r`(기존 전송값)은 gjc가 **별도 "alt+enter" 액션으로 소비** → 줄바꿈 안 됨
    (실측: `aaabbb` 붙음). claude에서만 줄바꿈이었음.
  - gjc 고유 shift+enter 인코딩 `\x1b[13;2~`(psmux 패턴)는 gjc에선 줄바꿈이지만
    **bash 프롬프트에 `;2~` 잔여 문자를 흘림** → 탈락.
  - **`\n`(LF, Ctrl+J)이 유니버설**: gjc 줄바꿈 ✅ (editor.ts의
    `data === "\n"` 분기), claude 줄바꿈 ✅ (실측), bash에선 Enter와 동일(무해).
    gjc 입력창 placeholder도 "Shift+Enter/Ctrl+J: New line"으로 안내.
- 수정: 콘솔 Alt+Enter/Shift+Enter/모바일 ⏎(`KEY_SEQ.nl`) 모두 `\n` 전송,
  데스크톱 Alt+Enter도 `\n`으로 통일. 빌드 태그 2026-07-26.5 배포.
- 검증: 같은 로컬 스택에서 **실키**로 claude(`c2-alt`/`c3-shift`)와
  gjc(`g2-alt`/`g3-shift`) 모두 3줄 입력 스크린샷 확보.
- 교훈: "터미널에서 줄바꿈" 요구는 **대상 TUI별로 기대 바이트가 다르다**.
  다음에 새 TUI가 추가되면 `\n` 해석부터 실측할 것. gjc 키 파서는
  `@gajae-code/tui/src/keys.ts` (kitty CSI-u + psmux `\x1b[13;2~` + 네이티브).
