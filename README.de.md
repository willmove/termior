<p align="center">
  <img src="assets/termior-logo.svg" width="128" height="128" alt="Termior logo">
</p>

# Termior

[English](README.md#english) · [简体中文](README.md#简体中文) · [繁體中文](README.zh-TW.md) · [日本語](README.ja.md) · [한국어](README.ko.md) · [Español](README.es.md) · Deutsch

---

## Deutsch

> Eine Open-Source-, plattformübergreifende, terminal-first KI-native Entwicklungsumgebung (ADE). BYOK, lokal zuerst, ohne Konto, ohne Telemetrie.

Termior bündelt ein echtes PTY-Terminal, einen schlanken Code-Editor, einen Datei-Explorer, Git, eine Web-Vorschau sowie einen eingebauten/externen Agenten mit Werkzeugaufrufen, Freigaben, Änderungsprüfung und Wiederherstellung in einem einzigen nativen Fenster. Die maßgebliche Produkt- und Engineering-Spezifikation liegt unter [docs/termior-spec.md](docs/termior-spec.md) (englisch).

### Kernwerte

1. **Natürliche niedrige Latenz** — Die Kern-UI hat keine JS-Laufzeit und keine IPC-Serialisierungsgrenze. Terminal und Editor rendern direkt auf der GPU, mit dem Ziel von 60 fps im Dauerbetrieb und 120 fps auf Displays mit hoher Bildwiederholrate.
2. **Tiefe Terminal–KI-Zusammenarbeit** — Ein echtes PTY, live cwd/Puffer-Kontext, Werkzeugaufrufe, Freigaben und hunk-genaue Diff-Prüfung leben im selben Workspace.
3. **Lokal zuerst und Nutzerkontrolle** — BYOK, Schlüssel landen ausschließlich im OS-Schlüsselbund, keine Telemetrie, kein Konto, plus Unterstützung lokaler Inferenzserver wie LM Studio, MLX und Ollama.
4. **Explizite Sicherheitsgrenzen** — Jede Datenträger-, Shell-, Secret- und Netzwerkoperation des eingebauten Agenten durchläuft Workspace-Autorisierung, Freigaben, eine Deny-List und SSRF-Richtlinien-Gates. Externe Agenten müssen ihr Backend, ihre Ausführungsumgebung, ihre tatsächlichen Isolationsfähigkeiten und offene Unknowns anzeigen, statt eigene Operationen als durch die eingebauten Gates abgedeckt auszugeben.

### Aktueller Desktop-Meilenstein

Der Workspace hat sich von einem reinen Logik-Prototyp zu einer baubaren GPUI-Desktop-Workbench entwickelt. Derzeit enthalten:

- Persistente Workspaces, 8 Tab-Arten, Splits, Explorer-/Source-Control-/History-Seitenleisten, eine Statusleiste und ein separates Einstellungsfenster;
- Ein echtes Terminal auf Basis von portable-pty und alacritty_terminal: Shell-Integration (bash/zsh/fish/PowerShell), OSC 7/133/777, IME, Suche, Scrollback, URL- und localhost-Erkennung sowie Windows Job Objects; Auswahl der Standard-Shell in den Einstellungen (Systemstandard / erkannte Shells inkl. Git Bash und WSL-Distributionen / manueller Pfad) plus ein optionales Shell-Menü beim Anlegen eines Terminals; ein Kontextmenü für Kopieren/Einfügen/Alles auswählen;
- [SSH-Sitzungen und SFTP](docs/ssh.md) (englisch): Verbindungsprofile mit Schnellzugriff aus der Seitenleiste, OS-Credential-Store, Passwort-/MFA-/Schlüssel-/Agent-Authentifizierung, Sprunghosts, Host-Key-Verifikation sowie Datei-/Verzeichnisübertragung mit Fortsetzung; ein zweispaltiger SFTP-Browser und ein Datei-Explorer, der dem aktiven Remote-Tab folgt;
- Rope-Editierpuffer, virtualisierter Viewport, inkrementelles tree-sitter-Highlighting, Suche, Undo/Redo, Inline-Completion-Ghost-Text, 10 Editor-Themes und eine Vim-Interaktionsschicht;
- Datei-Indizierung mit flacher Indizierung und entprellter Überwachung, gitignore-Bewusstsein, Fuzzy-Suche, gestreamtes Background-grep, Tastatur-Navigation im Baum und vollständige Kontextmenüs; projektabhängige Ordner pro Tab mit `cd`-Injektion und folgender Seitenleiste;
- Dedizierte Git-Diff-/History-/Commit-Datei-Tabs, Staging/Unstaging auf Datei- und Hunk-Ebene, bestätigtes Verwerfen, Commit, Branch, Fetch/Pull/Push und ein Commit-Graph, mit Statusaktualisierung außerhalb des UI-Threads;
- Web-Vorschau: localhost-URL-Erkennung, URL-Validierung und Übergabe an den Systembrowser (kein eingebettetes WebView — siehe [ADR 0002](docs/adr/0002-remove-embedded-webview.md), englisch); eine native Markdown-Vorschau, synchron mit dem geteilten Editierpuffer;
- 12 App-Themes mit eigenem Theme-Modell sowie ein GPU-gerenderter, unscharfer Fensterhintergrund;
- Echte HTTP/SSE-Adapter für OpenAI, Anthropic, Gemini, Groq, xAI, Cerebras, OpenRouter, DeepSeek, Mistral, OpenAI-kompatibel, LM Studio, MLX und Ollama;
- OS-Schlüsselbund, Sitzungs-/Projekt-Gedächtnis, Datei-/Bild-/Zwischenablage-/`@path`-Anhänge, Snippets/TODO sowie die Sendemodi Auto / Plan / Yolo;
- Eine Composer-Befehlsfläche: eine `/`-Slash-Befehlspalette (neue Sitzung, Auto/Plan/Yolo, Agent-Wechsel, Stopp, Anhang, Dock- und Inspector-Panels), die das Tastaturmapping wiederverwendet, statt Verhalten zu verzweigen; `#handle`-Snippet-Vervollständigung auf Basis von `/snippet` und `/snippets`; und ein `/todos`-Panel über einer app-lokalen TODO-Liste, die per `todo_read` / `todo_write` mit dem Agenten geteilt wird;
- Terminal-Auswahlen lassen sich mit Befehlsprovenienz an den Composer anhängen: Die OSC-133-C/D-Grenzen werden auf die exakten Bytes jedes Befehls ausgerichtet, sodass ein Anhang die Befehls-ID, das cwd, die Befehlszeile und den Exit-Code trägt; ein Kontextmenüeintrag hängt die Ausgabe des letzten fehlgeschlagenen Befehls an;
- Serialisierbare Task/Turn, Budgets und Abbruch, echte Werkzeugausführung, Freigabekarten, Befehls-Timeouts, persistente Shells, Hintergrundprozesse sowie die Sicherheitsschleife `write_file → KI-Diff → Entscheidung pro Hunk → atomares Schreiben`;
- Ein eingebauter Agent und der Codex app-server, im Composer umschaltbar; der eigenständige `termior-agent-host` liefert den strukturierten Backend-Vertrag, Codex-/ACP-Adapter, Fähigkeitsaushandlung und die Beschreibung der Ausführungsumgebung;
- Layered `AGENTS.md`-Regeln, ein Context Inspector, Agent Skills, MCP über stdio und Streamable HTTP, begrenzte Hooks, überprüfbares Memory, eigene Agenten und Subtask-Orchestrierung;
- journal/snapshot-Wiederherstellung, ein Recovery Center, inhaltsadressierte Checkpoints, direct/worktree/sandboxed-Ausführungsumgebungen sowie Einstiegspunkte für den Aufgabenbaum und die Automatisierungs-Warteschlange;
- Vereinheitlichtes Notification-Routing für eingebaute und Terminal-Agenten: unterdrückt, solange das Ziel sichtbar ist, thematisches Toast bei Verdeckung, Systembenachrichtigung bei unfokussiertem Fenster, mit Statusglocke im Header;
- Sichere, idempotente Installation und Entfernung der Claude-Code-OSC-Hooks.

Dies bleibt ein gestufter Meilenstein, nicht die Endabnahme der gesamten Spezifikation. Der Basis-Umfang des Desktops ist in [docs/desktop-milestone.md](docs/desktop-milestone.md) (englisch) dokumentiert; Implementierungsnachweise für die Agent-Stufen A–E sowie Plattform-Sandboxing, Remote-MCP-OAuth, ein echter ACP-Client und die Grenzen der Hintergrundautomatisierung stehen in [docs/ai-agent-implementation-status.md](docs/ai-agent-implementation-status.md) (englisch).

Das neueste getaggte Release ist `v0.2.1`; die Workspace-Version steht in [Cargo.toml](Cargo.toml).

### Workspace

```text
crates/
  termior                  GPUI-Einstiegspunkt, App-Bootstrap und Desktop-Verdrahtung
  termior-ui               GPUI-View-Schicht, persistenter Tab-/Sidebar-/Workspace-Zustand
  termior-ui-kit           geteilte Controls, Pane-Layout und Suchmodelle
  termior-i18n             zur Compile-Zeit eingebettete Übersetzungstabellen und Locale-Fallback
  termior-terminal         PTY-Sitzungen, Prozesslebenszyklus und Byte-Brücke
  termior-ssh              OpenSSH-Verbindungsprofile, Auth-Optionen, SFTP-Übertragungsbefehle
  termior-terminal-core    OSC, Shell-Integration, Suche
  termior-editor           Rope, tree-sitter, Vim, Completion-Zustand
  termior-explorer         Dateiindex, Baum, Suche, Watcher
  termior-explorer-core    reine Logik für Fuzzy-Matching und Globs
  termior-vcs              Git-Status, Diff, History und Remote-Operationen
  termior-preview          localhost-Erkennung und Vorschauzustand
  termior-ai               Provider, Task-Runtime, Werkzeuge, Kontext, Orchestrierung
  termior-agent-host       Backend-Vertrag für eingebaute/externe Agenten, Codex, ACP, MCP
  termior-diff             Hunk-Diff und Anwendung der Akzeptanzmengen
  termior-security         Autorisierung, Deny-List, SSRF, Werkzeug-Gating
  termior-store            atomares JSON, Migrationen, Einstellungen, Task-Journal, Checkpoints
  termior-theme            zentrale semantische Palette und Theme-Bibliothek
  termior-hooks            Claude-Code-Hooks
  termior-platform         Systembenachrichtigungen, externe URLs, Plattformgrenze des Auto-Updates
  termior-bench            Kaltstart, Speicher, Framerate und PTY-Durchsatz-Gates
```

### Bauen und Verifizieren

Erforderlich sind stabiles Rust, die plattformeigene Toolchain und die Systemabhängigkeiten von GPUI.

#### Windows: zuerst die MSVC-Umgebung aktivieren

Das Projekt kompiliert C-Code aus `libgit2-sys`, `libz-sys` und anderen; `cl.exe` muss daher die Header der C-Standardbibliothek (z. B. `time.h`) finden können. Ein direktes `cargo build` aus Git Bash oder einem schlichten Terminal lässt `INCLUDE`, `LIB` und `VCINSTALLDIR` ungesetzt, und `cl.exe` scheitert mit `fatal error C1083: Cannot open include file: 'time.h'` (in cc-rs-Logs sichtbar als `command did not execute successfully (status code: exit code: 2)`). Aktiviere vor dem Build die MSVC-Umgebung.

Wähle eines der folgenden Verfahren, damit `INCLUDE` / `LIB` / `VCINSTALLDIR` nicht länger leer sind:

- **x64 Native Tools Command Prompt for VS 2022** (am einfachsten): aus dem Startmenü öffnen — die Variablen sind bereits gesetzt — und die untenstehenden Befehle ausführen.
- **PowerShell / cmd**: zuerst `vcvars64.bat` ausführen, dann bauen:
  ```bat
  "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
  cargo build
  ```
- **Git Bash**: den Aufruf in cmd einwickeln, damit die `.bat` im richtigen Interpreter läuft:
  ```bash
  cmd //c '"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" && cargo build'
  ```

Umgebung prüfen:

```bat
echo %INCLUDE%   &  rem sollte auf die MSVC-Header plus die ucrt/shared/um-Verzeichnisse des Windows SDK zeigen
echo %LIB%       &  rem sollte auf die passenden Bibliotheksverzeichnisse zeigen
echo %VCINSTALLDIR%
```

#### Verifizieren

Diese Befehle entsprechen der Core-/Desktop-Aufteilung aus der CI:

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

Die CI setzt dieselbe Aufteilung um: `ci.yml` enthält eine Icon-Generierungsprüfung, den Core-Job auf allen drei Plattformen, den Desktop-Job (Check, Clippy, Tests, Release-Build, Binärgrößen-Gate, Windows-Smoke-Frames, Split-Pane-Smoke, NFR-Prozess-Gate), einen Coverage-Job mit 80-%-Zeilenschwelle sowie einen Benchmark-Job mit PTY-Durchsatz-Gate. `audit.yml` führt bei jedem Push und wöchentlich `cargo-deny` aus, um Lizenz- und CVE-Drift zu erkennen.

#### App-Icons

`assets/termior-logo.svg` ist die einzige Quelle der App-Icons. Nach dem Bearbeiten der SVG generiere das Windows-ICO, das macOS-ICNS, die generischen PNGs und die Linux-hicolor-Assets mit Node.js 22+ neu:

```bash
npm ci
npm run icons
npm run icons:check
```

#### Die Desktop-App ausführen

```bash
cargo run -p termior
```

#### Release

Ein Release-Tag (`v<workspace-version>`) löst einen dreiplattformigen Release-Build aus, der portable ZIP/TAR-Archive mit SHA-256-Prüfsummen sowie plattformspezifische Installer erzeugt: ein Inno-Setup-`*-setup.exe` unter Windows, ein per Drag-and-Drop zu installierendes `.dmg` unter macOS und ein per `cargo-deb` erzeugtes `.deb` unter Linux. Die Pipeline erzwingt die 60-MiB-Grenze für einzelne Binärdateien und die 100-MiB-Grenze für Archive/Installer aus Spec NFR-05. Lokal lassen sich Prüfung und Paketierung mit `scripts/check-release-binary.ps1` und `scripts/package-release.ps1` reproduzieren (der Windows-Installer erfordert Inno Setup 6 auf dem Rechner).

Die Release-Größe stammt vor allem aus dem nativen GPUI-Rendering-Stack, der Terminal-/VTE-Schicht, den pro Sprache eingebundenen tree-sitter-Grammatiken sowie TLS-, Schlüsselbund- und Provider-Clients. Die Web-Vorschau übergibt an den Systembrowser, statt ein WebView einzubetten (ADR 0002), wodurch die Abhängigkeitsoberfläche plattformidentisch bleibt; PNG-Dekodierung und tree-sitter-Sprachen nutzen explizite Minimal-Features — neue UI-, Vorschau- oder Grammatikfähigkeiten müssen gegen die Abhängigkeitsbaum- und Größenberichte der CI bewertet werden.

Provider-Endpunkte, Modelle und Aktivierung liegen in `Termior-settings.json`; API-Schlüssel werden ausschließlich über das Einstellungsfenster in den OS-Schlüsselbund geschrieben und niemals in Einstellungs- oder Sitzungsdateien serialisiert.

#### Auto-Update

**Einstellungen → About** bietet einen Auto-Update-Schalter (standardmäßig an), eine manuelle Prüfung und Release Notes. Die App prüft 15 Sekunden nach dem Start und danach alle 6 Stunden die stabilen GitHub-Releases, lädt den zu OS und CPU-Architektur passenden Installer herunter und verifiziert dessen SHA-256. Nach Abschluss des Downloads zeigt die Titelleiste **Update available**; About öffnen und **Install update…** wählen, um das System-Installationsprogramm zu starten. Beende oder stoppe Terminal-Aufgaben, bevor du die App schließt, um die Installation abzuschließen — der Updater beendet die App und ihre Terminals nie von selbst. Mit deaktiviertem Auto-Update funktionieren manuelle Prüfungen weiterhin.

Windows nutzt Inno Setup, macOS öffnet ein DMG zum Ziehen-und-Ersetzen der App, Linux verwendet den DEB-Installer. Bei portablen Builds, Distributionen ohne DEB-Unterstützung oder fehlenden Architekturpaketen bitte manuell über die Release-Download-Seite aktualisieren. Netzwerk- oder Prüfsummenfehler zeigen einen Fehler an und behalten die aktuelle Version; an das System-Installationsprogramm übergebene Dateien bleiben im temporären Verzeichnis und können nach der Installation gelöscht werden. Prüfsummen verifizieren nur die Dateiintegrität — die aktuellen Windows- und macOS-Installer sind noch nicht signiert.

### Datenschutz und Sicherheit

- Keine Telemetrie, kein Konto, keine automatischen Uploads;
- Lokale Provider sind vollständig offline nutzbar;
- Sensible Pfade wie `.env`, `.ssh` und Credentials werden nach Kanonisierung in beide Richtungen abgelehnt;
- Der ausgehende Verkehr zu Cloud-Providern durchläuft stets SSRF-Prüfungen von URL und der IP nach der Auflösung;
- Dateischreibvorgänge landen nie direkt vom Modell auf der Festplatte — sie müssen die Werkzeug-Freigabe und die Hunk-Prüfung durchlaufen.

### Internationalisierung (i18n)

Die Oberfläche wird in sieben Sprachen ausgeliefert: English, 简体中文, 繁體中文, 日本語, 한국어, Español und Deutsch. **Einstellungen → General → Language** schaltet sofort um (das Hauptfenster folgt dem entprellten Speichern); der Standard **Dem System folgen** richtet sich nach der Betriebssystem-Sprache. Übersetzungen sind flache JSON-Tabellen, die zur Compile-Zeit in `crates/termior-i18n/locales/` eingebettet werden — `en.json` ist die Quelle der Wahrheit, jedes Locale muss eine identische Schlüsselmenge pflegen, und ein CI-Test erzwingt Parität sowie nicht-leere Werte. Das Design steht in [ADR 0007](docs/adr/0007-i18n-embedded-json-tables.md) (englisch).

### Lizenz

Das Projekt selbst steht unter der [MIT License](LICENSE). Lizenzen von Drittanbieter-Abhängigkeiten werden mit `cargo deny --exclude termior check` auditiert; [LICENSE-APACHE](LICENSE-APACHE) existiert ausschließlich, um die Upstream-Apache-2.0-Bedingungen des vendored `gpui_windows` zu bewahren; der Upstream-Abhängigkeitsbaum von GPUI wird separat verifiziert.
