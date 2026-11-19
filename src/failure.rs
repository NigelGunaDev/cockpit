//! Typed refusals of the presentation API. Clients switch on `code`, never on
//! message text. Interactive paths keep plain strings and convert at the edge.
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    /// The request itself is malformed: missing or unknown arguments.
    InvalidRequest,
    /// No usable Cockpit manifest at the path: missing, unreadable or not private.
    RunUnavailable,
    /// The manifest names a supervisor that is no longer running.
    HostStale,
    /// The target key no longer identifies a live terminal on the original server.
    TargetStale,
    /// The target has no Cockpit view; open it first.
    ViewMissing,
    /// Every tile slot is occupied.
    Capacity,
    /// The original session uses Cockpit's leader key as its own prefix.
    LeaderConflict,
    /// The view is visible or selected; only the user closes it.
    ViewVisible,
    /// The supervisor did not answer in time.
    Timeout,
    /// Anything else; the message explains it.
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Failure {
    pub code: Code,
    pub message: String,
}

impl Failure {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::new(Code::Failed, message)
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::new(Code::Failed, message)
    }
}

impl From<Failure> for String {
    fn from(failure: Failure) -> Self {
        failure.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialize_as_snake_case_and_plain_errors_are_failed() {
        let failure = Failure::new(Code::HostStale, "gone");
        assert_eq!(
            serde_json::to_value(&failure).unwrap(),
            serde_json::json!({"code": "host_stale", "message": "gone"})
        );
        assert_eq!(Failure::from("plain").code, Code::Failed);
        assert_eq!(String::from(failure), "gone");
    }
}
