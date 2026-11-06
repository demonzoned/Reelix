use serde::{Deserialize, Serialize};

/// Lifecycle state of a persisted background job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

/// Settings key that holds the [`BackgroundPace`].
pub const BACKGROUND_PACE_KEY: &str = "background_work";

/// How often scheduled background work runs. Normal keeps every task's own interval; the slower
/// paces stretch it for small servers that also do other work. A task is never stretched past
/// once a day (or its own interval, when that is longer), so daily refreshes stay daily.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackgroundPace {
    #[default]
    Normal,
    Relaxed,
    Light,
    Minimal,
}

impl BackgroundPace {
    pub const ALL: [Self; 4] = [Self::Normal, Self::Relaxed, Self::Light, Self::Minimal];

    /// How many times longer than normal each task waits between runs.
    pub fn factor(self) -> i64 {
        match self {
            Self::Normal => 1,
            Self::Relaxed => 5,
            Self::Light => 20,
            Self::Minimal => 50,
        }
    }

    /// `interval_secs` at this pace, capped at a day unless the task's own interval is longer.
    pub fn apply(self, interval_secs: i64) -> i64 {
        const DAY: i64 = 86_400;
        let interval = interval_secs.max(1);
        interval.saturating_mul(self.factor()).min(interval.max(DAY))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pace_stretches_frequent_tasks_and_keeps_daily_ones_daily() {
        assert_eq!(BackgroundPace::Normal.apply(30), 30);
        assert_eq!(BackgroundPace::Relaxed.apply(30), 150);
        assert_eq!(BackgroundPace::Light.apply(60), 1_200);
        assert_eq!(BackgroundPace::Minimal.apply(30), 1_500);
        // 12 hours at Light would be 10 days: capped at a day.
        assert_eq!(BackgroundPace::Light.apply(43_200), 86_400);
        // Daily and weekly tasks keep their own interval.
        assert_eq!(BackgroundPace::Minimal.apply(86_400), 86_400);
        assert_eq!(BackgroundPace::Minimal.apply(604_800), 604_800);
        assert_eq!(BackgroundPace::Minimal.apply(0), 50);
    }

    #[test]
    fn pace_round_trips_as_lowercase() {
        for p in BackgroundPace::ALL {
            let s = serde_json::to_string(&p).unwrap();
            assert_eq!(serde_json::from_str::<BackgroundPace>(&s).unwrap(), p);
        }
        assert_eq!(
            serde_json::to_string(&BackgroundPace::Light).unwrap(),
            "\"light\""
        );
    }
}
