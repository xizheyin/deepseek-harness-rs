//! Bounded process-local Goal state and automatic-round prompts.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use thiserror::Error;

pub(crate) const MAX_GOAL_OBJECTIVE_BYTES: usize = 4 * 1024;
pub(crate) const MAX_GOAL_ROUNDS: u32 = 32;
const MIN_BLOCKED_ROUNDS: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GoalPhase {
    Active,
    Paused,
    Blocked,
    Complete,
}

impl GoalPhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::Complete => "complete",
        }
    }

    const fn is_unfinished(self) -> bool {
        matches!(self, Self::Active | Self::Paused)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoalSnapshot {
    objective: String,
    revision: u64,
    phase: GoalPhase,
    armed: bool,
    rounds: u32,
    blocked_rounds: u32,
}

impl GoalSnapshot {
    pub(crate) fn to_value(&self) -> Value {
        json!({
            "activation": if self.armed { "armed" } else { "disarmed" },
            "blockedRounds": self.blocked_rounds,
            "maxRounds": MAX_GOAL_ROUNDS,
            "objective": self.objective,
            "phase": self.phase.as_str(),
            "revision": self.revision,
            "rounds": self.rounds,
        })
    }

    fn notice(&self) -> String {
        format!(
            "Goal · {} · {} · round {}/{} · revision {} · {}",
            self.phase.as_str(),
            if self.armed { "armed" } else { "disarmed" },
            self.rounds,
            MAX_GOAL_ROUNDS,
            self.revision,
            self.objective
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoalRound {
    prompt: String,
    revision: u64,
    number: u32,
}

impl GoalRound {
    pub(crate) fn prompt(&self) -> &str {
        &self.prompt
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn number(&self) -> u32 {
        self.number
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GoalCommand {
    Show,
    Create(String),
    Edit(String),
    Pause,
    Resume,
    Clear,
}

impl GoalCommand {
    pub(crate) fn parse(input: &str) -> Option<Result<Self, GoalError>> {
        let trimmed = input.trim_matches(char::is_whitespace);
        let suffix = trimmed.strip_prefix("/goal")?;
        if !suffix.is_empty() && !suffix.starts_with(char::is_whitespace) {
            return None;
        }
        let argument = suffix.trim_matches(char::is_whitespace);
        if argument.is_empty() {
            return Some(Ok(Self::Show));
        }
        if argument.eq_ignore_ascii_case("pause") {
            return Some(Ok(Self::Pause));
        }
        if argument.eq_ignore_ascii_case("resume") {
            return Some(Ok(Self::Resume));
        }
        if argument.eq_ignore_ascii_case("clear") {
            return Some(Ok(Self::Clear));
        }
        if argument.eq_ignore_ascii_case("edit") {
            return Some(Err(GoalError::EmptyObjective));
        }
        if let Some(separator) = argument.find(char::is_whitespace) {
            let (head, tail) = argument.split_at(separator);
            if head.eq_ignore_ascii_case("edit") {
                return Some(validate_objective(tail).map(Self::Edit));
            }
        }
        Some(validate_objective(argument).map(Self::Create))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GoalUpdate {
    Edit,
    Pause,
    Resume,
    Complete,
    Blocked,
}

impl GoalUpdate {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "edit" => Some(Self::Edit),
            "pause" => Some(Self::Pause),
            "resume" => Some(Self::Resume),
            "complete" => Some(Self::Complete),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum GoalError {
    #[error("no Goal exists in this process")]
    Missing,
    #[error("an unfinished Goal already exists; edit, pause, complete, block, or clear it first")]
    Unfinished,
    #[error("the Goal objective must contain non-whitespace text")]
    EmptyObjective,
    #[error("the Goal objective exceeds {MAX_GOAL_OBJECTIVE_BYTES} UTF-8 bytes")]
    ObjectiveTooLarge,
    #[error("the Goal changed since this request; call get_goal and retry with its revision")]
    StaleRevision,
    #[error("this Goal transition is not valid from the current phase")]
    InvalidTransition,
    #[error("blocked requires {MIN_BLOCKED_ROUNDS} consecutive autonomous rounds")]
    BlockThreshold,
    #[error("Goal state is unavailable")]
    Unavailable,
}

#[derive(Default)]
struct GoalState {
    current: Option<GoalSnapshot>,
    revision_clock: u64,
    last_blocked_round: Option<u32>,
}

/// One bounded Goal shared by the interactive driver and Goal tools.
#[derive(Clone, Default)]
pub(crate) struct GoalRuntime {
    state: Arc<Mutex<GoalState>>,
}

impl std::fmt::Debug for GoalRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let present = self
            .state
            .lock()
            .map(|state| state.current.is_some())
            .unwrap_or(false);
        formatter
            .debug_struct("GoalRuntime")
            .field("goal_present", &present)
            .finish()
    }
}

impl GoalRuntime {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn snapshot(&self) -> Result<Option<GoalSnapshot>, GoalError> {
        self.state
            .lock()
            .map(|state| state.current.clone())
            .map_err(|_| GoalError::Unavailable)
    }

    pub(crate) fn is_armed(&self) -> Result<bool, GoalError> {
        Ok(self
            .snapshot()?
            .is_some_and(|goal| goal.phase == GoalPhase::Active && goal.armed))
    }

    pub(crate) fn apply_command(&self, command: GoalCommand) -> Result<String, GoalError> {
        match command {
            GoalCommand::Show => Ok(self
                .snapshot()?
                .map_or_else(|| "Goal · none".to_owned(), |goal| goal.notice())),
            GoalCommand::Create(objective) => self.create(objective).map(|goal| goal.notice()),
            GoalCommand::Edit(objective) => {
                let revision = self.require_snapshot()?.revision;
                self.update(revision, GoalUpdate::Edit, Some(objective))
                    .map(|goal| goal.notice())
            }
            GoalCommand::Pause => {
                let revision = self.require_snapshot()?.revision;
                self.update(revision, GoalUpdate::Pause, None)
                    .map(|goal| goal.notice())
            }
            GoalCommand::Resume => {
                let revision = self.require_snapshot()?.revision;
                self.update(revision, GoalUpdate::Resume, None)
                    .map(|goal| goal.notice())
            }
            GoalCommand::Clear => self.clear(),
        }
    }

    pub(crate) fn create(&self, objective: String) -> Result<GoalSnapshot, GoalError> {
        let objective = validate_objective(&objective)?;
        let mut state = self.state.lock().map_err(|_| GoalError::Unavailable)?;
        if state
            .current
            .as_ref()
            .is_some_and(|goal| goal.phase.is_unfinished())
        {
            return Err(GoalError::Unfinished);
        }
        let revision = next_revision(&mut state)?;
        let goal = GoalSnapshot {
            objective,
            revision,
            phase: GoalPhase::Active,
            armed: true,
            rounds: 0,
            blocked_rounds: 0,
        };
        state.current = Some(goal.clone());
        state.last_blocked_round = None;
        Ok(goal)
    }

    pub(crate) fn update(
        &self,
        expected_revision: u64,
        operation: GoalUpdate,
        objective: Option<String>,
    ) -> Result<GoalSnapshot, GoalError> {
        let objective = match operation {
            GoalUpdate::Edit => Some(validate_objective(
                objective.as_deref().ok_or(GoalError::EmptyObjective)?,
            )?),
            _ if objective.is_some() => return Err(GoalError::InvalidTransition),
            _ => None,
        };
        let mut state = self.state.lock().map_err(|_| GoalError::Unavailable)?;
        let mut goal = state.current.clone().ok_or(GoalError::Missing)?;
        if goal.revision != expected_revision {
            return Err(GoalError::StaleRevision);
        }
        match operation {
            GoalUpdate::Edit if goal.phase.is_unfinished() => {
                goal.objective = objective.ok_or(GoalError::EmptyObjective)?;
                goal.phase = GoalPhase::Active;
                goal.armed = true;
                goal.rounds = 0;
                goal.blocked_rounds = 0;
                state.last_blocked_round = None;
            }
            GoalUpdate::Pause if goal.phase == GoalPhase::Active => {
                goal.phase = GoalPhase::Paused;
                goal.armed = false;
                goal.blocked_rounds = 0;
                state.last_blocked_round = None;
            }
            GoalUpdate::Resume if goal.phase == GoalPhase::Paused => {
                goal.phase = GoalPhase::Active;
                goal.armed = true;
                goal.blocked_rounds = 0;
                state.last_blocked_round = None;
            }
            GoalUpdate::Complete if goal.phase == GoalPhase::Active => {
                goal.phase = GoalPhase::Complete;
                goal.armed = false;
                goal.blocked_rounds = 0;
                state.last_blocked_round = None;
            }
            GoalUpdate::Blocked if goal.phase == GoalPhase::Active && goal.rounds != 0 => {
                if state.last_blocked_round == Some(goal.rounds) {
                    return Err(GoalError::BlockThreshold);
                }
                goal.blocked_rounds = if state.last_blocked_round == goal.rounds.checked_sub(1) {
                    goal.blocked_rounds.saturating_add(1)
                } else {
                    1
                };
                state.last_blocked_round = Some(goal.rounds);
                if goal.blocked_rounds < MIN_BLOCKED_ROUNDS {
                    state.current = Some(goal);
                    return Err(GoalError::BlockThreshold);
                }
                goal.phase = GoalPhase::Blocked;
                goal.armed = false;
            }
            _ => return Err(GoalError::InvalidTransition),
        }
        goal.revision = next_revision(&mut state)?;
        state.current = Some(goal.clone());
        Ok(goal)
    }

    pub(crate) fn clear(&self) -> Result<String, GoalError> {
        let mut state = self.state.lock().map_err(|_| GoalError::Unavailable)?;
        if state.current.take().is_none() {
            return Err(GoalError::Missing);
        }
        state.last_blocked_round = None;
        let _ = next_revision(&mut state)?;
        Ok("Goal · cleared".to_owned())
    }

    pub(crate) fn next_round(&self) -> Result<Option<GoalRound>, GoalError> {
        let mut state = self.state.lock().map_err(|_| GoalError::Unavailable)?;
        let Some(mut goal) = state.current.clone() else {
            return Ok(None);
        };
        if goal.phase != GoalPhase::Active || !goal.armed {
            return Ok(None);
        }
        if goal.rounds >= MAX_GOAL_ROUNDS {
            goal.phase = GoalPhase::Blocked;
            goal.armed = false;
            goal.revision = next_revision(&mut state)?;
            state.current = Some(goal);
            return Ok(None);
        }
        goal.rounds += 1;
        let number = goal.rounds;
        let revision = goal.revision;
        let objective =
            serde_json::to_string(&goal.objective).map_err(|_| GoalError::Unavailable)?;
        let prompt = format!(
            "<goal_round>\nObjective: {objective}\nRound: {number}/{MAX_GOAL_ROUNDS}\nContinue making concrete progress toward this objective. Verify the work that matters. Use get_goal for current state. Call update_goal with expected_revision {revision} and operation complete only when the objective is actually achieved. If the same external blocker prevents progress for three consecutive rounds, call update_goal with operation blocked. Leave the Goal active when useful work remains.\n</goal_round>"
        );
        state.current = Some(goal);
        Ok(Some(GoalRound {
            prompt,
            revision,
            number,
        }))
    }

    pub(crate) fn pause_after_round_failure(&self, revision: u64) -> Result<(), GoalError> {
        let mut state = self.state.lock().map_err(|_| GoalError::Unavailable)?;
        let Some(mut goal) = state.current.clone() else {
            return Ok(());
        };
        if goal.revision != revision || goal.phase != GoalPhase::Active {
            return Ok(());
        }
        goal.phase = GoalPhase::Paused;
        goal.armed = false;
        goal.blocked_rounds = 0;
        state.last_blocked_round = None;
        goal.revision = next_revision(&mut state)?;
        state.current = Some(goal);
        Ok(())
    }

    fn require_snapshot(&self) -> Result<GoalSnapshot, GoalError> {
        self.snapshot()?.ok_or(GoalError::Missing)
    }
}

fn validate_objective(objective: &str) -> Result<String, GoalError> {
    let objective = objective.trim_matches(char::is_whitespace);
    if objective.is_empty() {
        return Err(GoalError::EmptyObjective);
    }
    if objective.len() > MAX_GOAL_OBJECTIVE_BYTES {
        return Err(GoalError::ObjectiveTooLarge);
    }
    Ok(objective.to_owned())
}

fn next_revision(state: &mut GoalState) -> Result<u64, GoalError> {
    state.revision_clock = state
        .revision_clock
        .checked_add(1)
        .ok_or(GoalError::Unavailable)?;
    Ok(state.revision_clock)
}

#[cfg(test)]
mod tests {
    use super::{GoalCommand, GoalError, GoalPhase, GoalRuntime, GoalUpdate, MAX_GOAL_ROUNDS};

    #[test]
    fn command_controls_are_exact_and_objectives_keep_non_control_suffixes() {
        assert_eq!(GoalCommand::parse("/goal"), Some(Ok(GoalCommand::Show)));
        assert_eq!(
            GoalCommand::parse(" /goal PAUSE "),
            Some(Ok(GoalCommand::Pause))
        );
        assert_eq!(
            GoalCommand::parse("/goal pause after tests"),
            Some(Ok(GoalCommand::Create("pause after tests".to_owned())))
        );
        assert_eq!(
            GoalCommand::parse("/goal EDIT ship it"),
            Some(Ok(GoalCommand::Edit("ship it".to_owned())))
        );
        assert_eq!(
            GoalCommand::parse("/goal edit"),
            Some(Err(GoalError::EmptyObjective))
        );
        assert_eq!(GoalCommand::parse("/goalkeeper"), None);
        assert_eq!(GoalCommand::parse("ordinary"), None);
    }

    #[test]
    fn lifecycle_revisions_are_monotonic_and_stale_updates_fail() {
        let goal = GoalRuntime::new();
        let created = goal.create("finish feature".to_owned()).unwrap();
        assert_eq!(created.revision, 1);
        assert_eq!(
            goal.create("replace".to_owned()),
            Err(GoalError::Unfinished)
        );
        let paused = goal
            .update(created.revision, GoalUpdate::Pause, None)
            .unwrap();
        assert_eq!(paused.phase, GoalPhase::Paused);
        assert_eq!(
            goal.update(created.revision, GoalUpdate::Resume, None),
            Err(GoalError::StaleRevision)
        );
        let resumed = goal
            .update(paused.revision, GoalUpdate::Resume, None)
            .unwrap();
        assert_eq!(resumed.phase, GoalPhase::Active);
        assert!(resumed.armed);
    }

    #[test]
    fn three_consecutive_block_reports_are_required() {
        let goal = GoalRuntime::new();
        let created = goal.create("blocked work".to_owned()).unwrap();
        for _ in 0..2 {
            let _ = goal.next_round().unwrap().unwrap();
            assert_eq!(
                goal.update(created.revision, GoalUpdate::Blocked, None),
                Err(GoalError::BlockThreshold)
            );
        }
        let _ = goal.next_round().unwrap().unwrap();
        let blocked = goal
            .update(created.revision, GoalUpdate::Blocked, None)
            .unwrap();
        assert_eq!(blocked.phase, GoalPhase::Blocked);
        assert!(!blocked.armed);
    }

    #[test]
    fn repeated_block_calls_in_one_round_do_not_advance_the_threshold() {
        let goal = GoalRuntime::new();
        let created = goal.create("blocked work".to_owned()).unwrap();
        let _ = goal.next_round().unwrap().unwrap();
        for _ in 0..3 {
            assert_eq!(
                goal.update(created.revision, GoalUpdate::Blocked, None),
                Err(GoalError::BlockThreshold)
            );
        }
        assert_eq!(goal.snapshot().unwrap().unwrap().blocked_rounds, 1);
    }

    #[test]
    fn round_prompt_is_bounded_numbered_and_cap_blocks_continuation() {
        let goal = GoalRuntime::new();
        let created = goal.create("quote \"objective\"".to_owned()).unwrap();
        for expected in 1..=MAX_GOAL_ROUNDS {
            let round = goal.next_round().unwrap().unwrap();
            assert_eq!(round.number(), expected);
            assert_eq!(round.revision(), created.revision);
            assert!(
                round
                    .prompt()
                    .contains("Objective: \"quote \\\"objective\\\"\"")
            );
        }
        assert_eq!(goal.next_round().unwrap(), None);
        let snapshot = goal.snapshot().unwrap().unwrap();
        assert_eq!(snapshot.phase, GoalPhase::Blocked);
        assert!(!snapshot.armed);
    }

    #[test]
    fn cancellation_pauses_only_the_same_active_revision() {
        let goal = GoalRuntime::new();
        let created = goal.create("keep going".to_owned()).unwrap();
        goal.pause_after_round_failure(created.revision).unwrap();
        let paused = goal.snapshot().unwrap().unwrap();
        assert_eq!(paused.phase, GoalPhase::Paused);
        goal.pause_after_round_failure(created.revision).unwrap();
        assert_eq!(goal.snapshot().unwrap().unwrap(), paused);
    }
}
