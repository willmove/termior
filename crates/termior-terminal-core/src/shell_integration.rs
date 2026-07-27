//! Shell integration 注入串生成（FR-TERM-06）。
//!
//! 用户无需改 rc 文件。Termior 在 spawn shell 时通过环境变量 / 命令行参数注入一段
//! 脚本，内部 source 用户真实配置，并发出 OSC 7（cwd）与 OSC 133 A/B/C/D（提示符与
//! 命令边界）。
//!
//! - zsh：经 `ZDOTDIR` 四件套（临时 ZDOTDIR + `.zshenv` + `.zshrc` 包装）。
//! - bash：经 `--rcfile`。
//! - pwsh：经 `-File profile.ps1`（内部 source 用户真实 `$PROFILE`）。
//!
//! 本模块只生成**纯字符串**（注入脚本 + 启动参数），不触盘、不 spawn。

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 支持的 shell 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    Zsh,
    Bash,
    Fish,
    Pwsh,
    PowerShell,
    Cmd,
}

impl ShellKind {
    pub fn name(&self) -> &'static str {
        match self {
            ShellKind::Zsh => "zsh",
            ShellKind::Bash => "bash",
            ShellKind::Fish => "fish",
            ShellKind::Pwsh => "pwsh",
            ShellKind::PowerShell => "powershell",
            ShellKind::Cmd => "cmd",
        }
    }
}

/// 一次 shell integration 注入所需的全部产物。
#[derive(Debug, Clone, PartialEq)]
pub struct ShellIntegrationSnippets {
    pub kind: ShellKind,
    /// spawn 时传给 shell 的参数（如 `--rcfile` / `-File`）。
    pub args: Vec<String>,
    /// 需要写入临时目录的文件名 → 内容（如 ZDOTDIR 下的 `.zshenv`/`.zshrc`）。
    pub files: Vec<(String, String)>,
    /// 需要设置的环境变量（如 `ZDOTDIR`）。
    pub env: Vec<(String, String)>,
}

/// 注入脚本里复用的 OSC 序列生成片段（FR-TERM-06）。
///
/// 这些片段会嵌入到各 shell 的注入脚本中，使 shell 在合适时机输出 OSC 7 / 133。
fn osc7() -> &'static str {
    // printf '\e]7;file://%s%s\a' "$HOSTNAME" "$PWD"
    r#"__termior_osc7() { printf '\033]7;file://%s%s\007' "$TERMior_HOSTNAME" "$PWD"; }"#
}

fn osc133_prompt() -> &'static str {
    // 在 PROMPT_COMMAND / precmd 前输出 A，命令行前输出 B；执行前输出 C;<cmd>
    r#"__termior_mark_prompt() { printf '\033]133;A\007'; }
__termior_mark_input() { printf '\033]133;B\007'; }
__termior_mark_cmd() { printf '\033]133;C;%s\007' "$1"; }
__termior_mark_exit() { printf '\033]133;D;%s\007' "$1"; }"#
}

