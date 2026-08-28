//! Durable, opt-in clock context sampled before each model step.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use jiff::{Timestamp, fmt::strtime, tz::TimeZone};
use thiserror::Error;

use crate::{
    agent::{AgentIdKind, AgentRuntime, AgentRuntimeError},
    model::{ContentBlock, ContextSnapshotSection, Message, MessageSource},
    session::{Session, StepId, TurnId, UnixMillis},
};

pub(crate) const TIME_CONTEXT_SOURCE: &str = "time-context";
pub(crate) const MAX_TIME_ZONE_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum TimeContextError {
    #[error("time zone must be one canonical IANA name such as Asia/Shanghai or UTC")]
    InvalidZone,
    #[error("time zone is not canonical; use {canonical}")]
    NonCanonicalZone { canonical: String },
    #[error("the system clock is unavailable")]
    Clock,
    #[error("the time-context timestamp could not be formatted")]
    Format,
    #[error("the time-context message could not be constructed")]
    Message,
    #[error(transparent)]
    Runtime(#[from] AgentRuntimeError),
}

pub(crate) trait TimeContextClock: Send + Sync {
    fn now(&self) -> Result<UnixMillis, TimeContextError>;
}

#[derive(Clone, Copy, Debug, Default)]
struct SystemTimeContextClock;

impl TimeContextClock for SystemTimeContextClock {
    fn now(&self) -> Result<UnixMillis, TimeContextError> {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| TimeContextError::Clock)?;
        let millis = i64::try_from(elapsed.as_millis()).map_err(|_| TimeContextError::Clock)?;
        UnixMillis::new(millis).map_err(|_| TimeContextError::Clock)
    }
}

#[derive(Clone)]
pub(crate) struct TimeContextRuntime {
    zone: TimeZone,
    zone_name: String,
    clock: Arc<dyn TimeContextClock>,
}

impl std::fmt::Debug for TimeContextRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TimeContextRuntime")
            .field("zone", &self.zone_name)
            .finish_non_exhaustive()
    }
}

impl TimeContextRuntime {
    pub(crate) fn new(zone_name: &str) -> Result<Self, TimeContextError> {
        Self::with_clock(zone_name, Arc::new(SystemTimeContextClock))
    }

    pub(crate) fn with_clock(
        zone_name: &str,
        clock: Arc<dyn TimeContextClock>,
    ) -> Result<Self, TimeContextError> {
        if zone_name.is_empty()
            || zone_name.len() > MAX_TIME_ZONE_BYTES
            || zone_name.chars().any(char::is_control)
        {
            return Err(TimeContextError::InvalidZone);
        }
        let zone = TimeZone::get(zone_name).map_err(|_| TimeContextError::InvalidZone)?;
        let canonical = zone
            .iana_name()
            .ok_or(TimeContextError::InvalidZone)?
            .to_owned();
        if canonical != zone_name {
            return Err(TimeContextError::NonCanonicalZone { canonical });
        }
        Ok(Self {
            zone,
            zone_name: canonical,
            clock,
        })
    }

    pub(crate) fn prepare(
        &self,
        session: &Session,
        turn: TurnId,
        step: StepId,
        runtime: &dyn AgentRuntime,
    ) -> Result<Message, TimeContextError> {
        let now = self.clock.now()?;
        let previous = session.time_context_baseline(step);
        let timestamp = format_timestamp(now, &self.zone)?;
        let elapsed = previous.map_or_else(
            || "unavailable".to_owned(),
            |previous| format_duration(now.get().saturating_sub(previous.get())),
        );
        let baseline = if step == StepId::first() {
            "model-visible message"
        } else {
            "step context"
        };
        let text = format!(
            "Time sampled while preparing turn {turn}, step {step}: {timestamp}\n\
             Terminal time zone for this request: {}. Interpret otherwise-unqualified dates and times in this zone.\n\
             Elapsed since the preceding {baseline}: {elapsed}.",
            self.zone_name,
        );
        let source = MessageSource::plugin_snapshot(
            TIME_CONTEXT_SOURCE,
            vec![ContextSnapshotSection {
                name: TIME_CONTEXT_SOURCE.to_owned(),
                text: text.clone(),
            }],
        )
        .map_err(|_| TimeContextError::Message)?;
        let content = ContentBlock::text(text).map_err(|_| TimeContextError::Message)?;
        let id = runtime.next_id(AgentIdKind::Message)?;
        Message::user(id, vec![content], source).map_err(|_| TimeContextError::Message)
    }
}

