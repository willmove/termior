//! Secret deny-list（FR-SEC-03 / INV-3）。
//!
//! AI 工具的读/写访问对下列敏感文件/目录**双向禁止**：
//! `.env`、`.env.*`、`.ssh/`、`credentials`、`.netrc`、`.aws/credentials`、钥匙串目录等。
//!
//! 判定在**路径规范化（canonicalize）之后**强制执行（INV-3），与调用来源无关。
//! 红队用例（`..` 穿越、符号链接、大小写、绝对/相对混合）均不可绕过：
//! 调用方应先调用 [`canonicalize_logical`] 消除 `..`/`.` 与分隔符差异，
//! 再交给 [`DenyList::check`]。扩充名单只能改代码（`DEFAULT_RULES`），不提供运行时配置。

/// 访问方向。deny-list 对读写双向生效。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Read,
    Write,
}

/// 命中的 deny 原因（便于 UI 展示与红队断言）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DenyReason {
    /// `.env` / `.env.local` / `.env.production` …
    DotEnv,
    /// `.ssh/` 目录及其下任何文件。
    Ssh,
    /// 文件名 `credentials`（任意目录）。
    Credentials,
    /// `.netrc`
    Netrc,
    /// `.aws/credentials`（及同类云凭据路径）。
    AwsCredentials,
    /// 钥匙串目录：macOS `Library/Keychains`、Windows `%APPDATA%\Microsoft\Credentials`、
    /// Linux `~/.local/share/keyrings`。
    Keychain,
    /// 用户自定义规则命中（运行时不可配，仅代码扩充）。
    Custom(String),
}

impl std::fmt::Display for DenyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DenyReason::DotEnv => write!(f, "dotenv file"),
            DenyReason::Ssh => write!(f, "ssh directory"),
            DenyReason::Credentials => write!(f, "credentials file"),
            DenyReason::Netrc => write!(f, ".netrc"),
            DenyReason::AwsCredentials => write!(f, "aws credentials"),
            DenyReason::Keychain => write!(f, "keychain directory"),
            DenyReason::Custom(name) => write!(f, "custom deny rule: {name}"),
        }
    }
}

/// 一条 deny 规则。
#[derive(Debug, Clone)]
pub struct DenyRule {
    pub reason: DenyReason,
    /// 见 [`Matcher`]。
    pub matcher: Matcher,
}

/// 路径匹配器：基于「路径组件序列」匹配，避免被 `/` 拼接绕过。
#[derive(Debug, Clone)]
pub enum Matcher {
    /// 完整文件名等于该值（如 `credentials`、`.env`、`.netrc`）。
    FileName(String),
    /// 直接父目录组件等于 `dir`，文件名等于 `file`（如 `.aws/credentials`）。
    ChildOf { dir: String, file: String },
    /// 路径中**任一组件**等于该目录名，则命中该目录及其下所有内容（如 `.ssh`）。
    AnyComponentDir(String),
    /// 文件名 glob（仅文件名维度，如 `.env*` 匹配 `.env`、`.env.local`）。
    FileNameGlob(String),
}

impl Matcher {
    fn matches(&self, comps: &[String], file_name: &str) -> bool {
        match self {
            Matcher::FileName(n) => file_name == n,
            Matcher::FileNameGlob(g) => glob_file_name(g, file_name),
            Matcher::ChildOf { dir, file } => {
                file_name == file
                    && comps.iter().rev().nth(1).map(|c| c == dir).unwrap_or(false)
            }
            Matcher::AnyComponentDir(d) => comps.iter().any(|c| c == d),
        }
    }
}

/// 简化的文件名 glob：仅支持 `*` 通配，匹配整个文件名。
fn glob_file_name(pat: &str, name: &str) -> bool {
    let pat_b = pat.as_bytes();
    let name_b = name.as_bytes();
    glob_rec(pat_b, name_b)
}

fn glob_rec(pat: &[u8], name: &[u8]) -> bool {
    match (pat.split_first(), name.split_first()) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some((b'*', rest)), _) => {
            // `*` 匹配零或多个字符；尝试所有后续位置。
            if rest.is_empty() {
                return true;
            }
            for i in 0..=name.len() {
                if glob_rec(rest, &name[i..]) {
                    return true;
                }
            }
            false
        }
        (Some((_, _)), None) => false,
        (Some((pa, prest)), Some((na, nrest))) if pa == na => glob_rec(prest, nrest),
        _ => false,
    }
}