/// 生成 zsh 的 shell integration 注入。
///
/// 「ZDOTDIR 四件套」：在临时 ZDOTDIR 下放置 `.zshenv`（source 用户真实 `.zshenv`）
/// 与 `.zshrc`（source 用户真实 `.zshrc`，注入 precmd/preexec hooks）。
pub fn zsh_snippets() -> ShellIntegrationSnippets {
    let zshenv = "\
# Termior shell integration (zsh) — .zshenv wrapper
# Source the user's real .zshenv if it exists.
if [ -n \"$TERMior_REAL_ZDOTDIR\" ] && [ -f \"$TERMior_REAL_ZDOTDIR/.zshenv\" ]; then
    source \"$TERMior_REAL_ZDOTDIR/.zshenv\"
elif [ -f \"$HOME/.zshenv\" ]; then
    source \"$HOME/.zshenv\"
fi
";
    let zshrc = format!(
        "\
# Termior shell integration (zsh) — .zshrc wrapper
# Source the user's real zshrc.
if [ -n \"$TERMior_REAL_ZDOTDIR\" ] && [ -f \"$TERMior_REAL_ZDOTDIR/.zshrc\" ]; then
    source \"$TERMior_REAL_ZDOTDIR/.zshrc\"
elif [ -f \"$HOME/.zshrc\" ]; then
    source \"$HOME/.zshrc\"
fi

# --- Termior integration (OSC 7 / 133) ---
export TERMior_HOSTNAME=\"${{TERMior_HOSTNAME:-$HOST}}\"
{osc7}
{osc133}

# Report cwd on each prompt (precmd) and emit prompt-start mark.
autoload -Uz add-zsh-hook
__termior_precmd() {{
    __termior_osc7
    __termior_mark_exit \"$?\"
    __termior_mark_prompt
}}
__termior_preexec() {{
    __termior_mark_cmd \"$1\"
}}
add-zsh-hook precmd __termior_precmd
add-zsh-hook preexec __termior_preexec
# Emit the input-end mark right after prompt is drawn.
PS1=\"$(__termior_mark_input)$PS1\"
",
        osc7 = osc7(),
        osc133 = osc133_prompt(),
    );

    ShellIntegrationSnippets {
        kind: ShellKind::Zsh,
        args: vec![],
        files: vec![(".zshenv".into(), zshenv.into()), (".zshrc".into(), zshrc)],
        env: vec![],
    }
}

/// 生成 bash 的 shell integration（经 `--rcfile`）。
pub fn bash_snippets() -> ShellIntegrationSnippets {
    let rcfile = format!(
        "\
# Termior shell integration (bash) — rcfile wrapper
# Source the user's real bashrc.
if [ -f \"$HOME/.bashrc\" ]; then
    source \"$HOME/.bashrc\"
elif [ -f \"$HOME/.bash_profile\" ]; then
    source \"$HOME/.bash_profile\"
fi

# --- Termior integration (OSC 7 / 133) ---
export TERMior_HOSTNAME=\"${{TERMior_HOSTNAME:-$HOSTNAME}}\"
{osc7}
{osc133}

# Bash reports cwd via PROMPT_COMMAND (array form supported).
__termior_prompt_cmd() {{
    __termior_osc7
    __termior_mark_exit \"$?\"
    __termior_mark_prompt
    __termior_mark_input
}}
if [[ \"${{PROMPT_COMMAND:-}}\" == *\"__termior_prompt_cmd\"* ]]; then
    :
else
    if [[ -n \"${{PROMPT_COMMAND:-}}\" ]]; then
        PROMPT_COMMAND=\"__termior_prompt_cmd; $PROMPT_COMMAND\"
    else
        PROMPT_COMMAND=\"__termior_prompt_cmd\"
    fi
fi
# Emit command-start mark before executing user command.
trap '__termior_mark_cmd \"$BASH_COMMAND\"' DEBUG
",
        osc7 = osc7(),
        osc133 = osc133_prompt(),
    );
    ShellIntegrationSnippets {
        kind: ShellKind::Bash,
        args: vec!["--rcfile".into()],
        files: vec![("termior-bashrc.sh".into(), rcfile)],
        env: vec![],
    }
}

/// 生成 pwsh 的 shell integration（经 `-File profile.ps1`）。
pub fn pwsh_snippets() -> ShellIntegrationSnippets {
    let profile = "\
# Termior shell integration (pwsh) — profile.ps1 wrapper
# Source the user's real $PROFILE if it exists.
if ($env:TERMior_REAL_PROFILE -and (Test-Path $env:TERMior_REAL_PROFILE)) {
    . $env:TERMior_REAL_PROFILE
} elseif ($PROFILE -and (Test-Path $PROFILE)) {
    . $PROFILE
}

# --- Termior integration (OSC 7 / 133) ---
$env:TERMior_HOSTNAME = if ($env:TERMior_HOSTNAME) { $env:TERMior_HOSTNAME } else { $env:COMPUTERNAME }

function script:__termior_Osc7 {
    # Build a well-formed file URL: authority and path must be separated by '/'.
    # Use ProviderPath and strip extended-length prefixes so the emitted path is
    # always a plain drive/UNC path — otherwise the hostname glues onto the drive
    # or the provider qualifier leaks into the URL. Skip non-FileSystem providers.
    if (-not $PWD.Provider -or $PWD.Provider.Name -ne 'FileSystem') { return }
    $path = $PWD.ProviderPath
    if ($path.StartsWith('\\\\?\\UNC\\')) {
        $path = '\\\\' + $path.Substring(8)
    } elseif ($path.StartsWith('\\\\?\\')) {
        $path = $path.Substring(4)
    }
    $path = $path -replace '\\\\','/'
    if ($path -match '^[A-Za-z]:/') { $path = '/' + $path }
    $raw = \"file://$($env:TERMior_HOSTNAME)$path\"
    [Console]::Write([char]27 + \"]7;\" + $raw + [char]7)
}
function script:__termior_MarkPrompt { [Console]::Write([char]27 + \"]133;A\" + [char]7) }
function script:__termior_MarkInput  { [Console]::Write([char]27 + \"]133;B\" + [char]7) }
function script:__termior_MarkCmd($c){ [Console]::Write([char]27 + \"]133;C;$c\" + [char]7) }
function script:__termior_MarkExit($e){ [Console]::Write([char]27 + \"]133;D;$e\" + [char]7) }

# precmd: emit cwd + exit code + prompt-start, then input-end after prompt render.
$__termior_OldPrompt = $function:prompt
function global:prompt {
    __termior_Osc7
    __termior_MarkExit $LASTEXITCODE
    __termior_MarkPrompt
    $out = if ($__termior_OldPrompt) { & $__termior_OldPrompt } else { \"PS> \" }
    __termior_MarkInput
    $out
}
# preexec: emit command-start on Enter, then chain to the default accept-line action.
# (Never bind Tab — that would replace the default tab-completion handler.)
if (Get-Module -ListAvailable PSReadLine) {
    Set-PSReadLineKeyHandler -Key Enter -BriefDescription TermiorMark -ScriptBlock {
        $line = $null
        [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$null)
        __termior_MarkCmd $line
        [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
    }
}
";
    ShellIntegrationSnippets {
        kind: ShellKind::Pwsh,
        args: vec!["-NoProfile".into(), "-NoExit".into(), "-File".into()],
        files: vec![("profile.ps1".into(), profile.into())],
        env: vec![],
    }
}

/// 按 shell 类型选择注入方案。
pub fn snippets_for(kind: ShellKind) -> Option<ShellIntegrationSnippets> {
    match kind {
        ShellKind::Zsh => Some(zsh_snippets()),
        ShellKind::Bash => Some(bash_snippets()),
        ShellKind::Fish => None,
        ShellKind::Pwsh | ShellKind::PowerShell => Some(pwsh_snippets()),
        ShellKind::Cmd => None, // cmd 不支持 shell integration
    }
}

/// 生成把 shell 当前目录切到 `dir` 的注入命令（含回车），按 shell 类型选择语法：
///
/// - cmd.exe：`cd /d "<path>"`（`/d` 处理跨盘符；双引号防空格）
/// - PowerShell/pwsh：`cd '<path>'`（单引号防 `$` 展开；`'` 转义为 `''`）
/// - POSIX shell（bash/zsh/fish）：`cd '<path>'`（`'` 转义为 `'\''`）；
///   `wsl` 为 true 时先把 Windows 盘符路径转成 `/mnt/<drive>/...`，
///   否则盘符路径统一为正斜杠（Git Bash/MSYS 语义，`D:\foo` → `D:/foo`）。
pub fn cd_command(kind: ShellKind, wsl: bool, dir: &Path) -> String {
    match kind {
        ShellKind::Cmd => format!("cd /d \"{}\"\r", dir.display()),
        ShellKind::Pwsh | ShellKind::PowerShell => {
            format!("cd '{}'\r", dir.display().to_string().replace('\'', "''"))
        }
        ShellKind::Bash | ShellKind::Zsh | ShellKind::Fish => {
            let path = if wsl {
                windows_to_wsl_path(dir)
            } else {
                posix_display_path(dir)
            };
            format!("cd '{}'\r", path.replace('\'', "'\\''"))
        }
    }
}

/// 把 Windows 路径（`C:\Users\foo`）转成 WSL 内可见的 `/mnt/c/Users/foo`。
/// 已是正斜杠或不可转换的路径原样返回（保守降级，不致失败）。
/// 纯字符串变换，与编译目标平台无关（盘符前缀本身即代表 Windows 语义）。
pub fn windows_to_wsl_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return s.into_owned();
    }
    let drive = (bytes[0] as char).to_ascii_lowercase();
    let rest = &s[2..];
    let rest = rest.replace('\\', "/");
    format!("/mnt/{drive}{rest}")
}

/// POSIX shell 里的路径展示：盘符路径统一为正斜杠（MSYS/Git Bash 语义），其余原样。
fn posix_display_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zsh_uses_zdotdir_quartet() {
        let s = zsh_snippets();
        let names: Vec<_> = s.files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&".zshenv"));
        assert!(names.contains(&".zshrc"));
        // 内部 source 用户真实配置
        let zshrc = s
            .files
            .iter()
            .find(|(n, _)| n == ".zshrc")
            .unwrap()
            .1
            .as_str();
        assert!(zshrc.contains("TERMior_REAL_ZDOTDIR") || zshrc.contains(".zshrc"));
        // 注入 OSC 7 / 133
        assert!(zshrc.contains("]7;") || zshrc.contains("__termior_osc7"));
        assert!(zshrc.contains("133"));
    }

    #[test]
    fn bash_uses_rcfile_arg() {
        let s = bash_snippets();
        assert!(s.args.contains(&"--rcfile".to_string()));
        assert!(s.files.iter().any(|(n, _)| n == "termior-bashrc.sh"));
        let rc = s.files[0].1.as_str();
        assert!(rc.contains("PROMPT_COMMAND"));
        assert!(rc.contains("133"));
    }

    #[test]
    fn pwsh_uses_file_arg_and_sources_profile() {
        let s = pwsh_snippets();
        assert!(s.args.contains(&"-File".to_string()));
        let prof = s
            .files
            .iter()
            .find(|(n, _)| n == "profile.ps1")
            .unwrap()
            .1
            .as_str();
        assert!(prof.contains("TERMior_REAL_PROFILE") || prof.contains("$PROFILE"));
        assert!(prof.contains("$PWD.ProviderPath"));
        assert!(prof.contains("FileSystem"));
        assert!(prof.contains("133"));
    }

    #[test]
    fn pwsh_osc7_url_separates_authority_and_path() {
        // Windows `$PWD.ProviderPath` is `C:\Users\...` → after `\`→`/` replace it does NOT
        // start with `/`. Without an explicit separator the hostname glues onto the
        // drive letter (`file://HOSTC:/Users/...`). The snippet must prefix `/` so
        // the emitted URL is `file://host/C:/Users/...`.
        let s = pwsh_snippets();
        let prof = s
            .files
            .iter()
            .find(|(n, _)| n == "profile.ps1")
            .unwrap()
            .1
            .as_str();
        assert!(
            prof.contains(r"if ($path -match '^[A-Za-z]:/') { $path = '/' + $path }"),
            "pwsh OSC 7 must ensure a '/' separates authority and path"
        );
        assert!(
            prof.contains(r#"$raw = "file://$($env:TERMior_HOSTNAME)$path""#),
            "pwsh OSC 7 must build the URL from the slash-prefixed path"
        );
        // The old broken form (path glued directly onto the hostname) must be gone.
        assert!(!prof.contains(r#"file://$($env:TERMior_HOSTNAME)$($PWD"#));
    }

    #[test]
    fn pwsh_does_not_bind_tab_and_marks_command_on_enter() {
        // Binding Tab would replace the default tab-completion handler. The
        // command-start mark must ride on Enter and chain to AcceptLine.
        let s = pwsh_snippets();
        let prof = s
            .files
            .iter()
            .find(|(n, _)| n == "profile.ps1")
            .unwrap()
            .1
            .as_str();
        assert!(
            !prof.contains("-Key Tab"),
            "pwsh integration must not hijack the Tab key"
        );
        assert!(
            prof.contains("-Key Enter"),
            "command mark should bind Enter"
        );
        assert!(
            prof.contains("[Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()"),
            "Enter handler must chain to AcceptLine so the line still submits"
        );
    }

    #[test]
    fn cmd_has_no_integration() {
        assert!(snippets_for(ShellKind::Cmd).is_none());
    }

    #[test]
    fn injection_emits_osc7_and_osc133() {
        for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Pwsh] {
            let s = snippets_for(kind).unwrap();
            let all = s.files.iter().map(|(_, c)| c.as_str()).collect::<String>();
            // 必须同时注入 OSC 7 与 OSC 133
            assert!(all.contains("]7;"), "{kind:?} missing OSC 7");
            assert!(all.contains("133;"), "{kind:?} missing OSC 133");
        }
    }

    #[test]
    fn injection_sources_user_real_config() {
        // 关键：内部必须 source 用户真实配置，而非替换
        for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Pwsh] {
            let s = snippets_for(kind).unwrap();
            let all = s.files.iter().map(|(_, c)| c.as_str()).collect::<String>();
            assert!(
                all.contains("source") || all.contains(". $PROFILE") || all.contains(". $env"),
                "{kind:?} should source real user config"
            );
        }
    }

    #[test]
    fn shellkind_name() {
        assert_eq!(ShellKind::Zsh.name(), "zsh");
        assert_eq!(ShellKind::Fish.name(), "fish");
        assert_eq!(ShellKind::Pwsh.name(), "pwsh");
        assert_eq!(ShellKind::Cmd.name(), "cmd");
    }

    #[test]
    fn shellkind_serialize() {
        let json = serde_json::to_string(&ShellKind::Zsh).unwrap();
        assert_eq!(json, "\"zsh\"");
    }

    #[test]
    fn cd_command_cmd_uses_slash_d_and_double_quotes() {
        let cmd = cd_command(ShellKind::Cmd, false, Path::new(r"D:\foo bar"));
        assert_eq!(cmd, "cd /d \"D:\\foo bar\"\r");
    }

    #[test]
    fn cd_command_powershell_single_quotes_and_escapes_quote() {
        let cmd = cd_command(ShellKind::Pwsh, false, Path::new(r"D:\it's"));
        assert_eq!(cmd, "cd 'D:\\it''s'\r");
        // `$` 在单引号内不展开。
        let cmd = cd_command(ShellKind::PowerShell, false, Path::new(r"C:\$recycle"));
        assert_eq!(cmd, "cd 'C:\\$recycle'\r");
    }

    #[test]
    fn cd_command_posix_native_path() {
        let cmd = cd_command(ShellKind::Bash, false, Path::new("/home/u/proj a"));
        assert_eq!(cmd, "cd '/home/u/proj a'\r");
        let cmd = cd_command(ShellKind::Zsh, false, Path::new("/it's/me"));
        assert_eq!(cmd, "cd '/it'\\''s/me'\r");
    }

    #[test]
    fn cd_command_posix_on_windows_uses_forward_slashes() {
        // Git Bash/MSYS：`cd` 内建不解析反斜杠盘符路径，统一为正斜杠。
        let cmd = cd_command(ShellKind::Bash, false, Path::new(r"D:\foo\bar"));
        assert_eq!(cmd, "cd 'D:/foo/bar'\r");
    }

    #[test]
    fn cd_command_wsl_converts_to_mnt_path() {
        let cmd = cd_command(ShellKind::Bash, true, Path::new(r"D:\proj a"));
        assert_eq!(cmd, "cd '/mnt/d/proj a'\r");
    }

    #[test]
    fn cd_command_unicode_path_passes_through() {
        // Windows 中文路径：非引号字符原样透传，不被转义破坏。
        let cmd = cd_command(ShellKind::Cmd, false, Path::new(r"D:\项目\测试 目录"));
        assert_eq!(cmd, "cd /d \"D:\\项目\\测试 目录\"\r");
        let cmd = cd_command(ShellKind::Pwsh, false, Path::new(r"D:\项目"));
        assert_eq!(cmd, "cd 'D:\\项目'\r");
    }

    #[test]
    fn windows_to_wsl_path_converts_drive_and_separators() {
        let p = Path::new(r"C:\Users\foo\termior-bashrc.sh");
        assert_eq!(windows_to_wsl_path(p), "/mnt/c/Users/foo/termior-bashrc.sh");
        // 无盘符路径原样返回（保守降级）。
        let rel = Path::new("relative/path");
        assert_eq!(windows_to_wsl_path(rel), "relative/path");
    }
}
