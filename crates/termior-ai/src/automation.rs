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
    /// Why this row exists: manual, catch-up, missed, coalesced, duplicate, retry, repo-event.
    #[serde(default)]
    pub schedule_reason: String,
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
    /// Trigger that created the run (`manual`, `schedule`, `repo-event`, `retry`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    /// Why the scheduler created, skipped, or coalesced this occurrence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_reason: Option<String>,
    /// `retry`, `stop`, or `wait-for-user`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_decision: Option<String>,
    /// `waiting` while a person must decide. Never `approved` from the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<String>,
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
    /// A scheduled occurrence the catch-up policy chose not to run.
    Missed,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationEngine {
    seen: BTreeMap<String, u64>,
    repo_windows: BTreeMap<String, u64>,
    sequence: u64,
    /// Last schedule poll per automation. Absent means the host has not observed it yet.
    #[serde(default)]
    last_checked: BTreeMap<String, u64>,
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
        self.record_trigger(run.clone(), "manual", "manual")?;
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
        let reason = match run.status {
            RunStatus::Duplicate => "duplicate",
            RunStatus::Coalesced => "coalesced",
            _ => "repo-event",
        };
        self.record_trigger(run.clone(), "repo-event", reason)?;
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
        let missed = self
            .store
            .engine
            .missed_instants(&automation, last_checked, now);
        let selected = self
            .store
            .engine
            .catch_up_times(&automation, last_checked, now);
        let selected_set = selected
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let mut runs = Vec::new();
        for scheduled_for in missed {
            let key = format!("schedule:{}:{scheduled_for}", automation.id);
            let mut run = self
                .store
                .engine
                .trigger(&automation, scheduled_for, key, now);
            let (status, reason) = if run.status == RunStatus::Duplicate {
                (RunStatus::Duplicate, "duplicate")
            } else if selected_set.contains(&scheduled_for) {
                (RunStatus::Queued, "catch-up")
            } else if matches!(automation.catch_up, CatchUpPolicy::Skip) {
                (RunStatus::Missed, "missed")
            } else {
                (RunStatus::Coalesced, "coalesced")
            };
            run.status = status;
            self.record_trigger(run.clone(), "schedule", reason)?;
            if status == RunStatus::Queued {
                runs.push(run);
            }
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
        let automation_id = run.automation_id.clone();
        let template_version = run.template_version;
        let dedupe_key = run.dedupe_key.clone();
        let schedule_reason = run.schedule_reason.clone();
        self.push_audit(AutomationAuditEvent {
            timestamp: now,
            automation_id,
            run_id: Some(run_id.into()),
            kind: "status".into(),
            detail: detail.into(),
            trigger: None,
            template_version: Some(template_version),
            dedupe_key: Some(dedupe_key),
            schedule_reason: (!schedule_reason.is_empty()).then_some(schedule_reason),
            retry_decision: None,
            approval: None,
            merge: None,
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

    fn record_trigger(
        &mut self,
        mut run: AutomationRun,
        trigger: &str,
        schedule_reason: &str,
    ) -> Result<(), std::io::Error> {
        run.schedule_reason = schedule_reason.to_owned();
        self.push_run_audit(&run, trigger, schedule_reason);
        self.store.recent_runs.push(run);
        if self.store.recent_runs.len() > 500 {
            let remove = self.store.recent_runs.len() - 500;
            self.store.recent_runs.drain(..remove);
        }
        self.store.persist(&self.data_root).map(|_| ())
    }

    fn push_run_audit(&mut self, run: &AutomationRun, trigger: &str, schedule_reason: &str) {
        self.push_audit(AutomationAuditEvent {
            timestamp: run.created_at,
            automation_id: run.automation_id.clone(),
            run_id: Some(run.run_id.clone()),
            kind: trigger.into(),
            detail: format!(
                "trigger={trigger} template={} dedupe={} status={:?} schedule={schedule_reason}",
                run.template_version, run.dedupe_key, run.status
            ),
            trigger: Some(trigger.into()),
            template_version: Some(run.template_version),
            dedupe_key: Some(run.dedupe_key.clone()),
            schedule_reason: Some(schedule_reason.into()),
            retry_decision: None,
            approval: None,
            merge: None,
        });
    }

    fn push_audit(&mut self, event: AutomationAuditEvent) {
        self.store.audit.push(event);
        if self.store.audit.len() > 2_000 {
            let remove = self.store.audit.len() - 2_000;
            self.store.audit.drain(..remove);
        }
    }

    /// While the app is running, create due schedule rows from the last poll.
    /// The first observation anchors at `now` and does not backfill history.
    pub fn poll_due(&mut self, now: u64) -> Result<Vec<AutomationRun>, std::io::Error> {
        let ids = self
            .store
            .automations
            .iter()
            .filter(|automation| {
                automation.enabled
                    && matches!(
                        automation.trigger,
                        Trigger::Interval { .. } | Trigger::LocalSchedule { .. }
                    )
            })
            .map(|automation| automation.id.clone())
            .collect::<Vec<_>>();
        let mut created = Vec::new();
        let mut dirty = false;
        for id in ids {
            let Some(last) = self.store.engine.last_checked.get(&id).copied() else {
                self.store.engine.last_checked.insert(id, now);
                dirty = true;
                continue;
            };
            if now > last {
                created.extend(self.catch_up(&id, last, now)?);
                self.store.engine.last_checked.insert(id, now);
                dirty = true;
            }
        }
        if dirty {
            self.store.persist(&self.data_root)?;
        }
        Ok(created)
    }

    /// Runs left in `Running` by a previous process wait for a person and are not replayed.
    pub fn recover_interrupted(&mut self, now: u64) -> Result<Vec<RunEffect>, std::io::Error> {
        let ids = self
            .store
            .recent_runs
            .iter()
            .filter(|run| run.status == RunStatus::Running)
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        let mut effects = Vec::new();
        for run_id in ids {
            if let Some(run) = self
                .store
                .recent_runs
                .iter_mut()
                .find(|run| run.run_id == run_id)
            {
                run.schedule_reason = "interrupted".into();
            }
            effects.push(self.finish_run(
                &run_id,
                now,
                RunFinish {
                    status: RunStatus::WaitingUser,
                    kind: "interrupted",
                    detail: "host restarted while the run was in progress; side effects were not replayed".into(),
                    retry_decision: Some("wait-for-user"),
                    approval: None,
                    merge: None,
                },
            )?);
        }
        Ok(effects)
    }

    /// Claim queued runs that fit the global cap, each template's concurrency, and the disk budget.
    pub fn claim_queued(
        &mut self,
        now: u64,
        limits: HostLimits,
    ) -> Result<Vec<AutomationRun>, std::io::Error> {
        let mut order = self
            .store
            .recent_runs
            .iter()
            .enumerate()
            .filter(|(_, run)| run.status == RunStatus::Queued)
            .map(|(index, run)| (run.created_at, run.run_id.clone(), index))
            .collect::<Vec<_>>();
        order.sort();
        let mut running = self
            .store
            .recent_runs
            .iter()
            .filter(|run| run.status == RunStatus::Running)
            .count();
        let mut claimed = Vec::new();
        let disk_blocked =
            limits.disk_budget_bytes > 0 && limits.disk_used_bytes >= limits.disk_budget_bytes;
        for (_, _, index) in order {
            let run = self.store.recent_runs[index].clone();
            let reason = if disk_blocked {
                Some("disk-budget")
            } else if run.template.budgets.max_model_steps == 0
                || run.template.budgets.max_wall_clock_ms == 0
            {
                Some("runtime-budget")
            } else if limits.max_concurrency == 0 || running >= usize::from(limits.max_concurrency)
            {
                Some("concurrency")
            } else {
                let own = self
                    .store
                    .recent_runs
                    .iter()
                    .filter(|other| {
                        other.automation_id == run.automation_id
                            && other.status == RunStatus::Running
                    })
                    .count();
                if own >= usize::from(run.template.max_concurrency) {
                    Some("concurrency")
                } else {
                    None
                }
            };
            if let Some(reason) = reason {
                self.note_deferral(&run, now, reason)?;
                continue;
            }
            self.store.recent_runs[index].status = RunStatus::Running;
            running += 1;
            let claimed_run = self.store.recent_runs[index].clone();
            self.push_audit(audit_from_run(
                &claimed_run,
                now,
                AuditParts {
                    kind: "claim",
                    detail: format!(
                        "claimed under concurrency={} disk={}/{}",
                        limits.max_concurrency, limits.disk_used_bytes, limits.disk_budget_bytes
                    ),
                    retry_decision: None,
                    schedule_reason: None,
                    approval: None,
                    merge: None,
                },
            ));
            claimed.push(claimed_run);
        }
        if !claimed.is_empty() {
            self.store.persist(&self.data_root)?;
        }
        Ok(claimed)
    }

    /// Apply an executor result. Approval waits are recorded and never approved.
    pub fn complete_run(
        &mut self,
        run_id: &str,
        outcome: ExecutionOutcome,
        now: u64,
    ) -> Result<RunEffect, std::io::Error> {
        match outcome {
            ExecutionOutcome::Completed { detail } => self.finish_run(
                run_id,
                now,
                RunFinish {
                    status: RunStatus::Completed,
                    kind: "completed",
                    detail,
                    retry_decision: None,
                    approval: None,
                    merge: None,
                },
            ),
            ExecutionOutcome::Merged { detail } => self.finish_run(
                run_id,
                now,
                RunFinish {
                    status: RunStatus::Completed,
                    kind: "merge",
                    detail: detail.clone(),
                    retry_decision: None,
                    approval: None,
                    merge: Some(detail),
                },
            ),
            ExecutionOutcome::NeedsApproval { detail } => self.finish_run(
                run_id,
                now,
                RunFinish {
                    status: RunStatus::WaitingUser,
                    kind: "approval",
                    detail,
                    retry_decision: None,
                    approval: Some("waiting".into()),
                    merge: None,
                },
            ),
            ExecutionOutcome::Failed {
                class,
                idempotent,
                result_known,
                detail,
            } => {
                let decision = classify_retry(class, idempotent, result_known);
                let (status, retry_decision) = match decision {
                    RetryDecision::Retry => (RunStatus::Failed, Some("retry")),
                    RetryDecision::Stop => (RunStatus::Failed, Some("stop")),
                    RetryDecision::WaitForUser => (RunStatus::WaitingUser, Some("wait-for-user")),
                };
                let effect = self.finish_run(
                    run_id,
                    now,
                    RunFinish {
                        status,
                        kind: "retry",
                        detail,
                        retry_decision,
                        approval: None,
                        merge: None,
                    },
                )?;
                if decision == RetryDecision::Retry {
                    self.enqueue_retry(run_id, now)?;
                }
                Ok(effect)
            }
        }
    }

    /// Poll due schedules, then claim and execute up to the concurrency and disk budget.
    pub fn tick<E: RunExecutor>(
        &mut self,
        now: u64,
        limits: HostLimits,
        executor: &mut E,
    ) -> Result<Vec<RunEffect>, std::io::Error> {
        self.poll_due(now)?;
        let claimed = self.claim_queued(now, limits)?;
        let mut effects = Vec::with_capacity(claimed.len());
        for run in claimed {
            let outcome = executor.execute(&run);
            effects.push(self.complete_run(&run.run_id, outcome, now)?);
        }
        Ok(effects)
    }

    fn enqueue_retry(&mut self, run_id: &str, now: u64) -> Result<(), std::io::Error> {
        let Some(failed) = self
            .store
            .recent_runs
            .iter()
            .find(|run| run.run_id == run_id)
            .cloned()
        else {
            return Ok(());
        };
        let automation = self
            .store
            .automations
            .iter()
            .find(|automation| automation.id == failed.automation_id)
            .cloned();
        let Some(automation) = automation else {
            return Ok(());
        };
        let key = format!("retry:{}:{now}", failed.run_id);
        let mut retry = self.store.engine.trigger(&automation, now, key, now);
        retry.template = failed.template.clone();
        retry.template_version = failed.template_version;
        if retry.status != RunStatus::Duplicate {
            retry.status = RunStatus::Queued;
        }
        self.record_trigger(retry, "retry", "retry")?;
        Ok(())
    }

    fn note_deferral(
        &mut self,
        run: &AutomationRun,
        now: u64,
        reason: &str,
    ) -> Result<(), std::io::Error> {
        let key = format!("defer:{}:{reason}", run.run_id);
        if self.store.engine.seen.contains_key(&key) {
            return Ok(());
        }
        self.store.engine.seen.insert(key, now);
        self.push_audit(audit_from_run(
            run,
            now,
            AuditParts {
                kind: "deferred",
                detail: format!("deferred: {reason}"),
                retry_decision: None,
                schedule_reason: Some(reason),
                approval: None,
                merge: None,
            },
        ));
        self.store.persist(&self.data_root)?;
        Ok(())
    }

    fn finish_run(
        &mut self,
        run_id: &str,
        now: u64,
        finish: RunFinish,
    ) -> Result<RunEffect, std::io::Error> {
        let Some(index) = self
            .store
            .recent_runs
            .iter()
            .position(|run| run.run_id == run_id)
        else {
            return Err(std::io::Error::other(format!("run {run_id} is missing")));
        };
        if finish.approval.as_deref() == Some("approved") {
            return Err(std::io::Error::other(
                "automation host cannot record an automatic approval",
            ));
        }
        let automation_id = self.store.recent_runs[index].automation_id.clone();
        let notification = self
            .store
            .automations
            .iter()
            .find(|automation| automation.id == automation_id)
            .map_or(NotificationPolicy::Actionable, |automation| {
                automation.notification
            });
        let status = finish.status;
        self.store.recent_runs[index].status = status;
        let run = self.store.recent_runs[index].clone();
        let approval_wait = finish.approval.as_deref() == Some("waiting");
        self.push_audit(audit_from_run(
            &run,
            now,
            AuditParts {
                kind: finish.kind,
                detail: finish.detail,
                retry_decision: finish.retry_decision,
                schedule_reason: None,
                approval: finish.approval,
                merge: finish.merge,
            },
        ));
        self.store.persist(&self.data_root)?;
        Ok(RunEffect {
            run_id: run.run_id,
            automation_id: run.automation_id,
            status,
            notify: should_notify(status, notification),
            auto_approved: false,
            approval_wait,
        })
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
            schedule_reason: String::new(),
        }
    }

    /// Every schedule instant in `(last_checked, now]`, before catch-up policy.
    pub fn missed_instants(
        &self,
        automation: &Automation,
        last_checked: u64,
        now: u64,
    ) -> Vec<u64> {
        match &automation.trigger {
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
                .collect()
            }
            Trigger::LocalSchedule {
                hour,
                minute,
                timezone,
            } => local_schedule_times(*hour, *minute, timezone, last_checked, now),
            _ => Vec::new(),
        }
    }

    pub fn catch_up_times(&self, automation: &Automation, last_checked: u64, now: u64) -> Vec<u64> {
        let missed = self.missed_instants(automation, last_checked, now);
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

/// Global caps the host loop applies while the app is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostLimits {
    pub max_concurrency: u16,
    /// `0` disables the disk gate. A positive budget blocks new claims once usage reaches it.
    pub disk_budget_bytes: u64,
    pub disk_used_bytes: u64,
}

/// Result of driving one claimed run. There is no approved variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionOutcome {
    Completed {
        detail: String,
    },
    Failed {
        class: FailureClass,
        idempotent: bool,
        result_known: bool,
        detail: String,
    },
    NeedsApproval {
        detail: String,
    },
    Merged {
        detail: String,
    },
}

/// What the UI should do after a run leaves the executor. `auto_approved` is always false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEffect {
    pub run_id: String,
    pub automation_id: String,
    pub status: RunStatus,
    pub notify: bool,
    pub auto_approved: bool,
    /// True only when the run stopped for a person to approve a tool or change.
    pub approval_wait: bool,
}

