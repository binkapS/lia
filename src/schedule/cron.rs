//! Cron expressions: when a job should run, written the way crontab does.
//!
//! People who keep crontabs know a shorter way to say when something
//! should run — "0 9 * * 1-5" means nine in the morning on weekdays. A
//! promise should be understood in the words the person uses, so a cron
//! expression is read here as well as "every half hour".
//!
//! The expression has five fields:
//!
//! ```text
//! minute hour day-of-month month day-of-week
//! ```
//!
//! A sixth field, seconds, is allowed in front of them. Every field takes
//! `*` (every one), a number, a range (`1-5`), a list (`1,3,5`), or a
//! step (`*/15`). Months and days of the week may be named: `jan`,
//! `mon`.

use chrono::{DateTime, Datelike, Days, Local, NaiveDate, NaiveTime, Timelike, Utc};
use serde::{Deserialize, Serialize};

/// How far ahead a cron schedule is looked for, in days.
///
/// Five years is more than enough for anything that can come round at all,
/// including "the 29th of February".
const LOOK_AHEAD_DAYS: i64 = 366 * 5;

/// The names a month may have.
const MONTH_NAMES: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// The names a day of the week may have.
const WEEKDAY_NAMES: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// A cron expression, taken apart into the parts a clock can be asked about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronSpec {
    /// The expression as it was written.
    pub expression: String,
    /// The seconds it can run on, 0–59.
    pub seconds: Vec<u32>,
    /// The minutes it can run on, 0–59.
    pub minutes: Vec<u32>,
    /// The hours it can run in, 0–23.
    pub hours: Vec<u32>,
    /// The days of the month it can run on, 1–31.
    pub days: Vec<u32>,
    /// The months it can run in, 1–12.
    pub months: Vec<u32>,
    /// The days of the week it can run on, 0–6, Sunday being 0.
    pub weekdays: Vec<u32>,
    /// Whether every day of the month was given.
    pub every_day: bool,
    /// Whether every day of the week was given.
    pub every_weekday: bool,
}

impl CronSpec {
    /// The next moment this schedule comes round after `from`.
    ///
    /// Local time is used, as crontabs do: "0 9 * * *" means nine in the
    /// morning wherever the person is.
    pub fn next_after(&self, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let local = from.with_timezone(&Local);
        let start = local.naive_local();
        let mut day = start.date();

        for _ in 0..LOOK_AHEAD_DAYS {
            if self.month_matches(day) && self.day_matches(day) {
                // On the first day, only the times still to come count.
                let threshold = if day == start.date() {
                    Some((start.hour(), start.minute(), start.second()))
                } else {
                    None
                };

                if let Some((hour, minute, second)) = self.first_time_after(threshold) {
                    let naive = day.and_time(NaiveTime::from_hms_opt(hour, minute, second)?);
                    if naive > start {
                        return Some(crate::schedule::when::to_utc(naive));
                    }
                }
            }

            day = day.checked_add_days(Days::new(1))?;
        }

        None
    }

    /// Whether the month of this day is one the schedule allows.
    fn month_matches(&self, day: NaiveDate) -> bool {
        self.months.contains(&day.month())
    }

    /// Whether this day is one the schedule allows.
    ///
    /// When both the day of the month and the day of the week are given,
    /// either one matching is enough — that is what crontab does, and it
    /// is what people expect from "0 0 13 * 5".
    fn day_matches(&self, day: NaiveDate) -> bool {
        let month_day_matches = self.days.contains(&day.day());
        let weekday_matches = self.weekdays.contains(&cron_weekday(day.weekday()));

        match (self.every_day, self.every_weekday) {
            (true, true) => true,
            (true, false) => weekday_matches,
            (false, true) => month_day_matches,
            (false, false) => month_day_matches || weekday_matches,
        }
    }