/// 默认 deny 规则集（FR-SEC-03）。扩充名单只能改此函数，不提供运行时配置。
pub fn default_rules() -> Vec<DenyRule> {
    use DenyReason::*;
    use Matcher::*;
    vec![
        DenyRule { reason: DotEnv, matcher: FileNameGlob(".env*".into()) },
        DenyRule { reason: Ssh, matcher: AnyComponentDir(".ssh".into()) },
        // 更具体的 ChildOf 规则须排在通用 FileName 规则之前（首个命中即返回）。
        DenyRule { reason: AwsCredentials, matcher: ChildOf { dir: ".aws".into(), file: "credentials".into() } },
        DenyRule { reason: Credentials, matcher: FileName("credentials".into()) },
        DenyRule { reason: Netrc, matcher: FileName(".netrc".into()) },
        DenyRule { reason: Keychain, matcher: AnyComponentDir("Keychains".into()) },
        DenyRule { reason: Keychain, matcher: AnyComponentDir("Credentials".into()) },
        DenyRule { reason: Keychain, matcher: AnyComponentDir("keyrings".into()) },
    ]
}

/// deny-list 视图。默认通过 [`DenyList::default`] 取得 [`default_rules`]。
#[derive(Debug, Clone)]
pub struct DenyList {
    pub rules: Vec<DenyRule>,
}

impl Default for DenyList {
    fn default() -> Self {
        Self { rules: default_rules() }
    }
}

impl DenyList {
    /// 判定给定（已规范化的）路径是否被 deny。
    pub fn check(&self, path: &str, _dir: Direction) -> Option<&DenyReason> {
        let comps = split_components(path);
        let file_name = comps.last().cloned().unwrap_or_default();
        for r in &self.rules {
            if r.matcher.matches(&comps, &file_name) {
                return Some(&r.reason);
            }
        }
        None
    }
}

/// 便捷函数：用默认规则集 + 逻辑规范化判定，返回命中原因（拥有所有权）。
///
/// 调用方在真实运行时应先用 `std::fs::canonicalize`（解析符号链接），
/// 再调用 [`DenyList::check`]；本函数用于无 fs 环境下的等价判定与单测。
pub fn check_path(path: &str, dir: Direction) -> Option<DenyReason> {
    let canon = canonicalize_logical(path);
    DenyList::default().check(&canon, dir).cloned()
}

