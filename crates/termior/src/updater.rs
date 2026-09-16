//! One updater per application, shared by every settings window.
use gpui::{App, AppContext, Context, Entity, Global, Task};
use std::time::Duration;
use termior_platform::update::{self, PreparedUpdate};

pub struct Updater {
    pub status: String,
    pub busy: bool,
    pub ready: Option<PreparedUpdate>,
    pub installer_opened: bool,
    enabled: bool,
    generation: u64,
    operation: Option<Task<()>>,
    timer: Option<Task<()>>,
}

struct GlobalUpdater(Entity<Updater>);
impl Global for GlobalUpdater {}

pub fn entity(cx: &App) -> Entity<Updater> {
    cx.global::<GlobalUpdater>().0.clone()
}

pub fn ready(cx: &App) -> bool {
    cx.try_global::<GlobalUpdater>()
        .is_some_and(|updater| updater.0.read(cx).ready.is_some())
}

pub fn init(cx: &mut App) {
    if cx.has_global::<GlobalUpdater>() {
        return;
    }
    let updater = cx.new(|_| Updater {
        status: "Updates have not been checked".into(),
        busy: false,
        ready: None,
        installer_opened: false,
        enabled: false,
        generation: 0,
        operation: None,
        timer: None,
    });
    cx.set_global(GlobalUpdater(updater));
}

pub fn start(enabled: bool, cx: &mut App) {
    init(cx);
    entity(cx).update(cx, |this, cx| {
        this.enabled = enabled;
        if this.timer.is_some() {
            return;
        }
        this.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(15))
                .await;
            loop {
                if this
                    .update(cx, |this, cx| {
                        if this.enabled {
                            this.check(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_secs(6 * 60 * 60))
                    .await;
            }
        }));
    });
}

pub fn set_enabled(enabled: bool, cx: &mut App) {
    init(cx);
    entity(cx).update(cx, |this, cx| {
        if this.enabled == enabled {
            return;
        }
        this.enabled = enabled;
        // Discard an in-flight response when the preference changes. Background I/O
        // can finish, but cannot trigger a subsequent download or publish stale state.
        if !this.installer_opened {
            this.generation += 1;
            this.operation = None;
            this.busy = false;
            if this.ready.is_none() {
                this.status = "Automatic updates are off".into();
            }
            if enabled {
                this.check(cx);
            }
        }
        cx.notify();
    });
}

impl Updater {
    pub fn check(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.ready.is_some() || self.installer_opened {
            return;
        }
        self.busy = true;
        self.status = "Checking for updates…".into();
        let generation = self.generation;
        self.operation = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { update::check() })
                .await;
            let available = match result {
                Ok(Some(update)) => update,
                result => {
                    let _ = this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return;
                        }
                        this.busy = false;
                        this.status = match result {
                            Ok(_) => "You are up to date".into(),
                            Err(error) => format!("Update check failed: {error}"),
                        };
                        cx.notify();
                    });
                    return;
                }
            };
            let proceed = this
                .update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    this.status = format!("Downloading Termior {}…", available.version);
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !proceed {
                return;
            }
            let result = cx
                .background_executor()
                .spawn(async move { update::download(available) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(prepared) => {
                        this.status = format!(
                            "Termior {} is ready to install (SHA-256 verified)",
                            prepared.version
                        );
                        this.ready = Some(prepared);
                    }
                    Err(error) => this.status = format!("Update download failed: {error}"),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub fn install(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(prepared) = self.ready.take() else {
            return;
        };
        self.busy = true;
        // Do not let a preference change cancel an installer handoff.
        self.installer_opened = true;
        self.status = "Opening installer…".into();
        self.operation = Some(cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { prepared.launch() }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(path) => this.status = format!("Installer opened. Finish your terminal tasks, close Termior, and complete installation. Installer: {}", path.display()),
                    Err(error) => {
                        this.installer_opened = false;
                        this.status = format!("Could not open installer: {error}. Check again to retry or use release downloads.");
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}