    /// The first time of day the schedule allows, after a given time.
    ///
    /// Without a threshold, the earliest time of day is wanted.
    fn first_time_after(&self, threshold: Option<(u32, u32, u32)>) -> Option<(u32, u32, u32)> {
        for &hour in &self.hours {
            for &minute in &self.minutes {
                for &second in &self.seconds {
                    let time = (hour, minute, second);
                    if let Some(limit) = threshold
                        && time <= limit
                    {
                        continue;
                    }
                    return Some(time);
                }
            }
        }
        None
    }

    /// A short description, for the model and for the user.
    pub fn describe(&self) -> String {
        let on_the_hour = self.seconds.iter().all(|second| *second == 0);
        let one_time = self.hours.len() == 1 && self.minutes.len() == 1 && on_the_hour;

        if one_time && self.months.len() == 12 {
            let (hour, minute) = (self.hours[0], self.minutes[0]);
            if self.every_day && self.every_weekday {
                return format!("every day at {hour:02}:{minute:02}");
            }
            if self.every_day {
                return format!("at {hour:02}:{minute:02} on {}", self.weekday_names());
            }
            if self.every_weekday {
                return format!("at {hour:02}:{minute:02} on the {}", self.day_names());
            }
        }

        format!("cron {}", self.expression)
    }

    /// The days of the week, named.
    fn weekday_names(&self) -> String {
        let names: Vec<&str> = self
            .weekdays
            .iter()
            .filter_map(|day| WEEKDAY_NAMES.get(*day as usize).copied())
            .collect();

        if names.len() == 7 {
            "every day".to_string()
        } else if names.len() == 5 && self.weekdays.contains(&1) && self.weekdays.contains(&5) {
            "weekdays".to_string()
        } else if names.len() == 2 && self.weekdays.contains(&0) && self.weekdays.contains(&6) {
            "weekends".to_string()
        } else {
            names.join(", ")
        }
    }

    /// The days of the month, named.
    fn day_names(&self) -> String {
        let days: Vec<String> = self.days.iter().map(|day| day.to_string()).collect();
        if days.len() == 1 {
            format!("{}th", days[0])
        } else {
            format!("{}th", days.join(", "))
        }
    }
}

/// The crontab number for a day of the week.
fn cron_weekday(weekday: chrono::Weekday) -> u32 {
    match weekday {
        chrono::Weekday::Sun => 0,
        chrono::Weekday::Mon => 1,
        chrono::Weekday::Tue => 2,
        chrono::Weekday::Wed => 3,
        chrono::Weekday::Thu => 4,
        chrono::Weekday::Fri => 5,
        chrono::Weekday::Sat => 6,
    }
}

/// Read a cron expression.
pub fn parse_cron(text: &str) -> Result<CronSpec, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("the expression is empty".into());
    }

    // The named shortcuts people know from crontab.
    if let Some(expanded) = named_schedule(text) {
        return parse_cron(&expanded);
    }

    let fields: Vec<&str> = text.split_whitespace().collect();
    let (seconds_field, rest) = match fields.len() {
        5 => (None, &fields[..]),
        6 => (Some(fields[0]), &fields[1..]),
        other => {
            return Err(format!(
                "a cron expression has five fields (minute hour day-of-month month day-of-week), not {other}"
            ));
        }
    };

    let (seconds, _) = match seconds_field {
        Some(field) => parse_field(field, 0, 59, &[])?,
        None => (vec![0], true),
    };
    let (minutes, _) = parse_field(rest[0], 0, 59, &[])?;
    let (hours, _) = parse_field(rest[1], 0, 23, &[])?;
    let (days, every_day) = parse_field(rest[2], 1, 31, &[])?;
    let (months, _) = parse_field(rest[3], 1, 12, &MONTH_NAMES)?;
    let (weekdays, every_weekday) = parse_field(rest[4], 0, 6, &WEEKDAY_NAMES)?;

    Ok(CronSpec {
        expression: text.to_string(),
        seconds,
        minutes,
        hours,
        days,
        months,
        weekdays,
        every_day,
        every_weekday,
    })
}

