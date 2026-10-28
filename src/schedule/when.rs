//! When a job should run.
//!
//! The assistant is asked for things in words — "every half hour", "at
//! five this afternoon", "tomorrow morning" — and this turns them into
//! something a clock can be asked about. A schedule is either a gap
//! between runs, a single moment, or a time of day that comes round
//! again.

use std::time::Duration;

use chrono::{DateTime, Datelike, Days, Local, NaiveTime, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};

use super::cron::{self, CronSpec};

/// A moment, or a rule for finding one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum When {
    /// Every this often, counted from the last run.
    Every {
        /// The gap between runs, in seconds.
        seconds: u64,
    },
    /// Once, at this moment.
    Once {
        /// When it runs, in UTC.
        at: DateTime<Utc>,
    },
    /// Every day, at this time of day (local time).
    Daily {
        /// The hour, 0–23.
        hour: u32,
        /// The minute, 0–59.
        minute: u32,
    },
    /// Every week, on this day, at this time of day (local time).
    Weekly {
        /// The day of the week.
        weekday: Weekday,
        /// The hour, 0–23.
        hour: u32,
        /// The minute, 0–59.
        minute: u32,
    },
    /// At the times a cron expression names.
    Cron {
        /// The expression, taken apart.
        spec: CronSpec,
    },
}

impl When {
    /// The next moment this schedule comes round after `from`.
    ///
    /// A one-off schedule has no next moment: it has run.
    pub fn next_after(&self, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            When::Every { seconds } => {
                let gap = Duration::from_secs(*seconds);
                // A gap of zero would spin, so it is treated as a minute.
                let gap = if gap.is_zero() {
                    Duration::from_secs(60)
                } else {
                    gap
                };
                Some(
                    from + chrono::Duration::from_std(gap)
                        .unwrap_or_else(|_| chrono::Duration::minutes(1)),
                )
            }
            When::Once { at } => {
                if *at > from {
                    Some(*at)
                } else {
                    None
                }
            }
            When::Daily { hour, minute } => Some(next_daily(from, *hour, *minute)),
            When::Weekly {
                weekday,
                hour,
                minute,
            } => Some(next_weekly(from, *weekday, *hour, *minute)),
            When::Cron { spec } => spec.next_after(from),
        }
    }

    /// A short description, for the model and for the user.
    pub fn describe(&self) -> String {
        match self {
            When::Every { seconds } => {
                if *seconds % 3600 == 0 && *seconds >= 3600 {
                    let hours = seconds / 3600;
                    format!("every {hours} hour{}", if hours == 1 { "" } else { "s" })
                } else if *seconds % 60 == 0 {
                    let minutes = seconds / 60;
                    format!(
                        "every {minutes} minute{}",
                        if minutes == 1 { "" } else { "s" }
                    )
                } else {
                    format!("every {seconds} seconds")
                }
            }
            When::Once { at } => format!("once, at {}", at.format("%Y-%m-%d %H:%M UTC")),
            When::Daily { hour, minute } => {
                format!("every day at {:02}:{:02}", hour, minute)
            }
            When::Weekly {
                weekday,
                hour,
                minute,
            } => format!("every {weekday:?} at {:02}:{:02}", hour, minute),
            When::Cron { spec } => spec.describe(),
        }
    }
}

/// The next moment a daily time comes round, in local time.
fn next_daily(from: DateTime<Utc>, hour: u32, minute: u32) -> DateTime<Utc> {
    let local = from.with_timezone(&Local);
    let today = local.date_naive();

    let candidate = today.and_time(NaiveTime::from_hms_opt(hour, minute, 0).unwrap_or_default());
    let next = if candidate > local.naive_local() {
        candidate
    } else {
        match today.checked_add_days(Days::new(1)) {
            Some(day) => day.and_time(NaiveTime::from_hms_opt(hour, minute, 0).unwrap_or_default()),
            None => candidate,
        }
    };

    to_utc(next)
}