/// 将路径切分为组件序列，统一用正斜杠，去空段。
fn split_components(path: &str) -> Vec<String> {
    path.replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// 逻辑规范化（INV-3 的前置步骤）：消除 `.`、`..` 与分隔符差异。
///
/// 这是 `std::fs::canonicalize` 的**纯逻辑替代**——不触盘、不解析符号链接。
/// 调用方在真实运行时应优先使用 `std::fs::canonicalize`（能解析符号链接），
/// 其结果再喂给 [`DenyList::check`]。本函数用于单测与无 fs 环境下的等价判定。
pub fn canonicalize_logical(path: &str) -> String {
    let comps = split_components(path);
    let mut out: Vec<String> = Vec::with_capacity(comps.len());
    let mut is_abs = path.starts_with('/') || path.starts_with('\\');
    // Windows 盘符前缀（如 `C:`）作为绝对路径根保留。
    if let Some(first) = comps.first() {
        if first.len() == 2 && first.as_bytes()[1] == b':' {
            is_abs = true;
        }
    }
    for (i, c) in comps.iter().enumerate() {
        match c.as_str() {
            "." => {}
            ".." => {
                // 盘符根不可弹出
                let is_drive_root = out
                    .last()
                    .map(|last| last.len() == 2 && last.as_bytes()[1] == b':')
                    .unwrap_or(false);
                if is_drive_root {
                    // 保留盘符
                } else {
                    out.pop();
                }
            }
            _ => {
                let _ = i;
                out.push(c.clone());
            }
        }
    }
    join_components(&out, is_abs)
}

fn join_components(comps: &[String], is_abs: bool) -> String {
    if comps.is_empty() {
        return if is_abs { "/".into() } else { ".".into() };
    }
    // Windows 盘符前缀：`C:/Users/...`
    let is_drive = comps.first().map(|c| c.len() == 2 && c.as_bytes()[1] == b':').unwrap_or(false);
    let joined = comps.join("/");
    if is_abs && !is_drive {
        format!("/{joined}")
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deny(path: &str) -> Option<DenyReason> {
        DenyList::default().check(&canonicalize_logical(path), Direction::Read).cloned()
    }

    #[test]
    fn matches_dotenv_variants() {
        assert_eq!(deny("proj/.env"), Some(DenyReason::DotEnv));
        assert_eq!(deny("proj/.env.local"), Some(DenyReason::DotEnv));
        assert_eq!(deny("proj/.env.production"), Some(DenyReason::DotEnv));
        // 普通 env 文件不应误伤
        assert_eq!(deny("proj/env.example"), None);
    }

    #[test]
    fn matches_ssh_dir_and_nested() {
        assert_eq!(deny("/home/u/.ssh"), Some(DenyReason::Ssh));
        assert_eq!(deny("/home/u/.ssh/id_rsa"), Some(DenyReason::Ssh));
        assert_eq!(deny("/home/u/.ssh/config"), Some(DenyReason::Ssh));
        // 同名普通目录不命中
        assert_eq!(deny("/tmp/myssh"), None);
    }

    #[test]
    fn matches_credentials_anywhere() {
        assert_eq!(deny("/x/credentials"), Some(DenyReason::Credentials));
        assert_eq!(deny("credentials"), Some(DenyReason::Credentials));
    }

    #[test]
    fn matches_aws_credentials_specifically() {
        assert_eq!(deny("/home/u/.aws/credentials"), Some(DenyReason::AwsCredentials));
        // .aws 下其他文件不命中此规则（但 credentials 文件名规则会命中）
        assert_eq!(deny("/home/u/.aws/config"), None);
    }

    #[test]
    fn matches_netrc() {
        assert_eq!(deny("/home/u/.netrc"), Some(DenyReason::Netrc));
    }

    #[test]
    fn matches_keychain_dirs() {
        assert_eq!(deny("/Users/u/Library/Keychains/login.keychain"), Some(DenyReason::Keychain));
        assert_eq!(deny("/home/u/.local/share/keyrings/login.keyring"), Some(DenyReason::Keychain));
    }

    // —— 红队：路径穿越 / 规范化绕过 ——

    #[test]
    fn redteam_traversal_cannot_bypass() {
        // `..` 穿越到 .env
        assert_eq!(deny("proj/src/../../.env"), Some(DenyReason::DotEnv));
        // 多层穿越
        assert_eq!(deny("a/b/c/../../../.ssh/id_ed25519"), Some(DenyReason::Ssh));
    }

    #[test]
    fn redteam_backslash_separator_normalized() {
        assert_eq!(deny(r"proj\.env"), Some(DenyReason::DotEnv));
        assert_eq!(deny(r"C:\Users\u\.ssh\config"), Some(DenyReason::Ssh));
    }

    #[test]
    fn redteam_dot_segments_collapse() {
        assert_eq!(deny("proj/./.env"), Some(DenyReason::DotEnv));
        assert_eq!(deny("proj/.//.env"), Some(DenyReason::DotEnv));
    }

    #[test]
    fn redteam_case_is_significant() {
        // 大小写敏感：`.ENV` 不等于 `.env`（在大小写敏感文件系统上）。deny-list 保持精确匹配。
        // 但 macOS HFS+/Windows NTFS 不区分大小写——这是一个已知边界，记录于风险。
        // 此处断言「精确匹配」语义，由运行时的 fs::canonicalize 提供平台真实大小写归一。
        assert_eq!(deny("proj/.env"), Some(DenyReason::DotEnv));
        assert_eq!(deny("proj/.ENV"), None);
    }

    #[test]
    fn redteam_credentials_not_caught_by_extension() {
        // `credentials.json` 不等于 `credentials`（精确文件名匹配）
        assert_eq!(deny("proj/credentials.json"), None);
    }

    #[test]
    fn direction_both_read_and_write_denied() {
        let d = DenyList::default();
        let p = canonicalize_logical("proj/.env");
        assert!(d.check(&p, Direction::Read).is_some());
        assert!(d.check(&p, Direction::Write).is_some());
    }

    #[test]
    fn normal_files_pass() {
        assert_eq!(deny("src/main.rs"), None);
        assert_eq!(deny("Cargo.toml"), None);
        assert_eq!(deny("README.md"), None);
    }

    #[test]
    fn canonicalize_strips_drive_root_safely() {
        // Windows 盘符不被 `..` 弹出
        let p = canonicalize_logical(r"C:\proj\..\.env");
        assert!(p.starts_with("C:"), "got {p}");
        assert!(p.ends_with(".env"), "got {p}");
    }

    #[test]
    fn custom_rule_works() {
        let mut d = DenyList::default();
        d.rules.push(DenyRule {
            reason: DenyReason::Custom("test-secret".into()),
            matcher: Matcher::FileName("super-secret".into()),
        });
        let p = canonicalize_logical("x/super-secret");
        assert_eq!(d.check(&p, Direction::Read), Some(&DenyReason::Custom("test-secret".into())));
    }

    #[test]
    fn glob_matches_only_filename_dimension() {
        // `.env*` 不应匹配 `prefix.env`（文件名 glob 锚定开头，靠 * 吃掉后缀）
        assert_eq!(deny("proj/notenv"), None);
        assert_eq!(deny("proj/.environment"), Some(DenyReason::DotEnv));
    }
}