fn format_timestamp(now: UnixMillis, zone: &TimeZone) -> Result<String, TimeContextError> {
    let whole_seconds = now.get().div_euclid(1_000).saturating_mul(1_000);
    let timestamp = Timestamp::from_millisecond(whole_seconds)
        .map_err(|_| TimeContextError::Format)?
        .to_zoned(zone.clone());
    strtime::format("%Y-%m-%dT%H:%M:%S%:z[%Q]", &timestamp).map_err(|_| TimeContextError::Format)
}

fn format_duration(elapsed_ms: i64) -> String {
    let mut seconds = elapsed_ms.max(0) / 1_000;
    let days = seconds / 86_400;
    seconds %= 86_400;
    let hours = seconds / 3_600;
    seconds %= 3_600;
    let minutes = seconds / 60;
    seconds %= 60;
    let mut parts = Vec::with_capacity(4);
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    parts.push(format!("{seconds}s"));
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::{
        agent::{AgentIdKind, AgentRuntime},
        model::{ContentBlock, ContextSnapshotSection, Message, MessageSource, MessageSourceKind},
        session::{
            AppendError, Clock, ClockError, EventKind, EventValidationError, NewEvent, Session,
            StepId, SurfaceError, SurfaceIntent, TurnId, UnixMillis,
        },
    };

    use super::{
        TimeContextClock, TimeContextError, TimeContextRuntime, format_duration, format_timestamp,
    };

    #[derive(Debug)]
    struct FixedClock(Mutex<Vec<i64>>);

    impl TimeContextClock for FixedClock {
        fn now(&self) -> Result<UnixMillis, TimeContextError> {
            let value = self.0.lock().unwrap().remove(0);
            UnixMillis::new(value).map_err(|_| TimeContextError::Clock)
        }
    }

    #[derive(Debug)]
    struct FixedRuntime;

    #[derive(Clone, Copy, Debug)]
    struct SessionClock(i64);

    impl Clock for SessionClock {
        fn now(&self) -> Result<UnixMillis, ClockError> {
            UnixMillis::new(self.0).map_err(|error| ClockError::new(error.to_string()))
        }
    }

    impl AgentRuntime for FixedRuntime {
        fn next_id(&self, kind: AgentIdKind) -> Result<String, crate::agent::AgentRuntimeError> {
            Ok(format!("{}-fixed", kind.prefix()))
        }

        fn sample_unit(&self) -> Result<f64, crate::agent::AgentRuntimeError> {
            Ok(0.5)
        }
    }

    fn runtime(zone: &str, times: &[i64]) -> TimeContextRuntime {
        TimeContextRuntime::with_clock(zone, Arc::new(FixedClock(Mutex::new(times.to_vec()))))
            .unwrap()
    }

    #[test]
    fn canonical_zone_and_dst_formatting_are_exact() {
        assert!(matches!(
            TimeContextRuntime::new("america/NEW_YORK"),
            Err(TimeContextError::NonCanonicalZone { canonical }) if canonical == "America/New_York"
        ));
        assert!(matches!(
            TimeContextRuntime::new("Not/A_Real_Zone"),
            Err(TimeContextError::InvalidZone)
        ));
        let zone = jiff::tz::TimeZone::get("America/New_York").unwrap();
        assert_eq!(
            format_timestamp(UnixMillis::new(1_720_646_365_567).unwrap(), &zone).unwrap(),
            "2024-07-10T17:19:25-04:00[America/New_York]"
        );
        let utc = jiff::tz::TimeZone::get("UTC").unwrap();
        assert_eq!(
            format_timestamp(UnixMillis::new(0).unwrap(), &utc).unwrap(),
            "1970-01-01T00:00:00+00:00[UTC]"
        );
    }

    #[test]
    fn duration_matches_the_fixed_compact_whole_second_contract() {
        assert_eq!(format_duration(-1), "0s");
        assert_eq!(format_duration(999), "0s");
        assert_eq!(format_duration(61_999), "1m 1s");
        assert_eq!(format_duration(90_061_999), "1d 1h 1m 1s");
    }

    #[test]
    fn prepared_message_has_exact_snapshot_source_and_baselines() {
        let mut session =
            Session::with_clock("time-context-unit", SessionClock(1_720_646_365_567)).unwrap();
        let turn = TurnId::first();
        let step = StepId::first();
        session
            .append(NewEvent::log(EventKind::turn_start(turn)))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::step_start(turn, step)))
            .unwrap();
        let prepared = runtime("Asia/Shanghai", &[1_720_646_365_567])
            .prepare(&session, turn, step, &FixedRuntime)
            .unwrap();
        let crate::model::ContentBlockKind::Text { text } = prepared.content()[0].kind() else {
            panic!("time context must contain text")
        };
        assert!(text.contains("Time sampled while preparing turn 1, step 1:"));
        assert!(text.contains("Terminal time zone for this request: Asia/Shanghai."));
        assert!(text.ends_with("Elapsed since the preceding model-visible message: unavailable."));
        let MessageSourceKind::Plugin {
            plugin,
            form,
            sections,
            ..
        } = prepared.source().kind()
        else {
            panic!("time context must retain plugin snapshot ownership")
        };
        assert_eq!(plugin, "time-context");
        assert_eq!(*form, Some(crate::model::ContextForm::Snapshot));
        assert_eq!(sections.as_ref().unwrap()[0].text.as_str(), text.as_str());

        let reading = session
            .append(NewEvent::surface(
                EventKind::user_message(prepared),
                SurfaceIntent::append(),
            ))
            .unwrap();
        let second = StepId::new(2).unwrap();
        session
            .append(NewEvent::log(EventKind::step_end(turn, step)))
            .unwrap();
        let replacement = crate::model::Message::user(
            "summary",
            vec![crate::model::ContentBlock::text("shadowed context").unwrap()],
            crate::model::MessageSource::plugin("test-summary").unwrap(),
        )
        .unwrap();
        session
            .append(NewEvent::surface(
                EventKind::user_message(replacement),
                SurfaceIntent::replace(reading.seq(), reading.seq(), vec![reading.seq()]),
            ))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::step_start(turn, second)))
            .unwrap();
        let later = runtime("Asia/Shanghai", &[1_720_646_426_567])
            .prepare(&session, turn, second, &FixedRuntime)
            .unwrap();
        let crate::model::ContentBlockKind::Text { text } = later.content()[0].kind() else {
            panic!("time context must contain text")
        };
        assert!(text.ends_with("Elapsed since the preceding step context: 1m 1s."));
    }

    #[test]
    fn replay_restores_the_preceding_message_clock_without_process_state() {
        let base = 1_720_646_365_000;
        let mut original = Session::with_clock("time-context-replay", SessionClock(base)).unwrap();
        let turn = TurnId::first();
        let step = StepId::first();
        original
            .append(NewEvent::log(EventKind::turn_start(turn)))
            .unwrap();
        original
            .append(NewEvent::log(EventKind::step_start(turn, step)))
            .unwrap();
        let context = runtime("UTC", &[base])
            .prepare(&original, turn, step, &FixedRuntime)
            .unwrap();
        original
            .append(NewEvent::surface(
                EventKind::user_message(context),
                SurfaceIntent::append(),
            ))
            .unwrap();
        original
            .append(NewEvent::log(EventKind::step_end(turn, step)))
            .unwrap();
        original
            .append(NewEvent::log(EventKind::turn_end(
                turn,
                crate::session::TurnEndReason::Completed,
            )))
            .unwrap();
        let snapshot = original.to_json().unwrap();
        let mut resumed = Session::from_json(&snapshot, SessionClock(base + 61_000)).unwrap();
        let next_turn = TurnId::new(2).unwrap();
        resumed
            .append(NewEvent::log(EventKind::turn_start(next_turn)))
            .unwrap();
        resumed
            .append(NewEvent::log(EventKind::step_start(next_turn, step)))
            .unwrap();

        let next = runtime("UTC", &[base + 61_000])
            .prepare(&resumed, next_turn, step, &FixedRuntime)
            .unwrap();
        let crate::model::ContentBlockKind::Text { text } = next.content()[0].kind() else {
            panic!("time context must contain text")
        };
        assert!(text.ends_with("Elapsed since the preceding model-visible message: 1m 1s."));
    }

    #[test]
    fn malformed_time_context_is_rejected_by_shared_projection_validation() {
        let mut session = Session::with_clock("bad-time-context", SessionClock(1_000)).unwrap();
        let turn = TurnId::first();
        let step = StepId::first();
        session
            .append(NewEvent::log(EventKind::turn_start(turn)))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::step_start(turn, step)))
            .unwrap();
        let text = "Time sampled while preparing turn 1, step 2: 1970-01-01T00:00:01+00:00[UTC]\n\
                    Terminal time zone for this request: UTC. Interpret otherwise-unqualified dates and times in this zone.\n\
                    Elapsed since the preceding model-visible message: unavailable.";
        let message = Message::user(
            "bad-time-context-message",
            vec![ContentBlock::text(text).unwrap()],
            MessageSource::plugin_snapshot(
                "time-context",
                vec![ContextSnapshotSection {
                    name: "time-context".to_owned(),
                    text: text.to_owned(),
                }],
            )
            .unwrap(),
        )
        .unwrap();
        let error = session
            .append(NewEvent::surface(
                EventKind::user_message(message),
                SurfaceIntent::append(),
            ))
            .unwrap_err();
        assert!(matches!(
            error,
            AppendError::Validation(EventValidationError::Surface(
                SurfaceError::InvalidTimeContext
            ))
        ));
    }
}
