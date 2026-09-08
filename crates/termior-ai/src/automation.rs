//! Versioned local automation templates, dedupe, catch-up, and safe retry policy.

use crate::RuntimeBudgets;
use chrono::{Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trigger {
    Manual,
    Interval {
        every_seconds: u64,
        anchor_unix: u64,
    },
    LocalSchedule {
        hour: u8,
        minute: u8,
        timezone: String,
    },
    RepoEvent {
        event: String,
        coalesce_seconds: u64,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CatchUpPolicy {
    Skip,
    RunOnce,
    RunEach { maximum: u16 },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotificationPolicy {
    Actionable,
    FailuresOnly,
    Muted,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateVersion {
    pub version: u64,
    pub prompt: String,
    pub backend: String,
    pub agent_profile: String,
    pub environment_id: String,
    pub max_concurrency: u16,
    pub budgets: RuntimeBudgets,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Automation {
    pub id: String,
    pub name: String,
    pub template: TemplateVersion,
    pub trigger: Trigger,
    pub catch_up: CatchUpPolicy,
    pub notification: NotificationPolicy,
    pub enabled: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationRun {
    pub run_id: String,
    pub task_id: String,
    pub automation_id: String,
    pub template_version: u64,
    pub template: TemplateVersion,
    pub dedupe_key: String,
    pub scheduled_for: u64,
    pub created_at: u64,
    pub status: RunStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutomationStore {
    pub schema_version: u32,
    pub automations: Vec<Automation>,
    pub recent_runs: Vec<AutomationRun>,
    pub audit: Vec<AutomationAuditEvent>,
    #[serde(default)]
    pub engine: AutomationEngine,
}

impl Default for AutomationStore {
    fn default() -> Self {
        Self {
            schema_version: 1,
            automations: vec![],
            recent_runs: vec![],
            audit: vec![],
            engine: AutomationEngine::default(),
        }
    }
}

impl AutomationStore {
    pub fn persist(&self, data_root: &Path) -> Result<PathBuf, std::io::Error> {
        let path = data_root.join("automations.json");
        let json = serde_json::to_string_pretty(self)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        termior_store::atomic_write(&path, &json)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(path)
    }

    pub fn load(data_root: &Path) -> Result<Self, std::io::Error> {
        let path = data_root.join("automations.json");
        match std::fs::read_to_string(path) {
            Ok(json) => serde_json::from_str(&json)
                .map_err(|error| std::io::Error::other(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationAuditEvent {
    pub timestamp: u64,
    pub automation_id: String,
    pub run_id: Option<String>,
    pub kind: String,
    pub detail: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    Queued,
    Running,
    Completed,
    Failed,
    WaitingUser,
    Duplicate,
    Coalesced,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationEngine {
    seen: BTreeMap<String, u64>,
    repo_windows: BTreeMap<String, u64>,
    sequence: u64,
}

pub struct AutomationManager {
    data_root: PathBuf,
    pub store: AutomationStore,
}

impl AutomationManager {
    pub fn load(data_root: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        let data_root = data_root.into();
        let store = AutomationStore::load(&data_root)?;
        Ok(Self { data_root, store })
    }

    pub fn upsert(&mut self, mut automation: Automation) -> Result<(), std::io::Error> {
        if let Some(existing) = self
            .store
            .automations
            .iter_mut()
            .find(|existing| existing.id == automation.id)
        {
            if existing.template != automation.template {
                automation.template.version = existing.template.version.saturating_add(1);
            }
            *existing = automation;
        } else {
            automation.template.version = automation.template.version.max(1);
            self.store.automations.push(automation);
        }
        self.store.persist(&self.data_root).map(|_| ())
    }

    pub fn trigger_manual(
        &mut self,
        automation_id: &str,
        now: u64,
    ) -> Result<AutomationRun, std::io::Error> {
        let automation = self
            .store
            .automations
            .iter()
            .find(|automation| automation.id == automation_id && automation.enabled)
            .cloned()
            .ok_or_else(|| std::io::Error::other("automation is missing or disabled"))?;
        let key = format!(
            "manual:{}:{now}:{}",
            automation.id,
            self.store.recent_runs.len()
        );
        let run = self.store.engine.trigger(&automation, now, key, now);
        self.record_trigger(run.clone(), "manual")?;
        Ok(run)
    }

    pub fn trigger_repo_event(
        &mut self,
        automation_id: &str,
        repository: &str,
        event_id: &str,
        now: u64,
    ) -> Result<AutomationRun, std::io::Error> {
        let automation = self
            .store
            .automations
            .iter()
            .find(|automation| automation.id == automation_id && automation.enabled)
            .cloned()
            .ok_or_else(|| std::io::Error::other("automation is missing or disabled"))?;
        let run = self
            .store
            .engine
            .trigger_repo_event(&automation, repository, event_id, now);
        self.record_trigger(run.clone(), "repo-event")?;
        Ok(run)
    }

    pub fn catch_up(
        &mut self,
        automation_id: &str,
        last_checked: u64,
        now: u64,
    ) -> Result<Vec<AutomationRun>, std::io::Error> {
        let automation = self
            .store
            .automations
            .iter()
            .find(|automation| automation.id == automation_id && automation.enabled)
            .cloned()
            .ok_or_else(|| std::io::Error::other("automation is missing or disabled"))?;
        let times = self
            .store
            .engine
            .catch_up_times(&automation, last_checked, now);
        let mut runs = Vec::new();
        for scheduled_for in times {
            let key = format!("schedule:{}:{scheduled_for}", automation.id);
            let run = self
                .store
                .engine
                .trigger(&automation, scheduled_for, key, now);
            self.record_trigger(run.clone(), "catch-up")?;
            runs.push(run);
        }
        Ok(runs)
    }

    pub fn update_run_status(
        &mut self,
        run_id: &str,
        status: RunStatus,
        now: u64,
        detail: impl Into<String>,
    ) -> Result<bool, std::io::Error> {
        let Some(index) = self
            .store
            .recent_runs
            .iter()
            .position(|run| run.run_id == run_id)
        else {
            return Ok(false);
        };
        let automation_id = self.store.recent_runs[index].automation_id.clone();
        let notification = self
            .store
            .automations
            .iter()
            .find(|automation| automation.id == automation_id)
            .map_or(NotificationPolicy::Actionable, |automation| {
                automation.notification
            });
        let run = &mut self.store.recent_runs[index];
        run.status = status;
        self.store.audit.push(AutomationAuditEvent {
            timestamp: now,
            automation_id: run.automation_id.clone(),
            run_id: Some(run_id.into()),
            kind: "status".into(),
            detail: detail.into(),
        });
        self.store.persist(&self.data_root)?;
        Ok(should_notify(status, notification))
    }

    pub fn next_run_at(&self, automation_id: &str, after: u64) -> Option<u64> {
        let automation = self
            .store
            .automations
            .iter()
            .find(|automation| automation.id == automation_id && automation.enabled)?;
        next_run_after(automation, after)
    }

    fn record_trigger(&mut self, run: AutomationRun, kind: &str) -> Result<(), std::io::Error> {
        self.store.audit.push(AutomationAuditEvent {
            timestamp: run.created_at,
            automation_id: run.automation_id.clone(),
            run_id: Some(run.run_id.clone()),
            kind: kind.into(),
            detail: format!(
                "template={} dedupe={} status={:?}",
                run.template_version, run.dedupe_key, run.status
            ),
        });
        self.store.recent_runs.push(run);
        if self.store.recent_runs.len() > 500 {
            let remove = self.store.recent_runs.len() - 500;
            self.store.recent_runs.drain(..remove);
        }
        self.store.persist(&self.data_root).map(|_| ())
    }
}

pub fn next_run_after(automation: &Automation, after: u64) -> Option<u64> {
    match &automation.trigger {
        Trigger::Interval {
            every_seconds,
            anchor_unix,
        } if *every_seconds > 0 => {
            if after < *anchor_unix {
                Some(*anchor_unix)
            } else {
                anchor_unix.checked_add(
                    (after.saturating_sub(*anchor_unix) / every_seconds + 1) * every_seconds,
                )
            }
        }
        Trigger::LocalSchedule { .. } => {
            // Covers leap years and every DST transition while keeping this query bounded.
            local_schedule_times_for_trigger(automation, after, after.saturating_add(370 * 86_400))
                .into_iter()
                .next()
        }
        Trigger::Manual | Trigger::RepoEvent { .. } | Trigger::Interval { .. } => None,
    }
}

fn local_schedule_times_for_trigger(automation: &Automation, from: u64, to: u64) -> Vec<u64> {
    match &automation.trigger {
        Trigger::LocalSchedule {
            hour,
            minute,
            timezone,
        } => local_schedule_times(*hour, *minute, timezone, from, to),
        _ => Vec::new(),
    }
}
impl AutomationEngine {
    pub fn trigger(
        &mut self,
        automation: &Automation,
        scheduled_for: u64,
        dedupe_key: String,
        now: u64,
    ) -> AutomationRun {
        self.sequence += 1;
        let duplicate = self.seen.contains_key(&dedupe_key);
        if !duplicate {
            self.seen.insert(dedupe_key.clone(), now);
        }
        AutomationRun {
            run_id: format!("run-{}-{}", automation.id, self.sequence),
            task_id: format!("task-{}-{}", automation.id, self.sequence),
            automation_id: automation.id.clone(),
            template_version: automation.template.version,
            template: automation.template.clone(),
            dedupe_key,
            scheduled_for,
            created_at: now,
            status: if duplicate {
                RunStatus::Duplicate
            } else {
                RunStatus::Queued
            },
        }
    }
    pub fn catch_up_times(&self, automation: &Automation, last_checked: u64, now: u64) -> Vec<u64> {
        let missed = match &automation.trigger {
            Trigger::Interval {
                every_seconds,
                anchor_unix,
            } if *every_seconds > 0 && now > last_checked => {
                let first = anchor_unix
                    + ((last_checked.saturating_sub(*anchor_unix) / every_seconds) + 1)
                        * every_seconds;
                std::iter::successors((first <= now).then_some(first), |time| {
                    time.checked_add(*every_seconds).filter(|next| *next <= now)
                })
                .collect::<Vec<_>>()
            }
            Trigger::LocalSchedule {
                hour,
                minute,
                timezone,
            } => local_schedule_times(*hour, *minute, timezone, last_checked, now),
            _ => vec![],
        };
        match automation.catch_up {
            CatchUpPolicy::Skip => vec![],
            CatchUpPolicy::RunOnce => missed.last().copied().into_iter().collect(),
            CatchUpPolicy::RunEach { maximum } => missed
                .into_iter()
                .rev()
                .take(maximum as usize)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
        }
    }

    pub fn repo_event_key(automation_id: &str, repository: &str, event_id: &str) -> String {
        format!("repo:{automation_id}:{repository}:{event_id}")
    }

    pub fn trigger_repo_event(
        &mut self,
        automation: &Automation,
        repository: &str,
        event_id: &str,
        now: u64,
    ) -> AutomationRun {
        let exact = Self::repo_event_key(&automation.id, repository, event_id);
        if self.seen.contains_key(&exact) {
            return self.trigger(automation, now, exact, now);
        }
        let window_key = format!("{}:{repository}", automation.id);
        let coalesce_seconds = match automation.trigger {
            Trigger::RepoEvent {
                coalesce_seconds, ..
            } => coalesce_seconds,
            _ => 0,
        };
        let within_window = self
            .repo_windows
            .get(&window_key)
            .is_some_and(|last| now.saturating_sub(*last) < coalesce_seconds);
        let mut run = self.trigger(automation, now, exact, now);
        if within_window {
            run.status = RunStatus::Coalesced;
        } else {
            self.repo_windows.insert(window_key, now);
        }
        run
    }
}

impl AutomationRun {
    pub fn create_task(
        &self,
        project_dir: impl Into<PathBuf>,
        journal_root: Option<&Path>,
    ) -> Result<crate::TaskRuntime, String> {
        let runtime = crate::TaskRuntime::new(
            crate::TaskConfig::new(
                self.template.prompt.clone(),
                project_dir,
                self.template.backend.clone(),
                self.template.environment_id.clone(),
            ),
            self.template.budgets.clone(),
            crate::ApprovalPolicy::Prompt,
        )
        .with_model(self.template.agent_profile.clone())
        .with_task_id(self.task_id.clone())?;
        match journal_root {
            Some(root) => runtime.try_with_journal(root),
            None => Ok(runtime),
        }
    }
}

fn local_schedule_times(hour: u8, minute: u8, timezone: &str, from: u64, to: u64) -> Vec<u64> {
    if hour > 23 || minute > 59 || to <= from {
        return vec![];
    }
    let Ok(timezone) = timezone.parse::<chrono_tz::Tz>() else {
        return vec![];
    };
    let Some(mut date) = Utc
        .timestamp_opt(from as i64, 0)
        .single()
        .map(|time| time.with_timezone(&timezone).date_naive())
    else {
        return vec![];
    };
    let Some(end) = Utc
        .timestamp_opt(to as i64, 0)
        .single()
        .map(|time| time.with_timezone(&timezone).date_naive())
    else {
        return vec![];
    };
    let mut result = Vec::new();
    while date <= end {
        let local = timezone.with_ymd_and_hms(
            date.year(),
            date.month(),
            date.day(),
            hour as u32,
            minute as u32,
            0,
        );
        let instant = match local {
            chrono::LocalResult::Single(value) => Some(value),
            // During a fall-back overlap, run once at the first occurrence.
            chrono::LocalResult::Ambiguous(first, second) => Some(first.min(second)),
            // A spring-forward gap has no such local time and is skipped.
            chrono::LocalResult::None => None,
        };
        if let Some(timestamp) = instant.map(|value| value.timestamp() as u64) {
            if timestamp > from && timestamp <= to {
                result.push(timestamp);
            }
        }
        let Some(next) = date.succ_opt() else { break };
        date = next;
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    Transient,
    Permanent,
    UnknownEffect,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    Retry,
    Stop,
    WaitForUser,
}
pub fn classify_retry(
    failure: FailureClass,
    idempotent: bool,
    result_known: bool,
) -> RetryDecision {
    match (failure, idempotent, result_known) {
        (FailureClass::Transient, true, true) => RetryDecision::Retry,
        (FailureClass::UnknownEffect, _, _) | (_, _, false) => RetryDecision::WaitForUser,
        _ => RetryDecision::Stop,
    }
}
pub fn should_notify(status: RunStatus, policy: NotificationPolicy) -> bool {
    match policy {
        NotificationPolicy::Muted => false,
        NotificationPolicy::FailuresOnly => {
            matches!(status, RunStatus::Failed | RunStatus::WaitingUser)
        }
        NotificationPolicy::Actionable => matches!(
            status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::WaitingUser
        ),
    }
}
