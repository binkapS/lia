//! Schedules whose wall-clock times belong to a specific IANA timezone.
//!
//! Existing [`When`] values keep their host-local behavior. `ZonedSchedule`
//! is the explicit-timezone counterpart for reusable routines and reminders.

use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use super::{When, cron::CronSpec};

const LOOK_AHEAD_DAYS: u64 = 366 * 5;

/// A schedule evaluated in its own IANA timezone, independent of the host.
///
/// The serialized representation is `{ "when": ..., "timezone": "..." }`.
/// Existing `When` and job representations are unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZonedSchedule {
    when: When,
    timezone: Tz,
}

impl ZonedSchedule {
    /// Create a schedule using a validated IANA timezone identifier.
    pub fn new(when: When, timezone: &str) -> Result<Self, String> {
        let timezone = timezone
            .parse::<Tz>()
            .map_err(|_| format!("invalid IANA timezone \"{timezone}\""))?;
        Ok(Self { when, timezone })
    }

    /// The schedule rule.
    pub fn when(&self) -> &When {
        &self.when
    }

    /// The canonical IANA timezone identifier.
    pub fn timezone(&self) -> &str {
        self.timezone.name()
    }

    /// Find the next scheduled instant after `from`.
    ///
    /// A nonexistent local wall time during a forward clock change is
    /// skipped. If a wall time occurs twice during a backward clock change,
    /// its earlier instant is selected. Interval and one-off schedules are
    /// absolute instants and retain their ordinary `When` semantics.
    pub fn next_after(&self, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match &self.when {
            When::Every { .. } | When::Once { .. } => self.when.next_after(from),
            When::Daily { hour, minute } => {
                let time = NaiveTime::from_hms_opt(*hour, *minute, 0).unwrap_or_default();
                self.next_matching_date(
                    from,
                    |_| true,
                    |date, after| self.resolve_after(date.and_time(time), after),
                )
            }
            When::Weekly {
                weekday,
                hour,
                minute,
            } => {
                let time = NaiveTime::from_hms_opt(*hour, *minute, 0).unwrap_or_default();
                self.next_matching_date(
                    from,
                    |date| date.weekday() == *weekday,
                    |date, after| self.resolve_after(date.and_time(time), after),
                )
            }
            When::Cron { spec } => self.next_cron(spec, from),
        }
    }

    fn next_matching_date(
        &self,
        from: DateTime<Utc>,
        matches: impl Fn(NaiveDate) -> bool,
        candidate: impl Fn(NaiveDate, DateTime<Utc>) -> Option<DateTime<Utc>>,
    ) -> Option<DateTime<Utc>> {
        let first_day = from.with_timezone(&self.timezone).date_naive();
        for offset in 0..=LOOK_AHEAD_DAYS {
            let day = first_day.checked_add_days(Days::new(offset))?;
            if matches(day)
                && let Some(next) = candidate(day, from)
            {
                return Some(next);
            }
        }
        None
    }

    fn next_cron(&self, spec: &CronSpec, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.next_matching_date(
            from,
            |date| cron_matches_date(spec, date),
            |date, after| {
                for &hour in &spec.hours {
                    for &minute in &spec.minutes {
                        for &second in &spec.seconds {
                            let time = NaiveTime::from_hms_opt(hour, minute, second)?;
                            if let Some(next) = self.resolve_after(date.and_time(time), after) {
                                return Some(next);
                            }
                        }
                    }
                }
                None
            },
        )
    }

    fn resolve_after(
        &self,
        local: chrono::NaiveDateTime,
        after: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        let instant = match self.timezone.from_local_datetime(&local) {
            chrono::LocalResult::Single(value) => value.with_timezone(&Utc),
            chrono::LocalResult::Ambiguous(first, second) => first.min(second).with_timezone(&Utc),
            chrono::LocalResult::None => return None,
        };
        (instant > after).then_some(instant)
    }
}

fn cron_matches_date(spec: &CronSpec, date: NaiveDate) -> bool {
    let month_day_matches = spec.days.contains(&date.day());
    let weekday_number = match date.weekday() {
        Weekday::Sun => 0,
        Weekday::Mon => 1,
        Weekday::Tue => 2,
        Weekday::Wed => 3,
        Weekday::Thu => 4,
        Weekday::Fri => 5,
        Weekday::Sat => 6,
    };
    let weekday_matches = spec.weekdays.contains(&weekday_number);
    (match (spec.every_day, spec.every_weekday) {
        (true, true) => true,
        (true, false) => weekday_matches,
        (false, true) => month_day_matches,
        (false, false) => month_day_matches || weekday_matches,
    }) && spec.months.contains(&date.month())
}

