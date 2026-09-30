<p align="center">
  <img src="assets/termior-logo.svg" width="128" height="128" alt="Termior logo">
</p>

# Termior

[English](README.md#english) · [简体中文](README.md#简体中文) · 繁體中文 · [日本語](README.ja.md) · [한국어](README.ko.md) · [Español](README.es.md) · [Deutsch](README.de.md)

---

## 繁體中文

> 開源、跨平台、終端機優先的 AI 原生開發工作台（ADE）。BYOK、本地優先、無帳號、無遙測。

Termior 在單一原生視窗中組合真 PTY 終端機、輕量程式碼編輯器、檔案瀏覽器、Git、Web 預覽，以及具有工具呼叫、審批、變更審閱與復原能力的內建/外部 Agent。產品與工程規格見 [docs/termior-spec.md](docs/termior-spec.md)。

### 核心價值

1. **原生低延遲**：核心 UI 無 JS 執行環境與 IPC 序列化邊界，終端機與編輯器直接使用原生 GPU 渲染，常態目標 60 fps，高更新率螢幕目標 120 fps。
2. **終端機與 AI 深度協同**：真 PTY、即時 cwd/緩衝上下文、工具呼叫、審批與 hunk 級 diff 審閱集中在同一工作區。
3. **本地優先與使用者掌控**：BYOK、金鑰只進系統鑰匙圈、無遙測、無帳號，並支援 LM Studio、MLX、Ollama 等本地推論服務。
4. **明確的安全邊界**：內建 Agent 的觸碰磁碟、觸碰 shell、觸碰金鑰與觸碰網路操作，一律經過 workspace 授權、審批、deny-list 與 SSRF 策略閘門；外部 Agent 則明確顯示其後端、執行環境、實際隔離能力與未知項，不把後端自有操作偽裝成已受內建閘門保護。

### 目前桌面里程碑

目前分支已從純邏輯原型推進到可編譯的 GPUI 桌面工作台，主要包含：

- 持久化工作區、8 類 tab、分割窗格、Explorer/Source Control/History 側欄、狀態列、獨立設定視窗；
- portable-pty + alacritty_terminal 真終端機，shell integration（bash/zsh/fish/PowerShell）、OSC 7/133/777、IME、搜尋、回捲瀏覽、URL/localhost 偵測與 Windows Job Object；設定頁可選預設 Shell（系統預設 / 偵測到的 shell，含 Git Bash 與 WSL 發行版 / 手動路徑），並可開啟「新建終端機時選擇 Shell」；提供複製/貼上/全選的內容選單；
- [SSH 連線與 SFTP](docs/ssh.md)：連線設定與側欄快速連線、系統憑證庫、密碼/MFA/金鑰/agent 認證、跳板機、主機指紋校驗、檔案/目錄上傳下載與續傳，並提供雙欄 SFTP 瀏覽器與跟隨活動遠端 tab 的 File Explorer；
- Rope 編輯緩衝、虛擬可視區、tree-sitter 增量語法高亮、搜尋、復原/重做、行內補全 ghost text、10 套編輯器主題與 Vim 互動層；
- 檔案索引（淺層索引與防抖監聽）、gitignore、模糊尋找、背景串流 grep、鍵盤樹狀導覽與完整內容選單；按 tab 記憶專案資料夾並注入 `cd`，側欄跟隨該錨點；
- Git 專用 diff/history/commit-file 分頁、檔案/hunk stage/unstage、確認後 discard、commit、branch、fetch/pull/push 與 commit graph，狀態刷新已移出 UI 執行緒；
- Web 預覽：localhost URL 偵測 + URL 校驗 + 交系統瀏覽器開啟（不內嵌 WebView，見 [ADR 0002](docs/adr/0002-remove-embedded-webview.md)）；Markdown 原生渲染預覽與共享編輯緩衝即時同步；
- 12 套應用主題、自訂主題模型與 GPU 渲染的視窗背景模糊；
- OpenAI、Anthropic、Gemini、Groq、xAI、Cerebras、OpenRouter、DeepSeek、Mistral、OpenAI-compatible、LM Studio、MLX、Ollama 的真實 HTTP/SSE 介面卡；
- OS 鑰匙圈、工作階段/專案記憶、檔案/圖片/剪貼簿/`@path` 附加內容、snippets/TODO，以及 Auto/Plan/Yolo 三段提交模式；
- Composer 命令面：`/` 斜線命令面板（新建工作階段、Auto/Plan/Yolo、切換 Agent、停止、附加、停駐與 inspector 面板）復用按鍵表執行、不另開一套行為；`#handle` 片段補全配合 `/snippet`、`/snippets`；`/todos` 面板與 Agent 透過 `todo_read` / `todo_write` 共享同一份應用內 TODO 清單；
- 終端機選區攜帶命令溯源附加到 Composer：OSC 133 C/D 邊界與每條命令的確切位元組對齊，附加上下文因此帶有命令 id、cwd、命令列與結束代碼，另有內容選單項目可直接附加最近一條失敗命令的輸出；
- 可序列化 Task/Turn、預算與取消、真實工具執行、審批卡片、命令逾時、持久 shell、背景處理程序，以及 `write_file → AI diff → 逐 hunk 決策 → 原子寫入` 安全閉環；
- 內建 Agent 與 Codex app-server 可在 Composer 中切換；獨立 `termior-agent-host` 提供結構化後端契約、Codex/ACP 介面卡、能力協商與執行環境描述；
- `AGENTS.md` 分層規則、Context Inspector、Agent Skills、MCP stdio/Streamable HTTP、受限 Hooks、可審閱 Memory、自訂 Agent 與子任務編排；
- journal/snapshot 復原、Recovery Center、內容定址 checkpoint、direct/worktree/sandboxed 執行環境，以及任務樹與自動化佇列入口；
- 內建/終端機 Agent 統一通知路由：可見時抑制、隱藏時主題 toast、視窗失焦時系統通知，並在 header 鈴鐺列出目前狀態；
- Claude Code OSC hooks 的安全、冪等安裝與解除安裝。

這仍是階段性里程碑，不等於整份 spec 已最終驗收。基礎桌面範圍見 [docs/desktop-milestone.md](docs/desktop-milestone.md)；Agent A–E 的實作證據與平台 sandbox、遠端 MCP OAuth、真實 ACP 用戶端和自動化背景執行等邊界見 [docs/ai-agent-implementation-status.md](docs/ai-agent-implementation-status.md)。

最新發布 tag 為 `v0.2.1`，工作區版本號定義在 [Cargo.toml](Cargo.toml)。

### Workspace

```text
crates/
  termior                  GPUI 進入點、應用啟動與桌面接線
  termior-ui               GPUI 視圖層與 tab/sidebar/workspace 持久狀態
  termior-ui-kit           通用控制項、pane 佈局與共享搜尋模型
  termior-i18n             編譯期內嵌翻譯表與語言回退
  termior-terminal         PTY 工作階段、處理程序生命週期與位元組橋接
  termior-ssh              OpenSSH 連線設定、認證選項與 SFTP 傳輸命令
  termior-terminal-core    OSC、shell integration、搜尋
  termior-editor           Rope、tree-sitter、Vim、補全狀態
  termior-explorer         檔案索引、樹、搜尋、watcher
  termior-explorer-core    模糊匹配與 glob 純邏輯
  termior-vcs              Git 狀態、diff、歷史與遠端操作
  termior-preview          localhost 偵測與預覽狀態
  termior-ai               Provider、任務執行時、工具、上下文與編排
  termior-agent-host       內建/外部 Agent 後端契約、Codex、ACP 與 MCP
  termior-diff             hunk diff 與接受集應用
  termior-security         授權、deny-list、SSRF、工具閘門
  termior-store            原子 JSON、遷移、設定、任務 journal 與 checkpoint
  termior-theme            中央語義色板與主題庫
  termior-hooks            Claude Code hooks
  termior-platform         系統通知、外部 URL 與自動更新平台邊界
  termior-bench            冷啟動、記憶體、幀率與 PTY 吞吐門禁
```

### 建置與驗證

需要 stable Rust、平台原生編譯工具，以及 GPUI 對應的系統相依套件。

#### Windows：先載入 MSVC 開發環境

專案編譯 `libgit2-sys`、`libz-sys` 等 C 程式碼，依賴 `cl.exe` 能找到 C 標準庫標頭檔（如 `time.h`）。
在 Git Bash / 一般終端機中直接執行 `cargo build` 時，`INCLUDE`、`LIB`、`VCINSTALLDIR` 等變數為空，
`cl.exe` 會報 `fatal error C1083: Cannot open include file: 'time.h'`（cc-rs 日誌中呈現為
`command did not execute successfully (status code: exit code: 2)`）。因此建置前需先啟用 MSVC 環境。

任選一種方式，使 `INCLUDE` / `LIB` / `VCINSTALLDIR` 不再為空：

- **x64 Native Tools Command Prompt for VS 2022**（最簡單）：從開始功能表開啟，其中已內建上述變數，直接執行下方命令即可。
- **PowerShell / cmd**：先執行 `vcvars64.bat` 再編譯：
  ```bat
  "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
  cargo build
  ```
- **Git Bash**：用 cmd 包一層，確保 `.bat` 在正確的解譯器中執行：
  ```bash
  cmd //c '"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" && cargo build'
  ```

驗證環境是否就緒：

```bat
echo %INCLUDE%   &  rem 應指向 MSVC 標頭檔與 Windows SDK 的 ucrt/shared/um 目錄
echo %LIB%       &  rem 應指向對應的函式庫目錄
echo %VCINSTALLDIR%
```

#### 驗證

以下主要命令與 CI 的 core/desktop 分工保持一致：

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

CI 按同一劃分執行：`ci.yml` 包含圖示產生校驗、三平台 core job、desktop job（check、clippy、測試、release 建置、二進位體積門禁、Windows 渲染煙測、分割窗格煙測、NFR 處理程序門禁）、80% 行覆蓋率的 coverage job，以及帶 PTY 吞吐門禁的 benchmark job；`audit.yml` 在每次 push 與每週定時執行 `cargo-deny`，捕捉授權與 CVE 漂移。

#### 應用圖示

應用圖示以 `assets/termior-logo.svg` 為唯一原始檔。修改 SVG 後，使用 Node.js 22+
重新產生 Windows ICO、macOS ICNS、通用 PNG 與 Linux hicolor 資源：

```bash
npm ci
npm run icons
npm run icons:check
```

#### 執行桌面應用

```bash
cargo run -p termior
```

#### 發布

發布 tag（`v<workspace-version>`）會觸發三平台 release 建置，產出 portable ZIP/TAR 與 SHA-256 校驗檔案，並附帶各平台安裝包：Windows 為 Inno Setup 安裝器 `*-setup.exe`，macOS 為拖曳安裝的 `.dmg`，Linux 為 `cargo-deb` 產生的 `.deb`。流水線按 Spec NFR-05 將單一二進位限制為 60 MiB，並將壓縮包/安裝包限制為 100 MiB；本機可用 `scripts/check-release-binary.ps1` 和 `scripts/package-release.ps1` 重現檢查與打包（Windows 安裝器需本機裝有 Inno Setup 6）。

Release 體積主要來自 GPUI 原生渲染堆疊、終端機/VTE、按語言引入的 tree-sitter grammar，以及 TLS、鑰匙圈與 Provider 用戶端。Web 預覽統一交系統瀏覽器開啟、不內嵌 WebView（ADR 0002），三平台相依面一致；PNG 解碼和 tree-sitter 語言均採用顯式最小 feature，新增 UI、預覽或語法能力時須結合 CI 的相依樹和體積報告評估增量。

Provider 的 endpoint、模型與啟用狀態保存在 `Termior-settings.json`；API key 只透過設定視窗寫入 OS 鑰匙圈，絕不會序列化進設定或工作階段檔案。

#### 自動更新

設定 → **About** 提供自動更新開關（預設開啟）、手動檢查與發行說明入口。應用程式在啟動 15 秒後、此後每 6 小時檢查 GitHub 穩定版；發現新版本時按系統與 CPU 架構下載安裝包，並核驗 SHA-256。下載完成後標題列顯示 **Update available**，點擊進入 About，選擇 **Install update…** 開啟系統安裝程式。請先結束終端機任務，再關閉應用完成安裝；更新器不會自動關閉應用或終端機。關閉自動更新後仍可手動檢查。

Windows 使用 Inno Setup，macOS 開啟 DMG 後拖曳替換應用，Linux 使用 DEB 安裝器。可攜版、不支援 DEB 的發行版、缺少對應架構安裝包時，請使用發行下載頁手動更新。網路或校驗失敗會顯示錯誤並保留目前版本；交給系統安裝程式的檔案保留在系統暫存目錄，安裝完成後可刪除。校驗和只用於檢查檔案完整性；目前 Windows 與 macOS 安裝包尚未簽署。

### 隱私與安全

- 無遙測、無帳號、無自動上傳
- 本地 Provider 可離線使用
- `.env`、`.ssh`、credentials 等敏感路徑在 canonicalize 後雙向拒絕
- 雲端 Provider 對外連線一律經過 URL 與解析後 IP 的 SSRF 檢查
- 寫檔案不會由模型直接落碟，必須經過工具審批與 hunk 審閱

### 國際化（i18n）

介面內建七種語言：English、简体中文、繁體中文、日本語、한국어、Español、Deutsch。**設定 → General → Language** 即選即切（主視窗隨防抖儲存同步）；預設「跟隨系統」使用作業系統語言。翻譯表是編譯期內嵌的 flat JSON（`crates/termior-i18n/locales/`），`en.json` 為唯一事實來源，各語言鍵集必須完全一致，CI 測試強制校驗鍵集合一致性與非空值。設計見 [ADR 0007](docs/adr/0007-i18n-embedded-json-tables.md)。

### 授權

專案本身採用 [MIT License](LICENSE)。第三方相依授權由 `cargo deny --exclude termior check` 稽核；[LICENSE-APACHE](LICENSE-APACHE) 僅為 vendored `gpui_windows` 保留其上游 Apache-2.0 條款，GPUI 上游相依樹另行核驗。
