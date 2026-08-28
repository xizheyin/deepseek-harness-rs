//! Durable Plan Mode state and its process-local next-step transition.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub(crate) const MAX_PLAN_BYTES: usize = 16 * 1024;
const MAX_PLAN_COMMAND_MESSAGE_BYTES: usize = 1_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanModeChange {
    active: bool,
}

impl PlanModeChange {
    #[must_use]
    pub(crate) const fn new(active: bool) -> Self {
        Self { active }
    }

    #[must_use]
    pub(crate) const fn active(&self) -> bool {
        self.active
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PlanModeCommand {
    Enter { message: Option<String> },
    Off,
}

impl PlanModeCommand {
    pub(crate) fn parse(input: &str) -> Option<Result<Self, PlanModeError>> {
        let trimmed = input.trim_matches(char::is_whitespace);
        let suffix = trimmed.strip_prefix("/plan")?;
        if !suffix.is_empty() && !suffix.starts_with(char::is_whitespace) {
            return None;
        }
        let argument = suffix.trim_matches(char::is_whitespace);
        if argument == "off" {
            return Some(Ok(Self::Off));
        }
        if argument.len() > MAX_PLAN_COMMAND_MESSAGE_BYTES {
            return Some(Err(PlanModeError::MessageTooLarge));
        }
        Some(Ok(Self::Enter {
            message: (!argument.is_empty()).then(|| argument.to_owned()),
        }))
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum PlanModeError {
    #[error("Plan Mode state is unavailable")]
    Unavailable,
    #[error("Plan Mode command message exceeds the interactive input limit")]
    MessageTooLarge,
    #[error("exit_plan_mode is only available in Plan Mode")]
    Inactive,
    #[error("Plan Mode changed before the prepared transition was committed")]
    Stale,
}

#[derive(Debug)]
struct PlanModeState {
    active: bool,
    pending: Option<bool>,
}

#[derive(Clone)]
pub(crate) struct PlanModeRuntime {
    state: Arc<Mutex<PlanModeState>>,
}

impl std::fmt::Debug for PlanModeRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PlanModeRuntime").finish()
    }
}

impl PlanModeRuntime {
    #[must_use]
    pub(crate) fn new(active: bool) -> Self {
        Self {
            state: Arc::new(Mutex::new(PlanModeState {
                active,
                pending: None,
            })),
        }
    }

    pub(crate) fn active(&self) -> Result<bool, PlanModeError> {
        self.state
            .lock()
            .map(|state| state.active)
            .map_err(|_| PlanModeError::Unavailable)
    }

    pub(crate) fn prepare_set(
        &self,
        active: bool,
    ) -> Result<Option<PreparedPlanModeMutation>, PlanModeError> {
        let state = self.state.lock().map_err(|_| PlanModeError::Unavailable)?;
        if state.pending.is_some() {
            return Err(PlanModeError::Stale);
        }
        if state.active == active {
            return Ok(None);
        }
        Ok(Some(PreparedPlanModeMutation {
            runtime: self.clone(),
            expected_active: state.active,
            target: active,
            boundary: false,
        }))
    }

    pub(crate) fn prepare_boundary(
        &self,
    ) -> Result<Option<PreparedPlanModeMutation>, PlanModeError> {
        let state = self.state.lock().map_err(|_| PlanModeError::Unavailable)?;
        let Some(target) = state.pending else {
            return Ok(None);
        };
        if target == state.active {
            return Err(PlanModeError::Stale);
        }
        Ok(Some(PreparedPlanModeMutation {
            runtime: self.clone(),
            expected_active: state.active,
            target,
            boundary: true,
        }))
    }

    pub(crate) fn prepare_approved_exit(&self) -> Result<PreparedPlanExit, PlanModeError> {
        let state = self.state.lock().map_err(|_| PlanModeError::Unavailable)?;
        if !state.active {
            return Err(PlanModeError::Inactive);
        }
        if state.pending.is_some() {
            return Err(PlanModeError::Stale);
        }
        Ok(PreparedPlanExit {
            runtime: self.clone(),
        })
    }
}

pub(crate) struct PreparedPlanModeMutation {
    runtime: PlanModeRuntime,
    expected_active: bool,
    target: bool,
    boundary: bool,
}

impl std::fmt::Debug for PreparedPlanModeMutation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedPlanModeMutation")
            .field("target", &self.target)
            .field("boundary", &self.boundary)
            .finish()
    }
}

