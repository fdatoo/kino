//! Durable transcode job state machine.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Durable lifecycle state for a row in `transcode_jobs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JobState {
    /// Job is eligible for scheduler dispatch.
    Planned,
    /// Runner is actively executing the encode pipeline.
    Running,
    /// Encode output is being verified before publication.
    Verifying,
    /// Job completed successfully and will not run again.
    Completed,
    /// Job exhausted or recorded a non-retryable failure.
    Failed,
    /// Job was cancelled by an operator or caller.
    Cancelled,
}

impl JobState {
    /// Return the stable kebab-case identifier used for durable storage.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Running => "running",
            Self::Verifying => "verifying",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Return whether the state rejects all future transitions.
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for JobState {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "planned" => Ok(Self::Planned),
            "running" => Ok(Self::Running),
            "verifying" => Ok(Self::Verifying),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(Error::InvalidJobState(other.to_owned())),
        }
    }
}

/// Validate a durable job state transition and return the accepted target state.
pub const fn try_transition(from: JobState, to: JobState) -> Result<JobState> {
    match (from, to) {
        (JobState::Planned, JobState::Running)
        | (JobState::Running, JobState::Verifying | JobState::Failed | JobState::Cancelled)
        | (JobState::Verifying, JobState::Completed | JobState::Failed) => Ok(to),
        _ => Err(Error::InvalidTransition { from, to }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATES: [JobState; 6] = [
        JobState::Planned,
        JobState::Running,
        JobState::Verifying,
        JobState::Completed,
        JobState::Failed,
        JobState::Cancelled,
    ];

    const TRANSITIONS: [(JobState, JobState, bool); 36] = [
        (JobState::Planned, JobState::Planned, false),
        (JobState::Planned, JobState::Running, true),
        (JobState::Planned, JobState::Verifying, false),
        (JobState::Planned, JobState::Completed, false),
        (JobState::Planned, JobState::Failed, false),
        (JobState::Planned, JobState::Cancelled, false),
        (JobState::Running, JobState::Planned, false),
        (JobState::Running, JobState::Running, false),
        (JobState::Running, JobState::Verifying, true),
        (JobState::Running, JobState::Completed, false),
        (JobState::Running, JobState::Failed, true),
        (JobState::Running, JobState::Cancelled, true),
        (JobState::Verifying, JobState::Planned, false),
        (JobState::Verifying, JobState::Running, false),
        (JobState::Verifying, JobState::Verifying, false),
        (JobState::Verifying, JobState::Completed, true),
        (JobState::Verifying, JobState::Failed, true),
        (JobState::Verifying, JobState::Cancelled, false),
        (JobState::Completed, JobState::Planned, false),
        (JobState::Completed, JobState::Running, false),
        (JobState::Completed, JobState::Verifying, false),
        (JobState::Completed, JobState::Completed, false),
        (JobState::Completed, JobState::Failed, false),
        (JobState::Completed, JobState::Cancelled, false),
        (JobState::Failed, JobState::Planned, false),
        (JobState::Failed, JobState::Running, false),
        (JobState::Failed, JobState::Verifying, false),
        (JobState::Failed, JobState::Completed, false),
        (JobState::Failed, JobState::Failed, false),
        (JobState::Failed, JobState::Cancelled, false),
        (JobState::Cancelled, JobState::Planned, false),
        (JobState::Cancelled, JobState::Running, false),
        (JobState::Cancelled, JobState::Verifying, false),
        (JobState::Cancelled, JobState::Completed, false),
        (JobState::Cancelled, JobState::Failed, false),
        (JobState::Cancelled, JobState::Cancelled, false),
    ];

    #[test]
    fn job_state_round_trips_through_string_id() -> crate::Result<()> {
        for state in STATES {
            assert_eq!(state.as_str().parse::<JobState>()?, state);
        }

        assert!(matches!(
            "unknown".parse::<JobState>(),
            Err(Error::InvalidJobState(value)) if value == "unknown"
        ));

        Ok(())
    }

    #[test]
    fn job_state_reports_terminal_states() {
        for state in STATES {
            assert_eq!(
                state.is_terminal(),
                matches!(
                    state,
                    JobState::Completed | JobState::Failed | JobState::Cancelled
                )
            );
        }
    }

    #[test]
    fn job_state_transition_rules_cover_every_pair() {
        for (from, to, allowed) in TRANSITIONS {
            let result = try_transition(from, to);

            if allowed {
                assert!(matches!(result, Ok(state) if state == to));
            } else {
                assert!(matches!(
                    result,
                    Err(Error::InvalidTransition {
                        from: error_from,
                        to: error_to,
                    }) if error_from == from && error_to == to
                ));
            }
        }
    }
}
