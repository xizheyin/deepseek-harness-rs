//! Durable Goal facts plus process-local activation for automatic rounds.

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    entropy::EntropySource,
    model::{Message, MessageSourceKind},
    session::{Clock, SystemClock, UnixMillis},
};

pub(crate) const MAX_GOAL_OBJECTIVE_BYTES: usize = 4 * 1024;
pub(crate) const MAX_GOAL_ROUNDS: u32 = 32;
const MIN_BLOCKED_ROUNDS: u32 = 3;
const GOAL_CHANGE_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GoalBlockReason {
    code: String,
    message: String,
}

impl GoalBlockReason {
    fn repeated() -> Self {
        Self {
            code: "repeated-blocker".to_owned(),
            message: "the same external blocker prevented progress for three rounds".to_owned(),
        }
    }

    pub(crate) fn model_reported(message: &str) -> Result<Self, GoalError> {
        let message = message.trim_matches(char::is_whitespace);
        if message.is_empty() || message.len() > 4 * 1024 {
            return Err(GoalError::InvalidBlockReason);
        }
        Ok(Self {
            code: "model-reported".to_owned(),
            message: message.to_owned(),
        })
    }

    fn validate(&self) -> Result<(), GoalError> {
        let valid_code = !self.code.is_empty()
            && self.code.len() <= 128
            && self
                .code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !valid_code || self.message.trim().is_empty() || self.message.len() > 4 * 1024 {
            return Err(GoalError::InvalidEvent);
        }
        Ok(())
    }
}

/// Durable snapshot carried by every non-clear `goal/change` event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GoalDurableSnapshot {
    id: String,
    revision: u64,
    objective: String,
    phase: GoalPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocked_reason: Option<GoalBlockReason>,
    max_goal_rounds: u32,
}

