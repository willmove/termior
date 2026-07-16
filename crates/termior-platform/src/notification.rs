use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Started,
    Working,
    Attention,
    Finished,
    Exited,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIndicator {
    pub id: String,
    pub title: String,
    pub status: AgentStatus,
    pub tab_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub target_tab: Option<u64>,
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationDecision {
    Suppress,
    InAppToast,
    System,
}

pub trait SystemNotifier: Send + Sync {
    fn notify(&self, notification: &Notification) -> Result<(), String>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NativeNotifier;

impl SystemNotifier for NativeNotifier {
    fn notify(&self, notification: &Notification) -> Result<(), String> {
        #[cfg(target_os = "windows")]
        let status = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!(
                "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType=WindowsRuntime] > $null; Add-Type -AssemblyName System.Runtime.WindowsRuntime; $x=[Windows.Data.Xml.Dom.XmlDocument]::new(); $x.LoadXml('<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>'); [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('Termior').Show([Windows.UI.Notifications.ToastNotification]::new($x))",
                xml_escape(&notification.title),
                xml_escape(&notification.body)
            ))
            .status();

        #[cfg(target_os = "macos")]
        let status = Command::new("osascript")
            .args([
                "-e",
                &format!(
                    "display notification {:?} with title {:?}",
                    notification.body, notification.title
                ),
            ])
            .status();

        #[cfg(all(unix, not(target_os = "macos")))]
        let status = Command::new("notify-send")
            .args([&notification.title, &notification.body])
            .status();

        status
            .map_err(|error| error.to_string())?
            .success()
            .then_some(())
            .ok_or_else(|| "native notification command failed".to_owned())
    }
}

#[derive(Debug, Clone)]
pub struct NotificationRouter {
    enabled: bool,
    agents: BTreeMap<String, AgentIndicator>,
}

impl Default for NotificationRouter {
    fn default() -> Self {
        Self {
            enabled: true,
            agents: BTreeMap::new(),
        }
    }
}

impl NotificationRouter {
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn route(
        &self,
        notification: &Notification,
        window_focused: bool,
        active_tab: Option<u64>,
    ) -> NotificationDecision {
        if !self.enabled {
            return NotificationDecision::Suppress;
        }
        if window_focused {
            if notification.target_tab.is_none() || notification.target_tab == active_tab {
                NotificationDecision::Suppress
            } else {
                NotificationDecision::InAppToast
            }
        } else {
            NotificationDecision::System
        }
    }

    pub fn update_agent(&mut self, indicator: AgentIndicator) {
        self.agents.insert(indicator.id.clone(), indicator);
    }

    pub fn remove_agent(&mut self, id: &str) {
        self.agents.remove(id);
    }

    pub fn bell_items(&self) -> Vec<&AgentIndicator> {
        self.agents.values().collect()
    }
}

#[cfg(target_os = "windows")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(tab: Option<u64>) -> Notification {
        Notification {
            title: "Agent".into(),
            body: "Done".into(),
            target_tab: tab,
            status: AgentStatus::Finished,
        }
    }

    #[test]
    fn routing_matches_visibility_matrix() {
        let router = NotificationRouter::default();
        assert_eq!(
            router.route(&notice(Some(1)), true, Some(1)),
            NotificationDecision::Suppress
        );
        assert_eq!(
            router.route(&notice(Some(2)), true, Some(1)),
            NotificationDecision::InAppToast
        );
        assert_eq!(
            router.route(&notice(Some(1)), false, Some(1)),
            NotificationDecision::System
        );
    }

    #[test]
    fn bell_tracks_both_agent_kinds() {
        let mut router = NotificationRouter::default();
        router.update_agent(AgentIndicator {
            id: "builtin".into(),
            title: "Composer".into(),
            status: AgentStatus::Working,
            tab_id: None,
        });
        router.update_agent(AgentIndicator {
            id: "terminal-1".into(),
            title: "Claude Code".into(),
            status: AgentStatus::Attention,
            tab_id: Some(1),
        });
        assert_eq!(router.bell_items().len(), 2);
    }
}
