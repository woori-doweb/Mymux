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