impl GoalDurableSnapshot {
    fn validate(&self) -> Result<(), GoalError> {
        validate_goal_id(&self.id)?;
        if self.revision == 0 || self.revision > crate::session::MAX_SAFE_INTEGER {
            return Err(GoalError::InvalidEvent);
        }
        let _ = validate_objective(&self.objective)?;
        if self.max_goal_rounds == 0
            || u64::from(self.max_goal_rounds) > crate::session::MAX_SAFE_INTEGER
        {
            return Err(GoalError::InvalidEvent);
        }
        match (&self.phase, &self.blocked_reason) {
            (GoalPhase::Blocked, Some(reason)) => reason.validate(),
            (GoalPhase::Blocked, None) | (_, Some(_)) => Err(GoalError::InvalidEvent),
            (_, None) => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GoalRef {
    id: String,
    revision: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum GoalOperation {
    Create,
    Edit,
    Pause,
    Resume,
    Complete,
    Block,
    Clear,
}

/// Versioned wire payload for the append-only `goal/change` session event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalChange {
    kind: String,
    version: u8,
    operation: GoalOperation,
    #[serde(skip_serializing_if = "Option::is_none")]
    goal: Option<GoalDurableSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rounds_started: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<UnixMillis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<UnixMillis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cleared: Option<GoalRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cleared_at: Option<UnixMillis>,
}

impl GoalChange {
    fn mutation(
        operation: GoalOperation,
        goal: GoalDurableSnapshot,
        rounds_started: u32,
        created_at: UnixMillis,
        updated_at: UnixMillis,
    ) -> Self {
        Self {
            kind: "goal/change".to_owned(),
            version: GOAL_CHANGE_VERSION,
            operation,
            goal: Some(goal),
            rounds_started: Some(rounds_started),
            created_at: Some(created_at),
            updated_at: Some(updated_at),
            cleared: None,
            cleared_at: None,
        }
    }

    fn clear(cleared: GoalRef, cleared_at: UnixMillis) -> Self {
        Self {
            kind: "goal/change".to_owned(),
            version: GOAL_CHANGE_VERSION,
            operation: GoalOperation::Clear,
            goal: None,
            rounds_started: None,
            created_at: None,
            updated_at: None,
            cleared: Some(cleared),
            cleared_at: Some(cleared_at),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), GoalError> {
        if self.kind != "goal/change" || self.version != GOAL_CHANGE_VERSION {
            return Err(GoalError::InvalidEvent);
        }
        match self.operation {
            GoalOperation::Clear => {
                if self.goal.is_some()
                    || self.rounds_started.is_some()
                    || self.created_at.is_some()
                    || self.updated_at.is_some()
                    || self.cleared_at.is_none()
                {
                    return Err(GoalError::InvalidEvent);
                }
                let cleared = self.cleared.as_ref().ok_or(GoalError::InvalidEvent)?;
                validate_goal_id(&cleared.id)?;
                if cleared.revision == 0 || cleared.revision > crate::session::MAX_SAFE_INTEGER {
                    return Err(GoalError::InvalidEvent);
                }
                Ok(())
            }
            _ => {
                if self.cleared.is_some() || self.cleared_at.is_some() {
                    return Err(GoalError::InvalidEvent);
                }
                let goal = self.goal.as_ref().ok_or(GoalError::InvalidEvent)?;
                goal.validate()?;
                let rounds = self.rounds_started.ok_or(GoalError::InvalidEvent)?;
                if rounds > goal.max_goal_rounds {
                    return Err(GoalError::InvalidEvent);
                }
                let created = self.created_at.ok_or(GoalError::InvalidEvent)?;
                let updated = self.updated_at.ok_or(GoalError::InvalidEvent)?;
                if updated < created {
                    return Err(GoalError::InvalidEvent);
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DurableGoalState {
    goal: GoalDurableSnapshot,
    rounds_started: u32,
    created_at: UnixMillis,
    updated_at: UnixMillis,
}

/// Goal facts reconstructed exclusively from committed Session events.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct GoalReplayState {
    current: Option<DurableGoalState>,
    used_ids: BTreeSet<String>,
}

impl GoalReplayState {
    pub(crate) fn apply_change(&mut self, change: &GoalChange) -> Result<(), GoalError> {
        change.validate()?;
        match change.operation {
            GoalOperation::Create => self.apply_create(change),
            GoalOperation::Edit
            | GoalOperation::Pause
            | GoalOperation::Resume
            | GoalOperation::Complete
            | GoalOperation::Block => self.apply_update(change),
            GoalOperation::Clear => self.apply_clear(change),
        }
    }

    pub(crate) fn apply_goal_message(&mut self, message: &Message) -> Result<(), GoalError> {
        let MessageSourceKind::Other { kind } = message.source().kind() else {
            return Ok(());
        };
        if kind != "goal" {
            return Ok(());
        }
        let source = message
            .source()
            .raw()
            .as_value()
            .as_object()
            .ok_or(GoalError::InvalidEvent)?;
        if source.len() != 4 || source.get("kind").and_then(Value::as_str) != Some("goal") {
            return Err(GoalError::InvalidEvent);
        }
        let goal_id = source
            .get("goalId")
            .and_then(Value::as_str)
            .ok_or(GoalError::InvalidEvent)?;
        let revision = source
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or(GoalError::InvalidEvent)?;
        let round = source
            .get("round")
            .and_then(Value::as_u64)
            .and_then(|round| u32::try_from(round).ok())
            .ok_or(GoalError::InvalidEvent)?;
        let current = self.current.as_mut().ok_or(GoalError::InvalidEvent)?;
        if current.goal.phase != GoalPhase::Active
            || current.goal.id != goal_id
            || current.goal.revision != revision
            || round != current.rounds_started.saturating_add(1)
            || round > current.goal.max_goal_rounds
        {
            return Err(GoalError::InvalidEvent);
        }
        current.rounds_started = round;
        Ok(())
    }

    fn apply_create(&mut self, change: &GoalChange) -> Result<(), GoalError> {
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.goal.phase != GoalPhase::Complete)
        {
            return Err(GoalError::InvalidEvent);
        }
        let next = durable_state(change)?;
        if next.goal.revision != 1
            || next.goal.phase != GoalPhase::Active
            || next.rounds_started != 0
            || self.used_ids.contains(&next.goal.id)
        {
            return Err(GoalError::InvalidEvent);
        }
        self.used_ids.insert(next.goal.id.clone());
        self.current = Some(next);
        Ok(())
    }

    fn apply_update(&mut self, change: &GoalChange) -> Result<(), GoalError> {
        let current = self.current.as_ref().ok_or(GoalError::InvalidEvent)?;
        let next = durable_state(change)?;
        if next.goal.id != current.goal.id
            || next.goal.revision != current.goal.revision.saturating_add(1)
            || (next.goal.objective != current.goal.objective
                && change.operation != GoalOperation::Edit)
            || next.rounds_started != current.rounds_started
            || next.created_at != current.created_at
            || next.updated_at < current.updated_at
        {
            return Err(GoalError::InvalidEvent);
        }
        let valid_transition = match change.operation {
            GoalOperation::Edit => {
                next.goal.phase == current.goal.phase
                    && next.goal.blocked_reason == current.goal.blocked_reason
            }
            GoalOperation::Pause => {
                current.goal.phase == GoalPhase::Active
                    && next.goal.phase == GoalPhase::Paused
                    && next.goal.blocked_reason.is_none()
                    && same_goal_definition(&current.goal, &next.goal)
            }
            GoalOperation::Resume => {
                matches!(
                    current.goal.phase,
                    GoalPhase::Active | GoalPhase::Paused | GoalPhase::Blocked
                ) && next.goal.phase == GoalPhase::Active
                    && next.goal.blocked_reason.is_none()
                    && next.rounds_started < next.goal.max_goal_rounds
                    && same_goal_definition(&current.goal, &next.goal)
            }
            GoalOperation::Complete => {
                current.goal.phase != GoalPhase::Complete
                    && next.goal.phase == GoalPhase::Complete
                    && next.goal.blocked_reason.is_none()
                    && same_goal_definition(&current.goal, &next.goal)
            }
            GoalOperation::Block => {
                current.goal.phase == GoalPhase::Active
                    && next.goal.phase == GoalPhase::Blocked
                    && next.goal.blocked_reason.is_some()
                    && same_goal_definition(&current.goal, &next.goal)
            }
            GoalOperation::Create | GoalOperation::Clear => false,
        };
        if !valid_transition {
            return Err(GoalError::InvalidEvent);
        }
        self.current = Some(next);
        Ok(())
    }

    fn apply_clear(&mut self, change: &GoalChange) -> Result<(), GoalError> {
        let current = self.current.as_ref().ok_or(GoalError::InvalidEvent)?;
        let cleared = change.cleared.as_ref().ok_or(GoalError::InvalidEvent)?;
        if cleared.id != current.goal.id
            || cleared.revision != current.goal.revision.saturating_add(1)
            || change
                .cleared_at
                .is_some_and(|time| time < current.updated_at)
        {
            return Err(GoalError::InvalidEvent);
        }
        self.current = None;
        Ok(())
    }

    fn runtime_state(&self) -> Option<DurableGoalState> {
        self.current.clone()
    }

    fn runtime_ids(&self) -> BTreeSet<String> {
        self.used_ids.clone()
    }
}

fn same_goal_definition(current: &GoalDurableSnapshot, next: &GoalDurableSnapshot) -> bool {
    current.objective == next.objective && current.max_goal_rounds == next.max_goal_rounds
}

fn durable_state(change: &GoalChange) -> Result<DurableGoalState, GoalError> {
    Ok(DurableGoalState {
        goal: change.goal.clone().ok_or(GoalError::InvalidEvent)?,
        rounds_started: change.rounds_started.ok_or(GoalError::InvalidEvent)?,
        created_at: change.created_at.ok_or(GoalError::InvalidEvent)?,
        updated_at: change.updated_at.ok_or(GoalError::InvalidEvent)?,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoalSnapshot {
    durable: DurableGoalState,
    armed: bool,
}

impl GoalSnapshot {
    pub(crate) fn to_value(&self) -> Value {
        let mut value = json!({
            "activation": if self.armed { "armed" } else { "disarmed" },
            "goalId": self.durable.goal.id,
            "maxGoalRounds": self.durable.goal.max_goal_rounds,
            "objective": self.durable.goal.objective,
            "phase": self.durable.goal.phase.as_str(),
            "revision": self.durable.goal.revision,
            "roundsStarted": self.durable.rounds_started,
        });
        if let Some(reason) = &self.durable.goal.blocked_reason {
            value["blockedReason"] = serde_json::to_value(reason).unwrap_or(Value::Null);
        }
        value
    }

    pub(crate) fn tool_value(&self) -> Value {
        let mut goal = json!({
            "id": self.durable.goal.id,
            "revision": self.durable.goal.revision,
            "objective": self.durable.goal.objective,
            "phase": self.durable.goal.phase.as_str(),
            "roundsStarted": self.durable.rounds_started,
            "maxGoalRounds": self.durable.goal.max_goal_rounds,
        });
        if let Some(reason) = &self.durable.goal.blocked_reason {
            goal["blockedReason"] = serde_json::to_value(reason).unwrap_or(Value::Null);
        }
        json!({
            "goal": goal,
            "activation": if self.armed { "armed" } else { "disarmed" },
        })
    }

    fn notice(&self) -> String {
        format!(
            "Goal · {} · {} · round {}/{} · revision {} · {}",
            self.durable.goal.phase.as_str(),
            if self.armed { "armed" } else { "disarmed" },
            self.durable.rounds_started,
            self.durable.goal.max_goal_rounds,
            self.durable.goal.revision,
            self.durable.goal.objective
        )
    }

    pub(crate) fn revision(&self) -> u64 {
        self.durable.goal.revision
    }

    #[cfg(test)]
    pub(crate) fn id(&self) -> &str {
        &self.durable.goal.id
    }

    pub(crate) fn rounds_started(&self) -> u32 {
        self.durable.rounds_started
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoalRound {
    prompt: String,
    goal_id: String,
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
    #[error("no Goal exists in this session")]
    Missing,
    #[error("an unfinished Goal already exists; edit, pause, complete, block, or clear it first")]
    Unfinished,
    #[error("the Goal objective must contain non-whitespace text")]
    EmptyObjective,
    #[error("the Goal objective exceeds {MAX_GOAL_OBJECTIVE_BYTES} UTF-8 bytes")]
    ObjectiveTooLarge,
    #[error("max_goal_rounds must be a positive 32-bit integer")]
    InvalidMaxRounds,
    #[error("Goal edit requires objective and/or max_goal_rounds")]
    InvalidEdit,
    #[error("blocked_reason must contain non-whitespace text")]
    InvalidBlockReason,
    #[error("the Goal changed since this request; call get_goal and retry with its revision")]
    StaleRevision,
    #[error("this Goal transition is not valid from the current phase")]
    InvalidTransition,
    #[error("blocked requires at least {MIN_BLOCKED_ROUNDS} autonomous rounds")]
    BlockThreshold,
    #[error("another Goal change is still being committed")]
    Busy,
    #[error("Goal state is unavailable")]
    Unavailable,
    #[error("Goal change could not be recorded: {0}")]
    Commit(String),
    #[error("invalid goal/change history")]
    InvalidEvent,
}

#[derive(Default)]
struct GoalState {
    current: Option<DurableGoalState>,
    used_ids: BTreeSet<String>,
    armed: bool,
    pending: Option<u64>,
    next_token: u64,
}

/// One Goal shared by the Session projection, interactive driver, and tools.
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
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn from_replay(replay: &GoalReplayState) -> Self {
        Self {
            state: Arc::new(Mutex::new(GoalState {
                current: replay.runtime_state(),
                used_ids: replay.runtime_ids(),
                armed: false,
                pending: None,
                next_token: 0,
            })),
        }
    }

    pub(crate) fn snapshot(&self) -> Result<Option<GoalSnapshot>, GoalError> {
        let state = self.state.lock().map_err(|_| GoalError::Unavailable)?;
        Ok(state.current.clone().map(|durable| GoalSnapshot {
            durable,
            armed: state.armed,
        }))
    }

    pub(crate) fn is_armed(&self) -> Result<bool, GoalError> {
        let state = self.state.lock().map_err(|_| GoalError::Unavailable)?;
        Ok(state.armed
            && state
                .current
                .as_ref()
                .is_some_and(|goal| goal.goal.phase == GoalPhase::Active))
    }

    pub(crate) fn apply_command(&self, command: GoalCommand) -> Result<String, GoalError> {
        match self.prepare_command(command)? {
            GoalCommandPreparation::Show(message) => Ok(message),
            GoalCommandPreparation::Mutation(mutation) => mutation.commit(),
        }
    }

    pub(crate) fn prepare_command(
        &self,
        command: GoalCommand,
    ) -> Result<GoalCommandPreparation, GoalError> {
        match command {
            GoalCommand::Show => Ok(GoalCommandPreparation::Show(
                self.snapshot()?
                    .map_or_else(|| "Goal · none".to_owned(), |goal| goal.notice()),
            )),
            GoalCommand::Create(objective) => self
                .prepare_create(objective)
                .map(GoalCommandPreparation::Mutation),
            GoalCommand::Edit(objective) => {
                let revision = self.require_snapshot()?.revision();
                self.prepare_update(revision, GoalUpdate::Edit, Some(objective))
                    .map(GoalCommandPreparation::Mutation)
            }
            GoalCommand::Pause => {
                let revision = self.require_snapshot()?.revision();
                self.prepare_update(revision, GoalUpdate::Pause, None)
                    .map(GoalCommandPreparation::Mutation)
            }
            GoalCommand::Resume => {
                let revision = self.require_snapshot()?.revision();
                self.prepare_update(revision, GoalUpdate::Resume, None)
                    .map(GoalCommandPreparation::Mutation)
            }
            GoalCommand::Clear => self.prepare_clear().map(GoalCommandPreparation::Mutation),
        }
    }

    #[cfg(test)]
    pub(crate) fn create(&self, objective: String) -> Result<GoalSnapshot, GoalError> {
        self.prepare_create(objective)?.commit_snapshot()
    }

    #[cfg(test)]
    pub(crate) fn update(
        &self,
        expected_revision: u64,
        operation: GoalUpdate,
        objective: Option<String>,
    ) -> Result<GoalSnapshot, GoalError> {
        self.prepare_update(expected_revision, operation, objective)?
            .commit_snapshot()
    }

    pub(crate) fn prepare_create(
        &self,
        objective: String,
    ) -> Result<PreparedGoalMutation, GoalError> {
        self.prepare_create_with_max(objective, None)
    }

    pub(crate) fn prepare_create_with_max(
        &self,
        objective: String,
        max_goal_rounds: Option<u32>,
    ) -> Result<PreparedGoalMutation, GoalError> {
        let objective = validate_objective(&objective)?;
        let max_goal_rounds = validate_max_goal_rounds(max_goal_rounds.unwrap_or(MAX_GOAL_ROUNDS))?;
        let mut state = self.state.lock().map_err(|_| GoalError::Unavailable)?;
        ensure_not_pending(&state)?;
        if state
            .current
            .as_ref()
            .is_some_and(|goal| goal.goal.phase != GoalPhase::Complete)
        {
            return Err(GoalError::Unfinished);
        }
        let id = format!(
            "goal-{}",
            EntropySource::system()
                .uuid_v4()
                .map_err(|_| GoalError::Unavailable)?
        );
        if state.used_ids.contains(&id) {
            return Err(GoalError::Unavailable);
        }
        let now = SystemClock.now().map_err(|_| GoalError::Unavailable)?;
        let next = DurableGoalState {
            goal: GoalDurableSnapshot {
                id,
                revision: 1,
                objective,
                phase: GoalPhase::Active,
                blocked_reason: None,
                max_goal_rounds,
            },
            rounds_started: 0,
            created_at: now,
            updated_at: now,
        };
        let change = GoalChange::mutation(
            GoalOperation::Create,
            next.goal.clone(),
            next.rounds_started,
            next.created_at,
            next.updated_at,
        );
        prepare_locked(self, &mut state, Some(next), true, change)
    }

    pub(crate) fn prepare_update(
        &self,
        expected_revision: u64,
        operation: GoalUpdate,
        objective: Option<String>,
    ) -> Result<PreparedGoalMutation, GoalError> {
        self.prepare_update_exact(None, expected_revision, operation, objective, None, None)
    }

    pub(crate) fn prepare_update_exact(
        &self,
        expected_goal_id: Option<&str>,
        expected_revision: u64,
        operation: GoalUpdate,
        objective: Option<String>,
        max_goal_rounds: Option<u32>,
        blocked_reason: Option<GoalBlockReason>,
    ) -> Result<PreparedGoalMutation, GoalError> {
        let objective = objective.as_deref().map(validate_objective).transpose()?;
        let max_goal_rounds = max_goal_rounds.map(validate_max_goal_rounds).transpose()?;
        match operation {
            GoalUpdate::Edit if objective.is_none() && max_goal_rounds.is_none() => {
                return Err(GoalError::InvalidEdit);
            }
            GoalUpdate::Edit if blocked_reason.is_some() => {
                return Err(GoalError::InvalidTransition);
            }
            GoalUpdate::Edit => {}
            GoalUpdate::Blocked if objective.is_some() || max_goal_rounds.is_some() => {
                return Err(GoalError::InvalidTransition);
            }
            GoalUpdate::Blocked => {}
            _ if objective.is_some() || max_goal_rounds.is_some() || blocked_reason.is_some() => {
                return Err(GoalError::InvalidTransition);
            }
            _ => {}
        }
        let mut state = self.state.lock().map_err(|_| GoalError::Unavailable)?;
        ensure_not_pending(&state)?;
        let current = state.current.clone().ok_or(GoalError::Missing)?;
        if current.goal.revision != expected_revision {
            return Err(GoalError::StaleRevision);
        }
        if expected_goal_id.is_some_and(|id| id != current.goal.id) {
            return Err(GoalError::StaleRevision);
        }
        let mut next = current.clone();
        let (goal_operation, armed) = match operation {
            GoalUpdate::Edit => {
                if let Some(objective) = objective {
                    next.goal.objective = objective;
                }
                if let Some(max_goal_rounds) = max_goal_rounds {
                    next.goal.max_goal_rounds = max_goal_rounds;
                }
                (GoalOperation::Edit, state.armed)
            }
            GoalUpdate::Pause if current.goal.phase == GoalPhase::Active => {
                next.goal.phase = GoalPhase::Paused;
                next.goal.blocked_reason = None;
                (GoalOperation::Pause, false)
            }
            GoalUpdate::Resume
                if matches!(
                    current.goal.phase,
                    GoalPhase::Active | GoalPhase::Paused | GoalPhase::Blocked
                ) && current.rounds_started < current.goal.max_goal_rounds =>
            {
                next.goal.phase = GoalPhase::Active;
                next.goal.blocked_reason = None;
                (GoalOperation::Resume, true)
            }
            GoalUpdate::Complete if current.goal.phase != GoalPhase::Complete => {
                next.goal.phase = GoalPhase::Complete;
                next.goal.blocked_reason = None;
                (GoalOperation::Complete, false)
            }
            GoalUpdate::Blocked if current.goal.phase == GoalPhase::Active => {
                next.goal.phase = GoalPhase::Blocked;
                next.goal.blocked_reason =
                    Some(blocked_reason.unwrap_or_else(GoalBlockReason::repeated));
                (GoalOperation::Block, false)
            }
            _ => return Err(GoalError::InvalidTransition),
        };
        next.goal.revision = current
            .goal
            .revision
            .checked_add(1)
            .filter(|revision| *revision <= crate::session::MAX_SAFE_INTEGER)
            .ok_or(GoalError::Unavailable)?;
        let now = SystemClock.now().map_err(|_| GoalError::Unavailable)?;
        next.updated_at = now.max(current.updated_at);
        let change = GoalChange::mutation(
            goal_operation,
            next.goal.clone(),
            next.rounds_started,
            next.created_at,
            next.updated_at,
        );
        prepare_locked(self, &mut state, Some(next), armed, change)
    }

    pub(crate) fn prepare_clear(&self) -> Result<PreparedGoalMutation, GoalError> {
        let mut state = self.state.lock().map_err(|_| GoalError::Unavailable)?;
        ensure_not_pending(&state)?;
        let current = state.current.as_ref().ok_or(GoalError::Missing)?;
        let revision = current
            .goal
            .revision
            .checked_add(1)
            .filter(|revision| *revision <= crate::session::MAX_SAFE_INTEGER)
            .ok_or(GoalError::Unavailable)?;
        let change = GoalChange::clear(
            GoalRef {
                id: current.goal.id.clone(),
                revision,
            },
            SystemClock.now().map_err(|_| GoalError::Unavailable)?,
        );
        prepare_locked(self, &mut state, None, false, change)
    }

    pub(crate) fn next_round(&self) -> Result<Option<GoalRound>, GoalError> {
        let mut state = self.state.lock().map_err(|_| GoalError::Unavailable)?;
        ensure_not_pending(&state)?;
        if !state.armed {
            return Ok(None);
        }
        let Some(goal) = state.current.as_mut() else {
            return Ok(None);
        };
        if goal.goal.phase != GoalPhase::Active || goal.rounds_started >= goal.goal.max_goal_rounds
        {
            state.armed = false;
            return Ok(None);
        }
        goal.rounds_started += 1;
        let number = goal.rounds_started;
        let revision = goal.goal.revision;
        let goal_id = goal.goal.id.clone();
        let max_rounds = goal.goal.max_goal_rounds;
        let objective =
            serde_json::to_string(&goal.goal.objective).map_err(|_| GoalError::Unavailable)?;
        let goal_id_json = serde_json::to_string(&goal_id).map_err(|_| GoalError::Unavailable)?;
        let prompt = format!(
            "<goal_round>\nObjective: {objective}\nRound: {number}/{max_rounds}\nContinue making concrete progress toward this objective. Verify the work that matters. Use get_goal for current state. Call update_goal with goal_id {goal_id_json}, revision {revision}, and action complete only when the objective is actually achieved. If the same external blocker prevents progress for three consecutive rounds, call update_goal with action blocked and a concrete blocked_reason. Leave the Goal active when useful work remains.\n</goal_round>"
        );
        Ok(Some(GoalRound {
            prompt,
            goal_id,
            revision,
            number,
        }))
    }

    pub(crate) fn rollback_uncommitted_round(
        &self,
        revision: u64,
        round: u32,
    ) -> Result<(), GoalError> {
        let mut state = self.state.lock().map_err(|_| GoalError::Unavailable)?;
        if let Some(goal) = state.current.as_mut() {
            if goal.goal.revision == revision && goal.rounds_started == round {
                goal.rounds_started = goal.rounds_started.saturating_sub(1);
            }
        }
        Ok(())
    }

    fn require_snapshot(&self) -> Result<GoalSnapshot, GoalError> {
        self.snapshot()?.ok_or(GoalError::Missing)
    }
}

#[derive(Debug)]
pub(crate) enum GoalCommandPreparation {
    Show(String),
    Mutation(PreparedGoalMutation),
}

/// Single-use state change installed only after its Session event commits.
pub struct PreparedGoalMutation {
    runtime: GoalRuntime,
    token: u64,
    change: GoalChange,
    next: Option<DurableGoalState>,
    armed: bool,
    settled: bool,
}

impl std::fmt::Debug for PreparedGoalMutation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedGoalMutation")
            .field("operation", &self.change.operation)
            .finish_non_exhaustive()
    }
}

impl PreparedGoalMutation {
    pub(crate) fn change(&self) -> &GoalChange {
        &self.change
    }

    pub(crate) fn result_value(&self) -> Value {
        self.next.as_ref().map_or_else(
            || json!({ "goal": null }),
            |durable| {
                GoalSnapshot {
                    durable: durable.clone(),
                    armed: self.armed,
                }
                .tool_value()
            },
        )
    }

    pub(crate) fn commit(mut self) -> Result<String, GoalError> {
        let notice = self.next.as_ref().map_or_else(
            || "Goal · cleared".to_owned(),
            |durable| {
                GoalSnapshot {
                    durable: durable.clone(),
                    armed: self.armed,
                }
                .notice()
            },
        );
        self.install()?;
        Ok(notice)
    }

    pub(crate) fn commit_snapshot(mut self) -> Result<GoalSnapshot, GoalError> {
        let snapshot = GoalSnapshot {
            durable: self.next.clone().ok_or(GoalError::Unavailable)?,
            armed: self.armed,
        };
        self.install()?;
        Ok(snapshot)
    }

    fn install(&mut self) -> Result<(), GoalError> {
        let mut state = self
            .runtime
            .state
            .lock()
            .map_err(|_| GoalError::Unavailable)?;
        if state.pending != Some(self.token) {
            return Err(GoalError::Unavailable);
        }
        state.current = self.next.clone();
        if self.change.operation == GoalOperation::Create {
            let id = self
                .next
                .as_ref()
                .ok_or(GoalError::Unavailable)?
                .goal
                .id
                .clone();
            state.used_ids.insert(id);
        }
        state.armed = self.armed;
        state.pending = None;
        drop(state);
        self.settled = true;
        Ok(())
    }
}

impl Drop for PreparedGoalMutation {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        if let Ok(mut state) = self.runtime.state.lock() {
            if state.pending == Some(self.token) {
                state.pending = None;
            }
        }
    }
}

fn prepare_locked(
    runtime: &GoalRuntime,
    state: &mut GoalState,
    next: Option<DurableGoalState>,
    armed: bool,
    change: GoalChange,
) -> Result<PreparedGoalMutation, GoalError> {
    change.validate()?;
    state.next_token = state
        .next_token
        .checked_add(1)
        .ok_or(GoalError::Unavailable)?;
    let token = state.next_token;
    state.pending = Some(token);
    Ok(PreparedGoalMutation {
        runtime: runtime.clone(),
        token,
        change,
        next,
        armed,
        settled: false,
    })
}

fn ensure_not_pending(state: &GoalState) -> Result<(), GoalError> {
    if state.pending.is_some() {
        Err(GoalError::Busy)
    } else {
        Ok(())
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

fn validate_max_goal_rounds(value: u32) -> Result<u32, GoalError> {
    if value == 0 {
        Err(GoalError::InvalidMaxRounds)
    } else {
        Ok(value)
    }
}

fn validate_goal_id(id: &str) -> Result<(), GoalError> {
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        Err(GoalError::InvalidEvent)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        model::{ContentBlock, Message, MessageSource},
        session::{EventKind, NewEvent, Session, SystemClock, TurnId},
    };

    use super::{
        GoalCommand, GoalError, GoalPhase, GoalReplayState, GoalRuntime, GoalUpdate,
        MAX_GOAL_ROUNDS,
    };

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
    }

    #[test]
    fn prepared_change_is_invisible_until_commit_and_drop_aborts_it() {
        let goal = GoalRuntime::new();
        let prepared = goal.prepare_create("finish feature".to_owned()).unwrap();
        assert_eq!(goal.snapshot().unwrap(), None);
        drop(prepared);
        assert_eq!(goal.snapshot().unwrap(), None);
        let created = goal.create("finish feature".to_owned()).unwrap();
        assert_eq!(created.revision(), 1);
    }

    #[test]
    fn replayed_goal_starts_disarmed_and_can_be_explicitly_resumed() {
        let source = GoalRuntime::new();
        let prepared = source.prepare_create("resume safely".to_owned()).unwrap();
        let mut replay = GoalReplayState::default();
        replay.apply_change(prepared.change()).unwrap();
        let restored = GoalRuntime::from_replay(&replay);
        assert!(!restored.is_armed().unwrap());
        let revision = restored.snapshot().unwrap().unwrap().revision();
        let resumed = restored.update(revision, GoalUpdate::Resume, None).unwrap();
        assert!(resumed.armed);
    }

    #[test]
    fn replay_rejects_stale_changes_and_never_reuses_a_cleared_id() {
        let runtime = GoalRuntime::new();
        let create = runtime.prepare_create("strict replay".to_owned()).unwrap();
        let create_change = create.change().clone();
        let mut replay = GoalReplayState::default();
        replay.apply_change(&create_change).unwrap();
        create.commit().unwrap();

        let pause = runtime.prepare_update(1, GoalUpdate::Pause, None).unwrap();
        let pause_change = pause.change().clone();
        replay.apply_change(&pause_change).unwrap();
        assert_eq!(
            replay.apply_change(&pause_change),
            Err(GoalError::InvalidEvent)
        );
        pause.commit().unwrap();

        let clear = runtime.prepare_clear().unwrap();
        replay.apply_change(clear.change()).unwrap();
        clear.commit().unwrap();
        assert_eq!(
            replay.apply_change(&create_change),
            Err(GoalError::InvalidEvent)
        );

        let mut malformed = create_change;
        malformed.kind = "goal/other".to_owned();
        assert_eq!(
            GoalReplayState::default().apply_change(&malformed),
            Err(GoalError::InvalidEvent)
        );
    }

    #[test]
    fn cap_only_edit_replays_and_can_rearm_an_exhausted_goal() {
        let runtime = GoalRuntime::new();
        let create = runtime
            .prepare_create_with_max("bounded".to_owned(), Some(1))
            .unwrap();
        let mut replay = GoalReplayState::default();
        replay.apply_change(create.change()).unwrap();
        create.commit().unwrap();
        let goal_id = runtime.snapshot().unwrap().unwrap().id().to_owned();

        let round = runtime.next_round().unwrap().unwrap();
        let source = MessageSource::from_value(serde_json::json!({
            "kind": "goal",
            "goalId": round.goal_id,
            "revision": round.revision,
            "round": round.number,
        }))
        .unwrap();
        let message = Message::user(
            "goal-round-cap",
            vec![ContentBlock::text(round.prompt).unwrap()],
            source,
        )
        .unwrap();
        replay.apply_goal_message(&message).unwrap();
        assert_eq!(runtime.next_round().unwrap(), None);
        assert!(matches!(
            runtime.prepare_update_exact(Some(&goal_id), 1, GoalUpdate::Resume, None, None, None,),
            Err(GoalError::InvalidTransition)
        ));

        let edit = runtime
            .prepare_update_exact(Some(&goal_id), 1, GoalUpdate::Edit, None, Some(2), None)
            .unwrap();
        replay.apply_change(edit.change()).unwrap();
        edit.commit().unwrap();
        let resumed = runtime
            .prepare_update_exact(Some(&goal_id), 2, GoalUpdate::Resume, None, None, None)
            .unwrap();
        replay.apply_change(resumed.change()).unwrap();
        resumed.commit().unwrap();

        let restored = GoalRuntime::from_replay(&replay);
        let value = restored.snapshot().unwrap().unwrap().to_value();
        assert_eq!(value["maxGoalRounds"], 2);
        assert_eq!(value["revision"], 3);
        assert_eq!(value["activation"], "disarmed");
    }

    #[test]
    fn event_domain_allows_blocking_before_tool_policy_threshold() {
        let goal = GoalRuntime::new();
        let created = goal.create("blocked work".to_owned()).unwrap();
        let blocked = goal
            .update(created.revision(), GoalUpdate::Blocked, None)
            .unwrap();
        assert_eq!(blocked.durable.goal.phase, GoalPhase::Blocked);
    }

    #[test]
    fn round_prompt_is_bounded_numbered_and_cap_disarms_continuation() {
        let goal = GoalRuntime::new();
        let created = goal.create("quote \"objective\"".to_owned()).unwrap();
        for expected in 1..=MAX_GOAL_ROUNDS {
            let round = goal.next_round().unwrap().unwrap();
            assert_eq!(round.number(), expected);
            assert_eq!(round.revision(), created.revision());
            assert_eq!(round.goal_id, created.durable.goal.id);
        }
        assert_eq!(goal.next_round().unwrap(), None);
        assert!(!goal.is_armed().unwrap());
    }

    #[test]
    fn session_codec_replays_goal_change_and_exact_round_source() {
        let goal = GoalRuntime::new();
        let prepared = goal.prepare_create("persist this Goal".to_owned()).unwrap();
        let mut session = Session::new("goal-session").unwrap();
        session
            .append(NewEvent::log(EventKind::goal_change(
                prepared.change().clone(),
            )))
            .unwrap();
        prepared.commit().unwrap();

        let round = goal.next_round().unwrap().unwrap();
        let source = MessageSource::from_value(serde_json::json!({
            "kind": "goal",
            "goalId": round.goal_id,
            "revision": round.revision,
            "round": round.number,
        }))
        .unwrap();
        let message = Message::user(
            "goal-round-1",
            vec![ContentBlock::text(round.prompt).unwrap()],
            source,
        )
        .unwrap();
        let turn = TurnId::first();
        session
            .append(NewEvent::log(EventKind::turn_start(turn)))
            .unwrap();
        session
            .append(NewEvent::surface(
                EventKind::user_message(message),
                crate::session::SurfaceIntent::append(),
            ))
            .unwrap();

        let encoded = session.to_json().unwrap();
        assert!(encoded.contains("\"type\":\"goal/change\""));
        let restored = Session::from_json(&encoded, SystemClock).unwrap();
        let runtime = GoalRuntime::from_replay(restored.state().goal_replay());
        let value = runtime.snapshot().unwrap().unwrap().to_value();
        assert_eq!(value["roundsStarted"], 1);
        assert_eq!(value["activation"], "disarmed");
    }
}