/// The next moment a weekly time comes round, in local time.
fn next_weekly(from: DateTime<Utc>, weekday: Weekday, hour: u32, minute: u32) -> DateTime<Utc> {
    let local = from.with_timezone(&Local);
    let today = local.date_naive();
    let time = NaiveTime::from_hms_opt(hour, minute, 0).unwrap_or_default();

    // Walk forward a day at a time until the right day turns up, at most a
    // week.
    for offset in 0..=7 {
        let Some(day) = today.checked_add_days(Days::new(offset)) else {
            break;
        };
        if day.weekday() != weekday {
            continue;
        }
        let candidate = day.and_time(time);
        if candidate > local.naive_local() {
            return to_utc(candidate);
        }
    }

    to_utc(today.and_time(time))
}

/// Turn a local time into a moment in time.
///
/// A local time can be missing from the clock on the day the clocks go
/// forward; the first moment after the gap is used instead, so a job
/// still runs.
pub(crate) fn to_utc(naive: chrono::NaiveDateTime) -> DateTime<Utc> {
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt.with_timezone(&Utc),
        chrono::LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc),
        chrono::LocalResult::None => {
            // The clock skipped this time; take the moment just after it.
            let after = naive + chrono::Duration::hours(1);
            match Local.from_local_datetime(&after) {
                chrono::LocalResult::Single(dt) => dt.with_timezone(&Utc),
                chrono::LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc),
                chrono::LocalResult::None => Utc.from_utc_datetime(&after),
            }
        }
    }
}

/// Parse a schedule from words.
///
/// The words are the ones a person uses: "every 30 minutes", "at 5pm",
/// "daily at 09:00", "in two hours". The result is the schedule itself;
/// the caller works out when it first comes round.
pub fn parse_when(text: &str, now: DateTime<Utc>) -> Result<When, String> {
    let text = text.trim().to_lowercase();
    if text.is_empty() {
        return Err("say when it should run, for example \"every 30 minutes\"".into());
    }

    // "0 9 * * 1-5" — a cron expression, said the way a crontab says it.
    if cron::looks_like_cron(&text) {
        let spec = cron::parse_cron(&text).map_err(|error| {
            format!("cannot understand the cron expression \"{text}\": {error}")
        })?;
        return Ok(When::Cron { spec });
    }

    // "in 20 minutes" — a moment, a bit from now.
    if let Some(rest) = text.strip_prefix("in ") {
        let gap = parse_duration(rest.trim())
            .ok_or_else(|| format!("cannot understand \"{}\"", rest.trim()))?;
        return Ok(When::Once {
            at: now
                + chrono::Duration::from_std(gap).unwrap_or_else(|_| chrono::Duration::minutes(1)),
        });
    }

    // "every 30 minutes" — a gap between runs.
    if let Some(rest) = text.strip_prefix("every ") {
        let rest = rest.trim();
        if let Some(gap) = parse_duration(rest) {
            return Ok(When::Every {
                seconds: gap.as_secs().max(1),
            });
        }
        // "every day at 09:00", "every monday at 9am".
        if let Some(schedule) = day_and_time(rest) {
            return Ok(schedule);
        }
        return Err(format!("cannot understand \"every {rest}\""));
    }

    // "hourly", "daily", "weekly" — with or without a time.
    if text == "hourly" {
        return Ok(When::Every { seconds: 3600 });
    }
    if text == "daily" {
        return Ok(When::Daily { hour: 9, minute: 0 });
    }
    if text == "weekly" {
        return Ok(When::Weekly {
            weekday: Weekday::Mon,
            hour: 9,
            minute: 0,
        });
    }

    // "tomorrow 9am" — the next morning.
    if let Some(rest) = text.strip_prefix("tomorrow") {
        let rest = rest.trim();
        let (hour, minute) = parse_time(rest)
            .or_else(|| split_at_time(rest).and_then(|(_, time)| parse_time(time)))
            .ok_or_else(|| format!("cannot understand \"tomorrow {rest}\""))?;
        let local = now.with_timezone(&Local);
        let tomorrow = local
            .date_naive()
            .checked_add_days(Days::new(1))
            .unwrap_or_else(|| local.date_naive());
        return Ok(When::Once {
            at: to_utc(
                tomorrow.and_time(NaiveTime::from_hms_opt(hour, minute, 0).unwrap_or_default()),
            ),
        });
    }

    // "daily at 09:00", "monday at 9am".
    if let Some(schedule) = day_and_time(&text) {
        return Ok(schedule);
    }

    // "at 5pm" — today, or tomorrow if that has passed.
    if let Some(rest) = text.strip_prefix("at ")
        && let Some((hour, minute)) = parse_time(rest.trim())
    {
        let local = now.with_timezone(&Local);
        let time = NaiveTime::from_hms_opt(hour, minute, 0).unwrap_or_default();
        let today = local.date_naive().and_time(time);
        let at = if today > local.naive_local() {
            today
        } else {
            match local.date_naive().checked_add_days(Days::new(1)) {
                Some(day) => day.and_time(time),
                None => today,
            }
        };
        return Ok(When::Once { at: to_utc(at) });
    }

    // A moment written out in full.
    if let Ok(at) = DateTime::parse_from_rfc3339(&text) {
        return Ok(When::Once {
            at: at.with_timezone(&Utc),
        });
    }

    Err(format!("cannot understand \"{text}\""))
}

