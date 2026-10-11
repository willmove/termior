//! One updater per application, shared by every settings window.
use gpui::{App, AppContext, Context, Entity, Global, Task};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use termior_platform::update::{self, PreparedUpdate};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);
/// Speed is averaged over this trailing window so the ETA does not jitter per chunk.
const SPEED_WINDOW: Duration = Duration::from_secs(5);
const MIN_SPEED_SPAN: Duration = Duration::from_secs(1);

pub struct Updater {
    pub status: String,
    pub busy: bool,
    pub ready: Option<PreparedUpdate>,
    pub installer_opened: bool,
    /// Present only while an installer download is in flight.
    pub progress: Option<DownloadProgress>,
    enabled: bool,
    generation: u64,
    operation: Option<Task<()>>,
    progress_poll: Option<Task<()>>,
    timer: Option<Task<()>>,
}

pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: u64,
    samples: VecDeque<(Instant, u64)>,
}

impl DownloadProgress {
    fn new(total: u64, now: Instant) -> Self {
        Self {
            downloaded: 0,
            total,
            samples: VecDeque::from([(now, 0)]),
        }
    }

    fn record(&mut self, downloaded: u64, now: Instant) {
        self.downloaded = downloaded.min(self.total);
        self.samples.push_back((now, self.downloaded));
        // Keep one sample at or beyond the window edge so the span stays >= SPEED_WINDOW.
        while self
            .samples
            .get(1)
            .is_some_and(|(at, _)| now - *at >= SPEED_WINDOW)
        {
            self.samples.pop_front();
        }
    }

    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.downloaded as f64 / self.total as f64) as f32
        }
    }

    /// Bytes per second over the trailing window; `None` until a full second is sampled.
    pub fn speed(&self) -> Option<f64> {
        let (start, first) = self.samples.front()?;
        let (end, last) = self.samples.back()?;
        let span = *end - *start;
        (span >= MIN_SPEED_SPAN).then(|| (last - first) as f64 / span.as_secs_f64())
    }

    pub fn eta(&self) -> Option<Duration> {
        let speed = self.speed().filter(|speed| *speed > 0.0)?;
        Some(Duration::from_secs_f64(
            (self.total - self.downloaded) as f64 / speed,
        ))
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `m:ss`, or `h:mm:ss` past an hour.
pub fn format_eta(eta: Duration) -> String {
    let secs = eta.as_secs_f64().ceil() as u64;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
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
        status: t!("updater.status_not_checked").to_string(),
        busy: false,
        ready: None,
        installer_opened: false,
        progress: None,
        enabled: false,
        generation: 0,
        operation: None,
        progress_poll: None,
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
            this.stop_progress();
            this.busy = false;
            if this.ready.is_none() {
                this.status = t!("updater.status_disabled").to_string();
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
        self.status = t!("updater.status_checking").to_string();
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
                            Ok(_) => t!("updater.status_up_to_date").to_string(),
                            Err(error) => {
                                tf!("updater.status_check_failed", "error" => error.to_string())
                                    .to_string()
                            }
                        };
                        cx.notify();
                    });
                    return;
                }
            };
            let downloaded = Arc::new(AtomicU64::new(0));
            let proceed = this
                .update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    this.status =
                        tf!("updater.status_downloading", "version" => available.version.clone())
                            .to_string();
                    this.start_progress(available.size(), downloaded.clone(), cx);
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !proceed {
                return;
            }
            let result = cx
                .background_executor()
                .spawn(async move { update::download(available, &downloaded) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.busy = false;
                this.stop_progress();
                match result {
                    Ok(prepared) => {
                        this.status = tf!(
                            "updater.status_ready",
                            "version" => prepared.version.clone()
                        )
                        .to_string();
                        this.ready = Some(prepared);
                    }
                    Err(error) => {
                        this.status =
                            tf!("updater.status_download_failed", "error" => error.to_string())
                                .to_string()
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn start_progress(&mut self, total: u64, downloaded: Arc<AtomicU64>, cx: &mut Context<Self>) {
        self.progress = Some(DownloadProgress::new(total, Instant::now()));
        self.progress_poll = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(PROGRESS_INTERVAL).await;
            let polling = this.update(cx, |this, cx| {
                let Some(progress) = this.progress.as_mut() else {
                    return false;
                };
                progress.record(downloaded.load(Ordering::Relaxed), Instant::now());
                cx.notify();
                true
            });
            if !matches!(polling, Ok(true)) {
                break;
            }
        }));
    }

    fn stop_progress(&mut self) {
        self.progress = None;
        self.progress_poll = None;
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
        self.status = t!("updater.status_opening_installer").to_string();
        self.operation = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { prepared.launch() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(path) => {
                        this.status = tf!(
                            "updater.status_installer_opened",
                            "path" => path.display().to_string()
                        )
                        .to_string()
                    }
                    Err(error) => {
                        this.installer_opened = false;
                        this.status = tf!(
                            "updater.status_installer_failed",
                            "error" => error.to_string()
                        )
                        .to_string();
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_uses_trailing_window_and_eta_follows() {
        let start = Instant::now();
        let mut progress = DownloadProgress::new(10_000, start);
        progress.record(500, start + Duration::from_millis(500));
        assert_eq!(progress.speed(), None);
        assert_eq!(progress.eta(), None);
        for second in 1..=8 {
            progress.record(second * 1000, start + Duration::from_secs(second));
        }
        assert_eq!(progress.speed(), Some(1000.0));
        assert_eq!(progress.eta(), Some(Duration::from_secs(2)));
        assert_eq!(
            progress.samples.front().unwrap().0,
            start + Duration::from_secs(3)
        );
        assert!((progress.fraction() - 0.8).abs() < f32::EPSILON);
        // A stall drives the windowed speed to zero instead of reporting a stale ETA.
        progress.record(8000, start + Duration::from_secs(20));
        assert_eq!(progress.speed(), Some(0.0));
        assert_eq!(progress.eta(), None);
    }

    #[test]
    fn formats_sizes_and_remaining_time() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(35 * 1024 * 1024), "35.0 MB");
        assert_eq!(format_eta(Duration::from_millis(8_200)), "0:09");
        assert_eq!(format_eta(Duration::from_secs(125)), "2:05");
        assert_eq!(format_eta(Duration::from_secs(3_725)), "1:02:05");
    }
}
