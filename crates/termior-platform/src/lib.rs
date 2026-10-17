//! Small platform boundary: notification routing, native notification commands and external URLs.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), warn(clippy::unwrap_used, clippy::expect_used))]

mod notification;
mod open;
pub mod update;

pub use notification::{
    AgentIndicator, AgentStatus, NativeNotifier, Notification, NotificationContext,
    NotificationDecision, NotificationRouter, NotificationTarget, SystemNotifier,
};
pub use open::{open_external, open_local_file, PlatformError};