/// How an approval wait is shown. Automation never opens a modal and never approves itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalPresentation {
    pub open_modal: bool,
    pub auto_approve: bool,
    pub notify: bool,
}

pub fn approval_presentation(policy: NotificationPolicy) -> ApprovalPresentation {
    ApprovalPresentation {
        open_modal: false,
        auto_approve: false,
        notify: should_notify(RunStatus::WaitingUser, policy),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationAdminSummary {
    pub next_run_at: Option<u64>,
    pub last_runs: Vec<AutomationRun>,
    pub missed: u64,
    pub coalesced: u64,
    pub duplicate: u64,
}

pub fn admin_summary(
    store: &AutomationStore,
    automation_id: &str,
    now: u64,
) -> Option<AutomationAdminSummary> {
    let automation = store
        .automations
        .iter()
        .find(|automation| automation.id == automation_id)?;
    let matched = store
        .recent_runs
        .iter()
        .filter(|run| run.automation_id == automation_id)
        .cloned()
        .collect::<Vec<_>>();
    let count =
        |status: RunStatus| matched.iter().filter(|run| run.status == status).count() as u64;
    let missed = count(RunStatus::Missed);
    let coalesced = count(RunStatus::Coalesced);
    let duplicate = count(RunStatus::Duplicate);
    let mut last_runs = matched;
    last_runs.reverse();
    last_runs.truncate(5);
    let next_run_at = if automation.enabled {
        next_run_after(automation, now)
    } else {
        None
    };
    Some(AutomationAdminSummary {
        next_run_at,
        last_runs,
        missed,
        coalesced,
        duplicate,
    })
}

pub trait RunExecutor {
    fn execute(&mut self, run: &AutomationRun) -> ExecutionOutcome;
}

struct RunFinish {
    status: RunStatus,
    kind: &'static str,
    detail: String,
    retry_decision: Option<&'static str>,
    approval: Option<String>,
    merge: Option<String>,
}

struct AuditParts<'a> {
    kind: &'a str,
    detail: String,
    retry_decision: Option<&'a str>,
    schedule_reason: Option<&'a str>,
    approval: Option<String>,
    merge: Option<String>,
}

fn audit_from_run(run: &AutomationRun, now: u64, parts: AuditParts<'_>) -> AutomationAuditEvent {
    let schedule_reason = parts
        .schedule_reason
        .map(str::to_owned)
        .or_else(|| (!run.schedule_reason.is_empty()).then(|| run.schedule_reason.clone()));
    AutomationAuditEvent {
        timestamp: now,
        automation_id: run.automation_id.clone(),
        run_id: Some(run.run_id.clone()),
        kind: parts.kind.into(),
        detail: parts.detail,
        trigger: (!run.schedule_reason.is_empty()).then(|| run.schedule_reason.clone()),
        template_version: Some(run.template_version),
        dedupe_key: Some(run.dedupe_key.clone()),
        schedule_reason,
        retry_decision: parts.retry_decision.map(str::to_owned),
        approval: parts.approval,
        merge: parts.merge,
    }
}

/// Drive a claimed run with [`crate::ApprovalPolicy::Prompt`].
/// A tool that needs approval stops the run; this function never resolves that approval.
pub async fn drive_automation_run(
    run: &AutomationRun,
    project_dir: &Path,
    journal_root: Option<&Path>,
    provider: &dyn crate::Provider,
    executor: &dyn crate::RuntimeToolExecutor,
) -> ExecutionOutcome {
    let mut task = match run.create_task(project_dir, journal_root) {
        Ok(task) => task,
        Err(error) => {
            return ExecutionOutcome::Failed {
                class: FailureClass::Permanent,
                idempotent: true,
                result_known: true,
                detail: error,
            };
        }
    };
    task.set_approval_policy(crate::ApprovalPolicy::Prompt);
    if approval_is_waiting(&task) {
        persist_automation_task(&task, journal_root);
        return ExecutionOutcome::NeedsApproval {
            detail: "run is already waiting for approval".into(),
        };
    }
    if task.task().state != crate::TaskState::Idle {
        persist_automation_task(&task, journal_root);
        return ExecutionOutcome::Failed {
            class: FailureClass::UnknownEffect,
            idempotent: false,
            result_known: false,
            detail: format!(
                "task is {:?}; the host did not replay it",
                task.task().state
            ),
        };
    }
    let prompt = run.template.prompt.clone();
    let driven = task
        .handle(
            crate::TaskCommand::Start { user_input: prompt },
            provider,
            executor,
            &mut |_| {},
        )
        .await;
    persist_automation_task(&task, journal_root);
    if approval_is_waiting(&task) || approved_by_yolo(&task) {
        return ExecutionOutcome::NeedsApproval {
            detail: task
                .pending_approval()
                .map(|request| request.summary)
                .unwrap_or_else(|| "waiting for approval".into()),
        };
    }
    match driven {
        Ok(()) => match task.task().state {
            crate::TaskState::CompletedVerified | crate::TaskState::CompletedUnverified => {
                ExecutionOutcome::Completed {
                    detail: format!("{:?}", task.task().state),
                }
            }
            crate::TaskState::Failed => ExecutionOutcome::Failed {
                class: FailureClass::Permanent,
                idempotent: true,
                result_known: true,
                detail: "task failed".into(),
            },
            crate::TaskState::WaitingUser => ExecutionOutcome::Failed {
                class: FailureClass::UnknownEffect,
                idempotent: false,
                result_known: false,
                detail: "task is waiting for the user".into(),
            },
            other => ExecutionOutcome::Failed {
                class: FailureClass::UnknownEffect,
                idempotent: false,
                result_known: false,
                detail: format!("task stopped in {other:?}"),
            },
        },
        Err(error) => ExecutionOutcome::Failed {
            class: FailureClass::Transient,
            idempotent: true,
            result_known: true,
            detail: error.to_string(),
        },
    }
}

fn approval_is_waiting(task: &crate::TaskRuntime) -> bool {
    matches!(
        task.task().state,
        crate::TaskState::WaitingApproval | crate::TaskState::WaitingChangeReview
    ) || task.invocations().iter().any(|invocation| {
        matches!(
            invocation.state,
            crate::ToolState::AwaitingApproval | crate::ToolState::AwaitingChangeReview
        )
    })
}

fn approved_by_yolo(task: &crate::TaskRuntime) -> bool {
    task.invocations().iter().any(|invocation| {
        invocation.decision.as_ref().is_some_and(|decision| {
            decision.approved && decision.source == crate::DecisionSource::Yolo
        })
    })
}

fn persist_automation_task(task: &crate::TaskRuntime, journal_root: Option<&Path>) {
    if let Some(root) = journal_root {
        if let Err(error) = task.persist_journal(root) {
            log::warn!("automation journal persist failed: {error}");
        }
    }
}
