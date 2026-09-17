//! Isolated OpenSSH askpass process. Never initialize app logging or a workspace.
use gpui::{px, size, AppContext, Bounds, Focusable, WindowBounds};
use std::{io::Write, process::Command};
use termior_ssh::{
    credentials::{self, Kind},
    Profile,
};

fn reply(secret: &str) -> ! {
    let mut out = std::io::stdout().lock();
    let ok = writeln!(out, "{secret}").and_then(|_| out.flush()).is_ok();
    std::process::exit(if ok { 0 } else { 1 });
}

fn resolved_destination(profile: &Profile) -> (String, String, bool) {
    let mut cmd = Command::new("ssh");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd.arg("-G");
    if !profile.user.is_empty() {
        cmd.args(["-l", &profile.user]);
    }
    if let Some(port) = profile.port {
        cmd.args(["-p", &port.to_string()]);
    }
    cmd.arg("--").arg(&profile.host);
    let mut user = profile.user.clone();
    let mut host = profile.host.clone();
    let mut direct = false;
    if let Ok(output) = cmd.output() {
        if output.status.success() {
            direct = profile.jump_host.is_empty();
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if ["proxyjump ", "proxycommand "].iter().any(|prefix| {
                    line.strip_prefix(prefix)
                        .is_some_and(|value| value != "none")
                }) {
                    direct = false;
                }
                if let Some(value) = line.strip_prefix("user ") {
                    user = value.into();
                }
                if let Some(value) = line.strip_prefix("hostname ") {
                    host = value.into();
                }
            }
        }
    }
    (user, host, direct)
}

pub fn run() -> ! {
    let Some(prompt) = std::env::args().nth(1) else {
        std::process::exit(1)
    };
    let profile: Profile = std::env::var("TERMIOR_SSH_ASKPASS_PROFILE")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(|p: &Profile| p.validate().is_ok())
        .unwrap_or_else(|| std::process::exit(1));
    // OpenSSH host trust is always explicit; never answer from the credential vault.
    if std::env::var("SSH_ASKPASS_PROMPT").as_deref() == Ok("confirm")
        || prompt.contains("Are you sure you want to continue connecting")
    {
        let result = rfd::MessageDialog::new()
            .set_title("SSH 主机指纹确认")
            .set_description(&prompt)
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Warning)
            .show();
        if result == rfd::MessageDialogResult::Yes {
            reply("yes")
        }
        std::process::exit(1);
    }
    if std::env::var("SSH_ASKPASS_PROMPT").as_deref() == Ok("none") {
        rfd::MessageDialog::new()
            .set_title("SSH 认证提示")
            .set_description(&prompt)
            .show();
        std::process::exit(0);
    }
    let (user, host, direct) = resolved_destination(&profile);
    let kind = credentials::prompt_kind(&profile, &prompt, &user, &host);
    // A ProxyJump child's server-controlled keyboard-interactive prompt can mimic
    // the final host. OpenSSH askpass does not expose which hop invoked it.
    let mut error =
        (!direct).then(|| "代理/跳板连接需手动确认凭据；可使用 SSH Agent 免输入。".to_owned());
    if profile.use_saved_credentials && direct {
        if let Some(kind) = kind {
            // A rejected saved password must not cause repeated automatic attempts.
            let first_attempt = std::env::var_os("TERMIOR_SSH_ASKPASS_STATE").is_some_and(|dir| {
                let path = std::path::PathBuf::from(dir).join(match kind {
                    Kind::Password => "password-used",
                    Kind::Passphrase => "passphrase-used",
                });
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .is_ok()
            });
            if first_attempt {
                match credentials::load(&profile, kind) {
                    Ok(Some(secret)) => reply(&secret),
                    Ok(None) => (),
                    Err(message) => error = Some(message),
                }
            }
        }
    }
    let prompt = format!(
        "{} · {}\n{}{}",
        profile.name,
        profile.host,
        prompt,
        error.map(|e| format!("\n{e}")).unwrap_or_default()
    );
    gpui_platform::application()
        .with_assets(termior_ui_kit::IconAssets)
        .run(move |cx| {
            let bounds = Bounds::centered(None, size(px(600.), px(330.)), cx);
            let _ = cx.open_window(
                crate::app_identity::window_options(WindowBounds::Windowed(bounds)),
                |window, cx| {
                    let view = cx.new(|cx| crate::ssh_view::SshView::new_prompt(prompt, cx));
                    window.set_window_title("SSH 身份认证 · Termior");
                    window.focus(&view.read(cx).focus_handle(cx), cx);
                    window.on_window_should_close(cx, |_, _| std::process::exit(1));
                    window.activate_window();
                    view
                },
            );
        });
    std::process::exit(1)
}