/// The named schedules crontab knows, written out in full.
fn named_schedule(text: &str) -> Option<String> {
    let name = text.trim().to_lowercase();
    let expression = match name.as_str() {
        "@yearly" | "@annually" => "0 0 1 1 *",
        "@monthly" => "0 0 1 * *",
        "@weekly" => "0 0 * * 0",
        "@daily" | "@midnight" => "0 0 * * *",
        "@hourly" => "0 * * * *",
        "@every_minute" | "@minutely" => "* * * * *",
        _ => return None,
    };
    Some(expression.to_string())
}

/// Read one field of an expression.
///
/// Returns the values it allows, and whether it was `*` — a field of `*`
/// means "every one", which counts differently from a field that happens
/// to list every value.
fn parse_field(
    field: &str,
    min: u32,
    max: u32,
    names: &[&str],
) -> Result<(Vec<u32>, bool), String> {
    let field = field.trim();
    if field.is_empty() {
        return Err("a field is empty".into());
    }

    let mut values: Vec<u32> = Vec::new();
    let mut every = false;

    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!("\"{field}\" has an empty part"));
        }

        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step
                    .parse()
                    .map_err(|_| format!("\"{step}\" is not a number of steps"))?;
                if step == 0 {
                    return Err("the step cannot be zero".into());
                }
                (range.trim(), step)
            }
            None => (part, 1),
        };

        let (start, end) = if range == "*" || range == "?" {
            every = true;
            (min, max)
        } else if let Some((from, to)) = range.split_once('-') {
            (value_of(from, names)?, value_of(to, names)?)
        } else {
            let value = value_of(range, names)?;
            // "5/10" means from 5 onwards, every 10.
            if part.contains('/') {
                (value, max)
            } else {
                (value, value)
            }
        };

        // Sunday may be written as 7 as well as 0.
        let limit = if min == 0 && max == 6 { 7 } else { max };
        if start < min || end > limit || start > end {
            return Err(format!(
                "\"{part}\" is not a valid value here (allowed: {min}-{max})"
            ));
        }

        let mut value = start;
        while value <= end {
            values.push(value);
            value += step;
        }
    }

    values.sort_unstable();
    values.dedup();

    // Sunday is both 0 and 7 in crontab.
    if min == 0 && max == 6 {
        for value in values.iter_mut() {
            if *value == 7 {
                *value = 0;
            }
        }
        values.sort_unstable();
        values.dedup();
    }

    Ok((values, every))
}

/// The number a value stands for, named or written.
fn value_of(text: &str, names: &[&str]) -> Result<u32, String> {
    let text = text.trim();
    if let Ok(value) = text.parse::<u32>() {
        return Ok(value);
    }

    let lower = text.to_lowercase();
    if let Some(index) = names.iter().position(|name| *name == lower) {
        return Ok(index as u32 + if names.len() == 12 { 1 } else { 0 });
    }

    Err(format!("\"{text}\" is not a number"))
}

/// Whether the words are a cron expression rather than a sentence.
///
/// Only something shaped like a cron field counts: digits, stars and
/// punctuation, or a name of a month or a day. A phrase of ordinary words
/// is never a schedule.
pub fn looks_like_cron(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    if text.starts_with('@') {
        return named_schedule(text).is_some();
    }

    let fields: Vec<&str> = text.split_whitespace().collect();
    if fields.len() != 5 && fields.len() != 6 {
        return false;
    }
    fields.iter().all(|field| is_cron_field(field))
}

