//! 设置写盘与防抖保存。

use super::*;

impl SettingsView {
    /// 跟踪一个 fire-and-forget 的异步任务,使其在设置窗口关闭时随实体一起被取消。
    /// 丢弃已完成的任务,避免无界增长。
    pub(super) fn track_task(&mut self, task: gpui::Task<()>) {
        self.pending_tasks.retain(|task| !task.is_ready());
        self.pending_tasks.push(task);
    }

    pub(super) fn schedule_save_if_dirty(&mut self, cx: &mut Context<Self>) {
        if self.settings_dirty {
            self.settings_dirty = false;
            self.schedule_save(cx);
        }
    }

    /// 防抖写盘；逐字符编辑不会每次都落盘。
    pub(super) fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        let task = cx.spawn(async move |view, cx| {
            cx.background_executor().timer(SETTINGS_SAVE_DEBOUNCE).await;
            let _ = view.update(cx, |view, cx| {
                if view.save_generation == generation {
                    view.persist(cx);
                }
            });
        });
        self.track_task(task);
    }

    /// 立即写盘（关闭窗口或换页前 flush）。
    pub fn flush_save(&mut self, cx: &mut Context<Self>) {
        self.commit_edit();
        self.settings_dirty = false;
        self.save_generation = self.save_generation.wrapping_add(1);
        self.persist(cx);
    }

    pub(super) fn persist(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.settings.validate() {
            self.status = error.to_string();
            cx.notify();
            return;
        }
        let result = self
            .data_dir
            .as_ref()
            .map_or_else(
                || Err("application data directory is unavailable".to_owned()),
                |dir| {
                    std::fs::create_dir_all(dir)
                        .map_err(|error| error.to_string())
                        .and_then(|()| {
                            serde_json::to_string_pretty(&self.settings)
                                .map_err(|error| error.to_string())
                        })
                        .and_then(|json| {
                            atomic_write(&dir.join("Termior-settings.json"), &json)
                                .map_err(|error| error.to_string())
                        })
                },
            )
            .and_then(|()| {
                let dir = self
                    .data_dir
                    .as_ref()
                    .ok_or_else(|| "application data directory is unavailable".to_owned())?;
                let files = DataFiles::new(dir);
                files
                    .themes()
                    .save(&self.themes)
                    .map_err(|error| error.to_string())?;
                files
                    .agents()
                    .save(&self.agents)
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => {
                // 保存成功后清理与保存相关的旧状态；前缀用当前语言的文案生成，
                // 避免非英文界面下英文前缀匹配失效导致旧错误残留。
                let stale = self.status.is_empty()
                    || self.status == *t!("settings.status.saved")
                    || self
                        .status
                        .starts_with(&*tf!("settings.status.save_failed", "error" => ""))
                    || self
                        .status
                        .starts_with(&*tf!("settings.status.invalid_value", "error" => ""));
                if stale {
                    self.status.clear();
                }
                if let Some(on_save) = &self.on_save {
                    on_save(&self.settings, cx);
                }
            }
            Err(error) => {
                self.status = tf!("settings.status.save_failed", "error" => error).to_string()
            }
        }
        cx.notify();
    }

    /// 仅写盘、不依赖 GPUI（窗口 Drop 时的兜底）。
    pub(super) fn persist_to_disk(&self) -> Result<(), String> {
        self.settings
            .validate()
            .map_err(|error| error.to_string())?;
        let dir = self
            .data_dir
            .as_ref()
            .ok_or_else(|| "application data directory is unavailable".to_owned())?;
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        let json =
            serde_json::to_string_pretty(&self.settings).map_err(|error| error.to_string())?;
        atomic_write(&dir.join("Termior-settings.json"), &json)
            .map_err(|error| error.to_string())?;
        let files = DataFiles::new(dir);
        files
            .themes()
            .save(&self.themes)
            .map_err(|error| error.to_string())?;
        files
            .agents()
            .save(&self.agents)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}
