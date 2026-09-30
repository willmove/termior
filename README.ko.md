<p align="center">
  <img src="assets/termior-logo.svg" width="128" height="128" alt="Termior logo">
</p>

# Termior

[English](README.md#english) · [简体中文](README.md#简体中文) · [繁體中文](README.zh-TW.md) · [日本語](README.ja.md) · 한국어 · [Español](README.es.md) · [Deutsch](README.de.md)

---

## 한국어

> 오픈소스, 크로스 플랫폼, 터미널 우선 AI 네이티브 개발 환경(ADE). BYOK, 로컬 우선, 계정 불필요, 텔레메트리 없음.

Termior는 실제 PTY 터미널, 가벼운 코드 에디터, 파일 탐색기, Git, 웹 미리보기, 그리고 도구 호출·승인·변경 검토·복구 기능을 갖춘 내장/외부 에이전트를 하나의 네이티브 창에 통합합니다. 제품 및 엔지니어링 사양은 [docs/termior-spec.md](docs/termior-spec.md)를 참고하세요.

### 핵심 가치

1. **네이티브 저지연** — 코어 UI에는 JS 런타임과 IPC 직렬화 경계가 없습니다. 터미널과 에디터는 GPU에서 직접 렌더링하며, 상시 60 fps, 고주사율 디스플레이에서 120 fps를 목표로 합니다.
2. **터미널과 AI의 깊은 협업** — 실제 PTY, 실시간 cwd/버퍼 컨텍스트, 도구 호출, 승인, hunk 단위 diff 검토가 같은 워크스페이스에 모여 있습니다.
3. **로컬 우선과 사용자 통제** — BYOK, 키는 OS 키체인에만 저장, 텔레메트리 없음, 계정 불필요. LM Studio, MLX, Ollama 같은 로컬 추론 서버를 지원합니다.
4. **명확한 보안 경계** — 내장 에이전트의 디스크·셸·시크릿·네트워크 조작은 모두 워크스페이스 인가, 승인, deny-list, SSRF 정책 게이트를 통과합니다. 외부 에이전트는 자신의 백엔드, 실행 환경, 실제 격리 능력과 미지의 항목을 명시하며, 백엔드 자체 조작을 내장 게이트로 보호되는 것처럼 위장하지 않습니다.

### 현재 데스크톱 마일스톤

워크스페이스는 순수 로직 프로토타입에서 빌드 가능한 GPUI 데스크톱 워크벤치로 발전했습니다. 현재 포함된 기능:

- 영속 워크스페이스, 8종 탭, 분할 창, Explorer / Source Control / History 사이드바, 상태 표시줄, 별도 설정 창;
- portable-pty + alacritty_terminal 기반 실제 터미널: 셸 통합(bash/zsh/fish/PowerShell), OSC 7/133/777, IME, 검색, 스크롤백, URL/localhost 감지, Windows Job Objects. 설정에서 기본 셸 선택(시스템 기본 / 감지된 셸(Git Bash, WSL 배포판 포함) / 수동 경로)과 새 터미널 생성 시 셸 선택 메뉴(선택 사항). 복사/붙여넣기/모두 선택 컨텍스트 메뉴;
- [SSH 세션과 SFTP](docs/ssh.md): 연결 프로필과 사이드바 빠른 연결, OS 자격 증명 저장소, 비밀번호/MFA/키/에이전트 인증, 점프 호스트, 호스트 키 검증, 재개 가능한 파일/디렉터리 전송. 듀얼 패인 SFTP 브라우저와 활성 원격 탭을 따라가는 파일 탐색기;
- Rope 편집 버퍼, 가상화 뷰포트, tree-sitter 증분 하이라이팅, 검색, 실행 취소/다시 실행, 인라인 완성 고스트 텍스트, 10종 에디터 테마, Vim 상호작용 레이어;
- 파일 인덱싱(얕은 인덱싱과 디바운스 감시), gitignore 인식, 퍼지 검색, 스트리밍 백그라운드 grep, 키보드 트리 탐색과 완전한 컨텍스트 메뉴. 탭별 프로젝트 폴더와 `cd` 주입, 사이드바 추종;
- Git 전용 diff / history / commit-file 탭, 파일 및 hunk 단위 stage/unstage, 확인 거부(discard), commit, branch, fetch/pull/push, 커밋 그래프. 상태 갱신은 UI 스레드에서 분리;
- 웹 미리보기: localhost URL 감지 + URL 검증 + 시스템 브라우저로 열기(WebView를 내장하지 않음 — [ADR 0002](docs/adr/0002-remove-embedded-webview.md) 참고). 공유 편집 버퍼와 동기화되는 네이티브 Markdown 미리보기;
- 12종 앱 테마와 사용자 지정 테마 모델, GPU 렌더링 창 배경 블러;
- OpenAI, Anthropic, Gemini, Groq, xAI, Cerebras, OpenRouter, DeepSeek, Mistral, OpenAI 호환, LM Studio, MLX, Ollama의 실제 HTTP/SSE 어댑터;
- OS 키체인, 세션/프로젝트 메모리, 파일/이미지/클립보드/`@path` 첨부, 스니펫/TODO, Auto / Plan / Yolo 3단계 제출 모드;
- Composer 명령 영역: `/` 슬래시 명령 팔레트(새 세션, Auto/Plan/Yolo, 에이전트 전환, 중지, 첨부, 도킹 및 인스펙터 패널)는 키맵을 재사용하고 동작을 분기하지 않습니다. `#handle` 스니펫 완성은 `/snippet`과 `/snippets` 위에, `/todos` 패널은 `todo_read` / `todo_write`로 에이전트와 공유하는 앱 내 TODO 목록 위에 구축;
- 터미널 선택 영역을 명령 출처와 함께 Composer에 첨부: OSC 133 C/D 경계를 각 명령의 정확한 바이트에 정렬하여, 첨부 컨텍스트에 명령 id, cwd, 명령줄, 종료 코드가 담기며, 컨텍스트 메뉴 항목으로 최근 실패한 명령의 출력을 바로 첨부 가능;
- 직렬화 가능한 Task/Turn, 예산과 취소, 실제 도구 실행, 승인 카드, 명령 타임아웃, 영속 셸, 백그라운드 프로세스, 그리고 `write_file → AI diff → hunk별 결정 → 원자적 쓰기` 안전 폐쇄 루프;
- 내장 에이전트와 Codex app-server를 Composer에서 전환 가능. 독립 실행형 `termior-agent-host`가 구조화된 백엔드 계약, Codex/ACP 어댑터, 능력 협상, 실행 환경 기술을 제공;
- `AGENTS.md` 계층 규칙, Context Inspector, Agent Skills, stdio와 Streamable HTTP의 MCP, 제한된 Hooks, 검토 가능한 Memory, 사용자 지정 에이전트, 하위 작업 오케스트레이션;
- journal/snapshot 복구, Recovery Center, 콘텐츠 주소 체크포인트, direct/worktree/sandboxed 실행 환경, 작업 트리와 자동화 큐 진입점;
- 내장/터미널 에이전트의 통합 알림 라우팅: 대상이 보일 때는 억제, 숨겨졌을 때는 테마 toast, 창이 포커스를 잃으면 시스템 알림, 헤더 벨에 현재 상태 표시;
- Claude Code OSC hooks의 안전하고 멱등적인 설치와 제거.

이는 여전히 단계적 마일스톤이며, 사양 전체의 최종 승인은 아닙니다. 기본 데스크톱 범위는 [docs/desktop-milestone.md](docs/desktop-milestone.md), Agent 스테이지 A–E의 구현 증거와 플랫폼 샌드박싱, 원격 MCP OAuth, 실제 ACP 클라이언트, 백그라운드 자동화 실행 경계는 [docs/ai-agent-implementation-status.md](docs/ai-agent-implementation-status.md)에 문서화되어 있습니다.

최신 릴리스 태그는 `v0.2.1`이며, 워크스페이스 버전은 [Cargo.toml](Cargo.toml)에 정의됩니다.

### 워크스페이스

```text
crates/
  termior                  GPUI 진입점, 앱 부트스트랩, 데스크톱 배선
  termior-ui               GPUI 뷰 레이어, 탭/사이드바/워크스페이스 영속 상태
  termior-ui-kit           공유 컨트롤, 패인 레이아웃, 검색 모델
  termior-i18n             컴파일 시간 내장 번역 테이블과 로케일 폴백
  termior-terminal         PTY 세션, 프로세스 수명 주기, 바이트 브리지
  termior-ssh              OpenSSH 연결 프로필, 인증 옵션, SFTP 전송 명령
  termior-terminal-core    OSC, 셸 통합, 검색
  termior-editor           Rope, tree-sitter, Vim, 완성 상태
  termior-explorer         파일 인덱스, 트리, 검색, 감시자
  termior-explorer-core    순수 로직 퍼지 매칭과 glob
  termior-vcs              Git 상태, diff, 히스토리, 원격 조작
  termior-preview          localhost 감지와 미리보기 상태
  termior-ai               프로바이더, 작업 런타임, 도구, 컨텍스트, 오케스트레이션
  termior-agent-host       내장/외부 에이전트 백엔드 계약, Codex, ACP, MCP
  termior-diff             hunk diff와 수용 집합 적용
  termior-security         인가, deny-list, SSRF, 도구 게이팅
  termior-store            원자적 JSON, 마이그레이션, 설정, 작업 journal, 체크포인트
  termior-theme            중앙 시맨틱 팔레트와 테마 라이브러리
  termior-hooks            Claude Code hooks
  termior-platform         시스템 알림, 외부 URL, 자동 업데이트 플랫폼 경계
  termior-bench            콜드 스타트, 메모리, 프레임 레이트, PTY 처리량 게이트
```

### 빌드와 검증

stable Rust, 플랫폼 네이티브 빌드 도구 체인, GPUI의 시스템 의존성이 필요합니다.

#### Windows: 먼저 MSVC 환경 활성화

이 프로젝트는 `libgit2-sys`, `libz-sys` 등의 C 코드를 컴파일하므로 `cl.exe`가 C 표준 라이브러리 헤더(예: `time.h`)를 찾을 수 있어야 합니다. Git Bash나 일반 터미널에서 `cargo build`를 직접 실행하면 `INCLUDE`, `LIB`, `VCINSTALLDIR`가 비어 있어 `cl.exe`가 `fatal error C1083: Cannot open include file: 'time.h'`로 실패합니다(cc-rs 로그에는 `command did not execute successfully (status code: exit code: 2)`로 표시). 빌드 전에 MSVC 환경을 활성화하세요.

다음 중 하나로 `INCLUDE` / `LIB` / `VCINSTALLDIR`가 비어 있지 않게 만듭니다:

- **x64 Native Tools Command Prompt for VS 2022**(가장 간단): 시작 메뉴에서 열면 변수가 이미 설정되어 있어 아래 명령을 그대로 실행하면 됩니다.
- **PowerShell / cmd**: 먼저 `vcvars64.bat`를 실행한 뒤 빌드:
  ```bat
  "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
  cargo build
  ```
- **Git Bash**: cmd로 감싸 `.bat`이 올바른 인터프리터에서 실행되게 합니다:
  ```bash
  cmd //c '"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" && cargo build'
  ```

환경 확인:

```bat
echo %INCLUDE%   &  rem MSVC 헤더와 Windows SDK의 ucrt/shared/um 디렉터리를 가리켜야 함
echo %LIB%       &  rem 대응하는 라이브러리 디렉터리를 가리켜야 함
echo %VCINSTALLDIR%
```

#### 검증

아래 명령은 CI의 core/desktop 분할과 일치합니다:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --exclude termior -- -D warnings
cargo test --workspace --exclude termior --all-targets
cargo test -p termior-ui-kit --no-default-features
cargo check -p termior
cargo clippy -p termior --all-targets -- -D warnings
cargo test -p termior --all-targets
cargo build -p termior --release
cargo llvm-cov --workspace --exclude termior --tests --fail-under-lines 80
cargo bench -p termior-editor --bench large_file -- --quick
cargo bench -p termior-bench --bench pty_throughput -- --quick
```

CI는 같은 분할을 구현합니다: `ci.yml`은 아이콘 생성 검사, 3 플랫폼 core 잡, desktop 잡(check, clippy, 테스트, 릴리스 빌드, 바이너리 크기 게이트, Windows 스모크 프레임, 분할 창 스모크, NFR 프로세스 게이트), 줄 커버리지 80%의 coverage 잡, PTY 처리량 게이트가 있는 benchmark 잡으로 구성됩니다. `audit.yml`은 push마다와 매주 `cargo-deny`를 실행해 라이선스와 CVE 드리프트를 잡아냅니다.

#### 앱 아이콘

앱 아이콘의 유일한 소스는 `assets/termior-logo.svg`입니다. SVG를 편집한 뒤 Node.js 22+로 Windows ICO, macOS ICNS, 범용 PNG, Linux hicolor 에셋을 재생성하세요:

```bash
npm ci
npm run icons
npm run icons:check
```

#### 데스크톱 앱 실행

```bash
cargo run -p termior
```

#### 릴리스

릴리스 태그(`v<workspace-version>`)는 3 플랫폼 릴리스 빌드를 트리거하여 SHA-256 체크섬이 딸린 포터블 ZIP/TAR 아카이브와 각 플랫폼 설치 관리자(Windows는 Inno Setup `*-setup.exe`, macOS는 드래그 설치 `.dmg`, Linux는 `cargo-deb`가 만든 `.deb`)를 생성합니다. 파이프라인은 Spec NFR-05에 따라 단일 바이너리를 60 MiB, 아카이브/설치 관리자를 100 MiB로 제한합니다. 로컬에서는 `scripts/check-release-binary.ps1`과 `scripts/package-release.ps1`로 동일한 검사와 패키징을 재현할 수 있습니다(Windows 설치 관리자에는 Inno Setup 6 필요).

릴리스 크기는 주로 GPUI 네이티브 렌더링 스택, 터미널/VTE 레이어, 언어별 tree-sitter 그래머, TLS·키체인·프로바이더 클라이언트에서 나옵니다. 웹 미리보기는 WebView를 내장하지 않고 시스템 브라우저에 위임하므로(ADR 0002) 의존성 면이 플랫폼 간 동일합니다. PNG 디코딩과 tree-sitter 언어는 명시적 최소 feature를 사용하며, 새 UI·미리보기·그래머 기능을 추가할 때는 CI의 의존성 트리와 크기 보고서로 평가해야 합니다.

프로바이더의 엔드포인트, 모델, 활성화 상태는 `Termior-settings.json`에 저장됩니다. API 키는 설정 창을 통해서만 OS 키체인에 기록되며, 설정이나 세션 파일에 직렬화되지 않습니다.

#### 자동 업데이트

**설정 → About**에서 자동 업데이트 토글(기본 켜짐), 수동 확인, 릴리스 노트를 제공합니다. 앱은 시작 15초 후와 이후 6시간마다 GitHub 안정 릴리스를 확인하고, OS와 CPU 아키텍처에 맞는 설치 관리자를 내려받아 SHA-256을 검증합니다. 내려받기가 완료되면 제목 표시줄에 **Update available**이 표시되고, About을 열어 **Install update…**를 선택하면 시스템 설치 관리자가 실행됩니다. 설치를 마치려면 앱을 종료하기 전에 터미널 작업을 끝내거나 중지하세요. 업데이터가 스스로 앱이나 터미널을 닫지 않습니다. 자동 업데이트를 꺼도 수동 확인은 계속 동작합니다.

Windows는 Inno Setup, macOS는 DMG를 열어 앱을 드래그로 교체, Linux는 DEB 설치 관리자를 사용합니다. 포터블 빌드, DEB를 지원하지 않는 배포판, 해당 아키텍처 패키지가 없는 경우 릴리스 다운로드 페이지에서 수동으로 업데이트하세요. 네트워크나 체크섬 실패 시 오류를 표시하고 현재 버전을 유지합니다. 시스템 설치 관리자에 전달된 파일은 시스템 임시 디렉터리에 남으며 설치 후 삭제할 수 있습니다. 체크섬은 파일 무결성 검증에만 사용됩니다. 현재 Windows와 macOS 설치 관리자는 서명되지 않았습니다.

### 프라이버시와 보안

- 텔레메트리 없음, 계정 없음, 자동 업로드 없음;
- 로컬 프로바이더는 완전 오프라인으로 사용 가능;
- `.env`, `.ssh`, credentials 같은 민감한 경로는 canonicalize 후 양방향으로 거부;
- 클라우드 프로바이더의 외부 트래픽은 항상 URL과 확인 후 IP의 SSRF 검사를 통과;
- 모델이 파일을 디스크에 직접 쓰지 않으며, 도구 승인과 hunk 검토를 반드시 거칩니다.

### 국제화(i18n)

인터페이스는 7개 언어를 내장합니다: English, 简体中文, 繁體中文, 日本語, 한국어, Español, Deutsch. **설정 → General → Language**로 즉시 전환되며(메인 창은 디바운스 저장을 따름), 기본값인 '시스템 따르기'는 OS 로케일을 추적합니다. 번역은 `crates/termior-i18n/locales/`에 컴파일 시간 내장된 플랫 JSON 테이블이며 `en.json`이 유일한 원본입니다. 모든 로케일은 동일한 키 집합을 유지해야 하며, CI 테스트가 키 일치와 빈 값 없음을 강제합니다. 설계는 [ADR 0007](docs/adr/0007-i18n-embedded-json-tables.md)을 참고하세요.

### 라이선스

프로젝트 자체는 [MIT License](LICENSE) 하에 배포됩니다. 제3자 의존성 라이선스는 `cargo deny --exclude termior check`로 감사합니다. [LICENSE-APACHE](LICENSE-APACHE)는 vendored `gpui_windows`의 업스트림 Apache-2.0 조항을 보존하기 위해서만 존재하며, GPUI 업스트림 의존성 트리는 별도로 검증됩니다.
