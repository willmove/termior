//! Small platform boundary: notification routing, native notification commands and external URLs.

#![forbid(unsafe_code)]

mod notification;
mod open;

pub use notification::{
    AgentIndicator, AgentStatus, NativeNotifier, Notification, NotificationDecision,
    NotificationRouter, SystemNotifier,
};
pub use open::{open_external, PlatformError};