impl PreparedPlanModeMutation {
    #[must_use]
    pub(crate) const fn change(&self) -> PlanModeChange {
        PlanModeChange::new(self.target)
    }

    pub(crate) fn commit(self) -> Result<(), PlanModeError> {
        let mut state = self
            .runtime
            .state
            .lock()
            .map_err(|_| PlanModeError::Unavailable)?;
        let pending_matches = if self.boundary {
            state.pending == Some(self.target)
        } else {
            state.pending.is_none()
        };
        if state.active != self.expected_active || !pending_matches {
            return Err(PlanModeError::Stale);
        }
        state.active = self.target;
        if self.boundary {
            state.pending = None;
        }
        Ok(())
    }
}

pub struct PreparedPlanExit {
    runtime: PlanModeRuntime,
}

impl std::fmt::Debug for PreparedPlanExit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PreparedPlanExit").finish()
    }
}

impl PreparedPlanExit {
    pub(crate) fn commit(self) -> Result<(), PlanModeError> {
        let mut state = self
            .runtime
            .state
            .lock()
            .map_err(|_| PlanModeError::Unavailable)?;
        if !state.active || state.pending.is_some() {
            return Err(PlanModeError::Stale);
        }
        state.pending = Some(false);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::session::{EventKind, NewEvent, Session};

    use super::{PlanModeChange, PlanModeCommand, PlanModeRuntime};

    #[test]
    fn command_parser_reserves_only_the_exact_plan_prefix() {
        assert_eq!(
            PlanModeCommand::parse("/plan"),
            Some(Ok(PlanModeCommand::Enter { message: None }))
        );
        assert_eq!(
            PlanModeCommand::parse(" /plan inspect first "),
            Some(Ok(PlanModeCommand::Enter {
                message: Some("inspect first".to_owned())
            }))
        );
        assert_eq!(
            PlanModeCommand::parse("/plan off"),
            Some(Ok(PlanModeCommand::Off))
        );
        assert_eq!(PlanModeCommand::parse("/planner"), None);
    }

    #[test]
    fn approved_exit_is_pending_until_the_boundary_commits() {
        let runtime = PlanModeRuntime::new(true);
        runtime.prepare_approved_exit().unwrap().commit().unwrap();
        assert!(runtime.active().unwrap());
        let mutation = runtime.prepare_boundary().unwrap().unwrap();
        assert!(!mutation.change().active());
        mutation.commit().unwrap();
        assert!(!runtime.active().unwrap());
        assert!(runtime.prepare_boundary().unwrap().is_none());
    }

    #[test]
    fn setting_the_current_mode_is_idempotent() {
        assert!(
            PlanModeRuntime::new(false)
                .prepare_set(false)
                .unwrap()
                .is_none()
        );
        assert!(
            PlanModeRuntime::new(true)
                .prepare_set(true)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn session_projection_restores_the_last_recorded_mode() {
        let mut session = Session::new("plan-mode-replay").unwrap();
        session
            .append(NewEvent::log(EventKind::plan_mode(PlanModeChange::new(
                true,
            ))))
            .unwrap();
        assert!(session.state().plan_mode_active());
        session
            .append(NewEvent::log(EventKind::plan_mode(PlanModeChange::new(
                false,
            ))))
            .unwrap();
        assert!(!session.state().plan_mode_active());

        let snapshot = session.to_json().unwrap();
        let restored = Session::from_json(&snapshot, crate::session::SystemClock).unwrap();
        assert!(!restored.state().plan_mode_active());
    }
}