/// Split "day at time" into its two parts.
fn split_at_time(text: &str) -> Option<(&str, &str)> {
    let text = text.trim();
    let (day, time) = text.split_once(" at ")?;
    Some((day.trim(), time.trim()))
}

/// A schedule named by a day and a time: "daily at 09:00", "monday at 9am".
fn day_and_time(text: &str) -> Option<When> {
    let (day_part, time_part) = split_at_time(text)?;
    let (hour, minute) = parse_time(time_part)?;
    match day_part {
        "" | "day" | "daily" | "every day" => Some(When::Daily { hour, minute }),
        other => parse_weekday(other).map(|weekday| When::Weekly {
            weekday,
            hour,
            minute,
        }),
    }
}

/// Parse a length of time: "30 minutes", "2h", "1 day", "hour".
fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();

    // The number, if there is one. "every hour" means one hour.
    let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    let value: u64 = if digits.is_empty() {
        1
    } else {
        digits.parse().ok()?
    };
    let unit = text[digits.len()..].trim();

    let seconds = match unit {
        // A bare number is minutes, which is what people mean.
        "" => value * 60,
        "s" | "sec" | "secs" | "second" | "seconds" => value,
        "m" | "min" | "mins" | "minute" | "minutes" => value * 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => value * 3600,
        "d" | "day" | "days" => value * 86_400,
        "w" | "week" | "weeks" => value * 604_800,
        _ => return None,
    };
    Some(Duration::from_secs(seconds))
}

/// Parse a time of day: "9", "09:30", "5pm", "5:30 pm".
fn parse_time(text: &str) -> Option<(u32, u32)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    // A trailing "am" or "pm".
    let (text, meridiem) = if let Some(rest) = text.strip_suffix("am") {
        (rest.trim(), Some(false))
    } else if let Some(rest) = text.strip_suffix("pm") {
        (rest.trim(), Some(true))
    } else {
        (text, None)
    };

    let (hour_text, minute_text) = match text.split_once(':') {
        Some((h, m)) => (h.trim(), m.trim()),
        None => (text, "0"),
    };

    let mut hour: u32 = hour_text.parse().ok()?;
    let minute: u32 = minute_text.parse().ok()?;
    if minute > 59 {
        return None;
    }

    if let Some(afternoon) = meridiem {
        if !(1..=12).contains(&hour) {
            return None;
        }
        hour = match (hour, afternoon) {
            (12, false) => 0,
            (12, true) => 12,
            (h, true) => h + 12,
            (h, false) => h,
        };
    }

    if hour > 23 {
        return None;
    }
    Some((hour, minute))
}