#[derive(Serialize, Deserialize)]
struct StoredZonedSchedule {
    when: When,
    timezone: String,
}

impl Serialize for ZonedSchedule {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        StoredZonedSchedule {
            when: self.when.clone(),
            timezone: self.timezone().to_string(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ZonedSchedule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let stored = StoredZonedSchedule::deserialize(deserializer)?;
        Self::new(stored.when, &stored.timezone).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap()
    }

    #[test]
    fn timezone_identifier_is_validated() {
        assert!(ZonedSchedule::new(When::Daily { hour: 9, minute: 0 }, "Not/A_Zone").is_err());
    }

    #[test]
    fn nonexistent_wall_time_is_skipped() {
        let schedule = ZonedSchedule::new(
            When::Daily {
                hour: 2,
                minute: 30,
            },
            "America/New_York",
        )
        .unwrap();
        assert_eq!(
            schedule.next_after(utc(2026, 3, 8, 5, 0)),
            Some(utc(2026, 3, 9, 6, 30))
        );
    }

    #[test]
    fn ambiguous_wall_time_uses_earlier_occurrence() {
        let schedule = ZonedSchedule::new(
            When::Daily {
                hour: 1,
                minute: 30,
            },
            "America/New_York",
        )
        .unwrap();
        assert_eq!(
            schedule.next_after(utc(2026, 11, 1, 4, 0)),
            Some(utc(2026, 11, 1, 5, 30))
        );
        assert_eq!(
            schedule.next_after(utc(2026, 11, 1, 5, 40)),
            Some(utc(2026, 11, 2, 6, 30))
        );
    }

    #[test]
    fn cron_uses_the_schedule_timezone() {
        let schedule = ZonedSchedule::new(
            When::Cron {
                spec: super::super::cron::parse_cron("0 9 * * *").unwrap(),
            },
            "Asia/Tokyo",
        )
        .unwrap();
        assert_eq!(
            schedule.next_after(utc(2025, 12, 31, 13, 0)),
            Some(utc(2026, 1, 1, 0, 0))
        );
    }

    #[test]
    fn serialization_round_trips_and_rejects_bad_timezones() {
        let schedule =
            ZonedSchedule::new(When::Daily { hour: 9, minute: 5 }, "Europe/Paris").unwrap();
        let value = serde_json::to_value(&schedule).unwrap();
        assert_eq!(value["timezone"], "Europe/Paris");
        assert_eq!(
            serde_json::from_value::<ZonedSchedule>(value).unwrap(),
            schedule
        );
        assert!(
            serde_json::from_value::<ZonedSchedule>(serde_json::json!({
                "when": { "Daily": { "hour": 9, "minute": 5 } },
                "timezone": "Mars/Olympus"
            }))
            .is_err()
        );
    }

    #[test]
    fn host_timezone_probe() {
        let schedule =
            ZonedSchedule::new(When::Daily { hour: 9, minute: 0 }, "America/New_York").unwrap();
        println!(
            "NEXT_RUN={}",
            schedule
                .next_after(utc(2026, 1, 1, 0, 0))
                .unwrap()
                .to_rfc3339()
        );
    }

    #[test]
    fn next_run_is_independent_of_host_timezone() {
        let executable = std::env::current_exe().unwrap();
        let mut results = Vec::new();
        for host_timezone in ["Pacific/Honolulu", "Asia/Tokyo"] {
            let output = std::process::Command::new(&executable)
                .args([
                    "--exact",
                    "schedule::zoned::tests::host_timezone_probe",
                    "--nocapture",
                ])
                .env("TZ", host_timezone)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "probe failed with TZ={host_timezone}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            let next_run = stdout
                .lines()
                .find_map(|line| line.strip_prefix("NEXT_RUN="))
                .unwrap();
            results.push(next_run.to_string());
        }
        assert_eq!(results[0], results[1]);
        assert_eq!(results[0], "2026-01-01T14:00:00+00:00");
    }

    #[test]
    fn legacy_when_serialization_is_unchanged() {
        let when = When::Daily { hour: 9, minute: 5 };
        assert_eq!(
            serde_json::to_value(when).unwrap(),
            serde_json::json!({ "Daily": { "hour": 9, "minute": 5 } })
        );
    }
}