/// Whether one thing could be a field of a cron expression.
fn is_cron_field(field: &str) -> bool {
    if field.is_empty() {
        return false;
    }

    if field.chars().any(|c| c.is_ascii_alphabetic()) {
        // A name is allowed; a word is not.
        let lower = field.to_lowercase();
        return MONTH_NAMES.contains(&lower.as_str()) || WEEKDAY_NAMES.contains(&lower.as_str());
    }

    field
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '*' | '?' | ',' | '-' | '/'))
        && field.chars().any(|c| c.is_ascii_digit() || c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    /// Noon, in local time, so the tests do not depend on the clock's zone.
    fn now() -> DateTime<Utc> {
        Local
            .with_ymd_and_hms(2026, 10, 22, 12, 0, 0)
            .earliest()
            .expect("noon is a time that exists")
            .with_timezone(&Utc)
    }

    #[test]
    fn a_plain_time_is_read() {
        let spec = parse_cron("0 9 * * *").unwrap();
        assert_eq!(spec.minutes, vec![0]);
        assert_eq!(spec.hours, vec![9]);
        assert!(spec.every_day);
        assert!(spec.every_weekday);
    }

    #[test]
    fn a_step_is_read() {
        let spec = parse_cron("*/15 * * * *").unwrap();
        assert_eq!(spec.minutes, vec![0, 15, 30, 45]);
    }

    #[test]
    fn a_range_and_a_list_are_read() {
        let spec = parse_cron("0 9 * * 1-5").unwrap();
        assert_eq!(spec.weekdays, vec![1, 2, 3, 4, 5]);

        let spec = parse_cron("0,30 8,20 * * *").unwrap();
        assert_eq!(spec.minutes, vec![0, 30]);
        assert_eq!(spec.hours, vec![8, 20]);
    }

    #[test]
    fn names_are_read() {
        let spec = parse_cron("0 0 * jan mon").unwrap();
        assert_eq!(spec.months, vec![1]);
        assert_eq!(spec.weekdays, vec![1]);
    }

    #[test]
    fn sunday_is_both_zero_and_seven() {
        let spec = parse_cron("0 0 * * 7").unwrap();
        assert_eq!(spec.weekdays, vec![0]);
    }

    #[test]
    fn a_named_schedule_is_read() {
        let spec = parse_cron("@daily").unwrap();
        assert_eq!(spec.hours, vec![0]);
        assert_eq!(spec.minutes, vec![0]);
    }

    #[test]
    fn a_bad_expression_is_refused() {
        assert!(parse_cron("0 9 * *").is_err());
        assert!(parse_cron("0 99 * * *").is_err());
        assert!(parse_cron("0 9 * * xyz").is_err());
    }

    #[test]
    fn a_sentence_is_not_a_cron_expression() {
        assert!(!looks_like_cron("every 30 minutes"));
        assert!(!looks_like_cron("check the build after deploy"));
        assert!(!looks_like_cron("at 5pm"));
        assert!(looks_like_cron("0 9 * * 1-5"));
        assert!(looks_like_cron("*/5 * * * *"));
        assert!(looks_like_cron("@daily"));
    }

    #[test]
    fn the_next_run_is_the_next_matching_minute() {
        let spec = parse_cron("*/15 * * * *").unwrap();
        let next = spec.next_after(now()).unwrap();
        let local = next.with_timezone(&Local);
        assert_eq!((local.hour(), local.minute()), (12, 15));
    }

    #[test]
    fn the_next_run_is_tomorrow_when_the_day_has_gone() {
        let spec = parse_cron("0 9 * * *").unwrap();
        let next = spec.next_after(now()).unwrap();
        let local = next.with_timezone(&Local);
        assert_eq!((local.hour(), local.minute()), (9, 0));
        assert!(next > now());
    }

    #[test]
    fn a_weekday_only_runs_on_that_weekday() {
        let spec = parse_cron("0 9 * * 1").unwrap();
        let next = spec.next_after(now()).unwrap();
        let local = next.with_timezone(&Local);
        assert_eq!(local.weekday(), chrono::Weekday::Mon);
        assert_eq!((local.hour(), local.minute()), (9, 0));
    }

    #[test]
    fn the_description_is_readable() {
        assert_eq!(
            parse_cron("0 9 * * *").unwrap().describe(),
            "every day at 09:00"
        );
        assert_eq!(
            parse_cron("0 9 * * 1-5").unwrap().describe(),
            "at 09:00 on weekdays"
        );
        assert_eq!(
            parse_cron("0 9 * * 1,3").unwrap().describe(),
            "at 09:00 on mon, wed"
        );
    }
}