/// Parse a weekday: "monday", "mon", "tues".
fn parse_weekday(text: &str) -> Option<Weekday> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("every ") {
        return parse_weekday(rest);
    }
    match text {
        "monday" | "mon" => Some(Weekday::Mon),
        "tuesday" | "tue" | "tues" => Some(Weekday::Tue),
        "wednesday" | "wed" => Some(Weekday::Wed),
        "thursday" | "thu" | "thur" | "thurs" => Some(Weekday::Thu),
        "friday" | "fri" => Some(Weekday::Fri),
        "saturday" | "sat" => Some(Weekday::Sat),
        "sunday" | "sun" => Some(Weekday::Sun),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 22, 12, 0, 0).unwrap()
    }

    #[test]
    fn every_minutes_is_a_gap() {
        let when = parse_when("every 30 minutes", now()).unwrap();
        assert_eq!(when, When::Every { seconds: 1800 });
    }

    #[test]
    fn every_hours_is_a_gap() {
        assert_eq!(
            parse_when("every 2h", now()).unwrap(),
            When::Every { seconds: 7200 }
        );
    }

    #[test]
    fn a_gap_moves_forward() {
        let when = When::Every { seconds: 600 };
        let next = when.next_after(now()).unwrap();
        assert_eq!(next, now() + chrono::Duration::minutes(10));
    }

    #[test]
    fn a_daily_time_comes_round_tomorrow_when_it_has_passed() {
        let when = When::Daily { hour: 9, minute: 0 };
        let next = when.next_after(now()).unwrap();
        let local = next.with_timezone(&Local);
        assert_eq!(local.hour(), 9);
        assert_eq!(local.minute(), 0);
        assert!(next > now());
    }

    #[test]
    fn a_daily_time_comes_round_today_when_it_has_not() {
        let when = When::Daily {
            hour: 23,
            minute: 30,
        };
        let next = when.next_after(now()).unwrap();
        let local = next.with_timezone(&Local);
        assert_eq!((local.hour(), local.minute()), (23, 30));
        assert!(next > now());
    }

    #[test]
    fn a_one_off_runs_only_once() {
        let when = When::Once {
            at: now() + chrono::Duration::hours(1),
        };
        assert!(when.next_after(now()).is_some());
        assert!(
            when.next_after(now() + chrono::Duration::hours(2))
                .is_none()
        );
    }

    #[test]
    fn times_are_read_the_way_people_say_them() {
        assert_eq!(parse_time("9"), Some((9, 0)));
        assert_eq!(parse_time("09:30"), Some((9, 30)));
        assert_eq!(parse_time("5pm"), Some((17, 0)));
        assert_eq!(parse_time("5:30pm"), Some((17, 30)));
        assert_eq!(parse_time("12am"), Some((0, 0)));
        assert_eq!(parse_time("12pm"), Some((12, 0)));
        assert_eq!(parse_time("noon"), None);
    }

    #[test]
    fn a_time_in_the_past_is_tomorrow() {
        let when = parse_when("at 5pm", now()).unwrap();
        let When::Once { at } = when else {
            panic!("expected a single moment");
        };
        let local = at.with_timezone(&Local);
        assert_eq!(local.hour(), 17);
        assert!(at > now());
    }

    #[test]
    fn daily_at_a_time_is_daily() {
        let when = parse_when("daily at 08:30", now()).unwrap();
        assert_eq!(
            when,
            When::Daily {
                hour: 8,
                minute: 30
            }
        );
    }

    #[test]
    fn a_weekday_is_weekly() {
        let when = parse_when("every monday at 9am", now()).unwrap();
        assert_eq!(
            when,
            When::Weekly {
                weekday: Weekday::Mon,
                hour: 9,
                minute: 0
            }
        );
    }

    #[test]
    fn in_a_while_is_a_single_moment() {
        let when = parse_when("in 20 minutes", now()).unwrap();
        let When::Once { at } = when else {
            panic!("expected a single moment");
        };
        assert_eq!(at, now() + chrono::Duration::minutes(20));
    }

    #[test]
    fn nonsense_is_refused() {
        assert!(parse_when("whenever", now()).is_err());
        assert!(parse_when("", now()).is_err());
    }
}
