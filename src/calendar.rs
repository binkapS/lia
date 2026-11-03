//! Normalized calendar event and time model (roadmap F7).
//!
//! This module is network-free and format-agnostic: callers (a future CalDAV
//! client, an `.ics` importer, tests) fill a [`RawEvent`] with strings and
//! [`CalendarEvent::normalize`] turns it into a validated event.
//!
//! # Supported time inputs
//!
//! | input                                 | meaning                                   |
//! |---------------------------------------|-------------------------------------------|
//! | `2026-03-29`, `20260329`              | all-day date (end is exclusive)           |
//! | `2026-03-29T10:00:00Z`, `...+02:00`   | absolute instant (RFC 3339)               |
//! | `20260329T100000Z`                    | absolute UTC instant (iCalendar basic)    |
//! | `2026-03-29T10:00:00` + `tzid`        | wall time in an IANA zone                 |
//! | `2026-03-29T10:00:00` without `tzid`  | floating; needs a caller default zone     |
//!
//! # Explicitly unsupported (reported as [`CalendarError`])
//!
//! - Parsing iCalendar `RRULE`, `RDATE`, `EXDATE` or per-instance overrides.
//!   Recurrence is only the bounded [`Recurrence`] value below: every N days
//!   or every N weeks at the same local wall time, limited by a count
//!   (at most [`MAX_OCCURRENCES`]) or an `until` instant. There is no
//!   `BYDAY`/`BYMONTH` support.
//! - Non-IANA TZIDs such as Windows names or custom `VTIMEZONE` definitions.
//! - Mixing all-day and timed start/end, or a `tzid` on an all-day or
//!   offset-carrying value.
//! - Wall times that do not exist (inside a spring-forward gap) as an event
//!   start or end. Recurring occurrences landing in a gap are shifted one
//!   hour forward; ambiguous wall times resolve to the earlier instant.
//!
//! All-day events have no instant of their own: when filtering or displaying,
//! each date spans local midnight to local midnight in the *query* timezone.

use std::fmt;

use chrono::{
    DateTime, Days, Duration, FixedOffset, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone,
    Utc,
};
use chrono_tz::Tz;

/// Upper bound on occurrences expanded from one recurring event.
pub const MAX_OCCURRENCES: u32 = 1000;

/// Why an event, time or query could not be normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalendarError {
    MissingField(&'static str),
    InvalidTime(String),
    UnknownTimezone(String),
    /// A floating time was given and no default timezone was supplied.
    FloatingTimeWithoutZone(String),
    /// `tzid` supplied alongside a date, UTC or offset-carrying value.
    TimezoneConflict(String),
    /// The local wall time falls in a daylight-saving gap.
    NonexistentLocalTime(String),
    MixedAllDayAndTimed,
    EndBeforeStart,
    InvalidRecurrence(String),
    InvalidWindow,
}

impl fmt::Display for CalendarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingField(name) => write!(f, "event is missing required field `{name}`"),
            Self::InvalidTime(v) => write!(f, "unsupported or malformed time \"{v}\""),
            Self::UnknownTimezone(v) => write!(f, "unknown IANA timezone \"{v}\""),
            Self::FloatingTimeWithoutZone(v) => {
                write!(f, "floating time \"{v}\" needs a timezone")
            }
            Self::TimezoneConflict(v) => {
                write!(f, "timezone id cannot be combined with \"{v}\"")
            }
            Self::NonexistentLocalTime(v) => {
                write!(f, "local time \"{v}\" does not exist in its timezone")
            }
            Self::MixedAllDayAndTimed => {
                write!(f, "start and end must both be dates or both timed")
            }
            Self::EndBeforeStart => write!(f, "event ends before it starts"),
            Self::InvalidRecurrence(why) => write!(f, "invalid recurrence: {why}"),
            Self::InvalidWindow => write!(f, "query window end must be after its start"),
        }
    }
}

impl std::error::Error for CalendarError {}

/// A time value as received from a calendar source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTime {
    pub value: String,
    pub tzid: Option<String>,
}

impl RawTime {
    pub fn new(value: &str, tzid: Option<&str>) -> Self {
        Self {
            value: value.to_string(),
            tzid: tzid.map(str::to_string),
        }
    }
}

/// An unvalidated event as received from a calendar source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawEvent {
    pub source: String,
    pub calendar_id: String,
    pub uid: String,
    pub summary: String,
    pub start: Option<RawTime>,
    pub end: Option<RawTime>,
    pub recurrence: Option<Recurrence>,
}

/// The zone an event was originally expressed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventZone {
    Utc,
    Offset(FixedOffset),
    Iana(Tz),
    /// Source gave no zone; the caller's default IANA zone was applied.
    Floating(Tz),
}

impl EventZone {
    /// Human-readable label, e.g. `UTC`, `+02:00`, `Europe/Berlin`.
    pub fn label(&self) -> String {
        match self {
            Self::Utc => "UTC".to_string(),
            Self::Offset(o) => o.to_string(),
            Self::Iana(tz) => tz.name().to_string(),
            Self::Floating(tz) => format!("floating ({})", tz.name()),
        }
    }

    fn local_of(&self, at: DateTime<Utc>) -> NaiveDateTime {
        match self {
            Self::Utc => at.naive_utc(),
            Self::Offset(o) => at.with_timezone(o).naive_local(),
            Self::Iana(tz) | Self::Floating(tz) => at.with_timezone(tz).naive_local(),
        }
    }

    fn resolve(&self, local: NaiveDateTime) -> LocalResult<DateTime<Utc>> {
        match self {
            Self::Utc => LocalResult::Single(local.and_utc()),
            Self::Offset(o) => to_utc(o.from_local_datetime(&local)),
            Self::Iana(tz) | Self::Floating(tz) => to_utc(tz.from_local_datetime(&local)),
        }
    }

    /// Earlier instant if ambiguous; one hour later if in a gap.
    fn resolve_lenient(&self, local: NaiveDateTime) -> Option<DateTime<Utc>> {
        match self.resolve(local) {
            LocalResult::Single(v) => Some(v),
            LocalResult::Ambiguous(a, b) => Some(a.min(b)),
            LocalResult::None => {
                match self.resolve(local.checked_add_signed(Duration::hours(1))?) {
                    LocalResult::Single(v) => Some(v),
                    LocalResult::Ambiguous(a, b) => Some(a.min(b)),
                    LocalResult::None => None,
                }
            }
        }
    }
}

/// A normalized start or end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventTime {
    /// A calendar date with no time of day. As an end it is exclusive.
    AllDay(NaiveDate),
    Instant(DateTime<Utc>),
}

/// Bound on a [`Recurrence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceLimit {
    /// Total occurrences including the first (1..=[`MAX_OCCURRENCES`]).
    Count(u32),
    /// No occurrence may start after this instant (inclusive).
    Until(DateTime<Utc>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    Daily,
    Weekly,
}

/// Bounded recurrence: every `interval` days/weeks at the same local time
/// of the event's original zone. Always limited; see module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recurrence {
    pub frequency: Frequency,
    pub interval: u32,
    pub limit: RecurrenceLimit,
}

impl Recurrence {
    fn validate(&self) -> Result<(), CalendarError> {
        if self.interval == 0 {
            return Err(CalendarError::InvalidRecurrence(
                "interval must be >= 1".into(),
            ));
        }
        if let RecurrenceLimit::Count(n) = self.limit
            && (n == 0 || n > MAX_OCCURRENCES)
        {
            return Err(CalendarError::InvalidRecurrence(format!(
                "count must be 1..={MAX_OCCURRENCES}"
            )));
        }
        Ok(())
    }

    fn step_days(&self) -> u64 {
        u64::from(self.interval)
            * if self.frequency == Frequency::Weekly {
                7
            } else {
                1
            }
    }
}

/// Half-open query window `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Window {
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Self, CalendarError> {
        if end <= start {
            return Err(CalendarError::InvalidWindow);
        }
        Ok(Self { start, end })
    }
}

/// A validated calendar event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    source: String,
    calendar_id: String,
    uid: String,
    summary: String,
    start: EventTime,
    end: EventTime,
    zone: Option<EventZone>,
    recurrence: Option<Recurrence>,
}

/// One concrete instance of an event (the only one unless recurring).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence<'a> {
    pub event: &'a CalendarEvent,
    pub start: EventTime,
    pub end: EventTime,
    /// Zero-based position in the recurrence.
    pub index: u32,
}

enum Parsed {
    Date(NaiveDate),
    Instant(DateTime<Utc>, EventZone),
    Local(NaiveDateTime, EventZone),
}

fn parse_time(raw: &RawTime, floating_default: Option<&str>) -> Result<Parsed, CalendarError> {
    let v = raw.value.trim();
    let conflict = || CalendarError::TimezoneConflict(v.to_string());
    let bad = || CalendarError::InvalidTime(v.to_string());

    let date = NaiveDate::parse_from_str(v, "%Y-%m-%d").ok().or_else(|| {
        (v.len() == 8)
            .then(|| NaiveDate::parse_from_str(v, "%Y%m%d").ok())
            .flatten()
    });
    if let Some(d) = date {
        return if raw.tzid.is_some() {
            Err(conflict())
        } else {
            Ok(Parsed::Date(d))
        };
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(v) {
        if raw.tzid.is_some() {
            return Err(conflict());
        }
        let zone = if v.ends_with('Z') || v.ends_with('z') {
            EventZone::Utc
        } else {
            EventZone::Offset(*dt.offset())
        };
        return Ok(Parsed::Instant(dt.with_timezone(&Utc), zone));
    }
    if let Some(basic) = v.strip_suffix('Z')
        && let Ok(n) = NaiveDateTime::parse_from_str(basic, "%Y%m%dT%H%M%S")
    {
        if raw.tzid.is_some() {
            return Err(conflict());
        }
        return Ok(Parsed::Instant(n.and_utc(), EventZone::Utc));
    }
    let naive = NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S"))
        .map_err(|_| bad())?;
    let zone = match (&raw.tzid, floating_default) {
        (Some(id), _) => EventZone::Iana(parse_tz(id)?),
        (None, Some(id)) => EventZone::Floating(parse_tz(id)?),
        (None, None) => return Err(CalendarError::FloatingTimeWithoutZone(v.to_string())),
    };
    Ok(Parsed::Local(naive, zone))
}

fn to_utc<Z: TimeZone>(r: LocalResult<DateTime<Z>>) -> LocalResult<DateTime<Utc>> {
    match r {
        LocalResult::Single(v) => LocalResult::Single(v.with_timezone(&Utc)),
        LocalResult::Ambiguous(a, b) => {
            LocalResult::Ambiguous(a.with_timezone(&Utc), b.with_timezone(&Utc))
        }
        LocalResult::None => LocalResult::None,
    }
}

fn parse_tz(id: &str) -> Result<Tz, CalendarError> {
    id.trim()
        .parse::<Tz>()
        .map_err(|_| CalendarError::UnknownTimezone(id.to_string()))
}

fn required(value: &str, name: &'static str) -> Result<String, CalendarError> {
    let value = value.trim();
    if value.is_empty() {
        Err(CalendarError::MissingField(name))
    } else {
        Ok(value.to_string())
    }
}

impl CalendarEvent {
    /// Validate and normalize a raw event.
    ///
    /// `floating_default` is the IANA zone applied to zoneless wall times;
    /// pass `None` to reject them. A missing end means one day for all-day
    /// events and a zero-length event for timed ones.
    pub fn normalize(raw: RawEvent, floating_default: Option<&str>) -> Result<Self, CalendarError> {
        let source = required(&raw.source, "source")?;
        let calendar_id = required(&raw.calendar_id, "calendar_id")?;
        let uid = required(&raw.uid, "uid")?;
        let summary = required(&raw.summary, "summary")?;
        let start_raw = raw
            .start
            .as_ref()
            .ok_or(CalendarError::MissingField("start"))?;
        if let Some(r) = &raw.recurrence {
            r.validate()?;
        }

        let start_parsed = parse_time(start_raw, floating_default)?;
        let end_parsed = raw
            .end
            .as_ref()
            .map(|e| parse_time(e, floating_default))
            .transpose()?;

        let (start, end, zone) = match (start_parsed, end_parsed) {
            (Parsed::Date(s), None) => (
                EventTime::AllDay(s),
                EventTime::AllDay(
                    s.checked_add_days(Days::new(1))
                        .ok_or_else(|| CalendarError::InvalidTime(start_raw.value.clone()))?,
                ),
                None,
            ),
            (Parsed::Date(s), Some(Parsed::Date(e))) => {
                (EventTime::AllDay(s), EventTime::AllDay(e), None)
            }
            (Parsed::Date(_), Some(_)) | (_, Some(Parsed::Date(_))) => {
                return Err(CalendarError::MixedAllDayAndTimed);
            }
            (s, e) => {
                let (s_utc, zone) = resolve_strict(s, &start_raw.value)?;
                let e_utc = match e {
                    None => s_utc,
                    Some(e) => resolve_strict(e, raw.end.as_ref().map_or("", |t| &t.value))?.0,
                };
                (
                    EventTime::Instant(s_utc),
                    EventTime::Instant(e_utc),
                    Some(zone),
                )
            }
        };
        if instant_key(&end) < instant_key(&start) {
            return Err(CalendarError::EndBeforeStart);
        }
        Ok(Self {
            source,
            calendar_id,
            uid,
            summary,
            start,
            end,
            zone,
            recurrence: raw.recurrence,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn calendar_id(&self) -> &str {
        &self.calendar_id
    }
    pub fn uid(&self) -> &str {
        &self.uid
    }
    pub fn summary(&self) -> &str {
        &self.summary
    }
    pub fn start(&self) -> EventTime {
        self.start
    }
    pub fn end(&self) -> EventTime {
        self.end
    }
    pub fn is_all_day(&self) -> bool {
        matches!(self.start, EventTime::AllDay(_))
    }
    /// Original zone of the start; `None` for all-day events.
    pub fn original_zone(&self) -> Option<EventZone> {
        self.zone
    }
    pub fn recurrence(&self) -> Option<&Recurrence> {
        self.recurrence.as_ref()
    }

    /// Occurrences overlapping `window`, evaluated for `query_tz` (an IANA
    /// name used to place all-day dates). Zero-length events match when their
    /// instant lies inside the window.
    pub fn occurrences_in(
        &self,
        window: &Window,
        query_tz: &str,
    ) -> Result<Vec<Occurrence<'_>>, CalendarError> {
        let qtz = parse_tz(query_tz)?;
        let mut out = Vec::new();
        let limit = match self.recurrence.map(|r| r.limit) {
            Some(RecurrenceLimit::Count(n)) => n,
            _ => MAX_OCCURRENCES,
        };
        let step = self.recurrence.map_or(0, |r| r.step_days());
        for index in 0..if self.recurrence.is_some() { limit } else { 1 } {
            let Some((start, end)) = self.nth(index, step) else {
                break;
            };
            if let Some(RecurrenceLimit::Until(until)) = self.recurrence.map(|r| r.limit)
                && index > 0
                && start_instant(&start, qtz).is_none_or(|s| s > until)
            {
                break;
            }
            let (Some(s), Some(e)) = (start_instant(&start, qtz), start_instant(&end, qtz)) else {
                break;
            };
            if s >= window.end {
                break;
            }
            let overlaps = if e > s {
                e > window.start
            } else {
                s >= window.start
            };
            if overlaps {
                out.push(Occurrence {
                    event: self,
                    start,
                    end,
                    index,
                });
            }
        }
        Ok(out)
    }

    fn nth(&self, index: u32, step_days: u64) -> Option<(EventTime, EventTime)> {
        if index == 0 {
            return Some((self.start, self.end));
        }
        let shift = Days::new(step_days.checked_mul(u64::from(index))?);
        match (self.start, self.end, self.zone) {
            (EventTime::AllDay(s), EventTime::AllDay(e), _) => Some((
                EventTime::AllDay(s.checked_add_days(shift)?),
                EventTime::AllDay(e.checked_add_days(shift)?),
            )),
            (EventTime::Instant(s), EventTime::Instant(e), Some(zone)) => {
                let local = zone.local_of(s).checked_add_days(shift)?;
                let start = zone.resolve_lenient(local)?;
                Some((
                    EventTime::Instant(start),
                    EventTime::Instant(start + (e - s)),
                ))
            }
            _ => None,
        }
    }

    /// Render the event for a user in `display_tz`, e.g.
    /// `Standup: 2026-03-29 10:00–10:30 Europe/Berlin (+02:00)` or
    /// `Holiday: 2026-12-24–2026-12-26 (all day)`.
    pub fn display_in(&self, display_tz: &str) -> Result<String, CalendarError> {
        let tz = parse_tz(display_tz)?;
        Ok(format!(
            "{}: {}",
            self.summary,
            format_span(&self.start, &self.end, tz)
        ))
    }
}

impl Occurrence<'_> {
    /// Like [`CalendarEvent::display_in`] for this occurrence.
    pub fn display_in(&self, display_tz: &str) -> Result<String, CalendarError> {
        let tz = parse_tz(display_tz)?;
        Ok(format!(
            "{}: {}",
            self.event.summary,
            format_span(&self.start, &self.end, tz)
        ))
    }
}

/// Occurrences of all `events` overlapping `window`, ordered by start instant
/// (then UID). See [`CalendarEvent::occurrences_in`].
pub fn occurrences_in_window<'a>(
    events: &'a [CalendarEvent],
    window: &Window,
    query_tz: &str,
) -> Result<Vec<Occurrence<'a>>, CalendarError> {
    let qtz = parse_tz(query_tz)?;
    let mut all = Vec::new();
    for event in events {
        all.extend(event.occurrences_in(window, query_tz)?);
    }
    all.sort_by_cached_key(|o| (start_instant(&o.start, qtz), o.event.uid.clone(), o.index));
    Ok(all)
}

fn resolve_strict(p: Parsed, original: &str) -> Result<(DateTime<Utc>, EventZone), CalendarError> {
    match p {
        Parsed::Instant(at, zone) => Ok((at, zone)),
        Parsed::Local(local, zone) => match zone.resolve(local) {
            LocalResult::Single(v) => Ok((v, zone)),
            LocalResult::Ambiguous(a, b) => Ok((a.min(b), zone)),
            LocalResult::None => Err(CalendarError::NonexistentLocalTime(original.to_string())),
        },
        Parsed::Date(_) => Err(CalendarError::MixedAllDayAndTimed),
    }
}

/// Ordering key valid within one event (both ends are the same kind).
fn instant_key(t: &EventTime) -> (Option<DateTime<Utc>>, Option<NaiveDate>) {
    match t {
        EventTime::Instant(i) => (Some(*i), None),
        EventTime::AllDay(d) => (None, Some(*d)),
    }
}

fn start_instant(t: &EventTime, qtz: Tz) -> Option<DateTime<Utc>> {
    match t {
        EventTime::Instant(i) => Some(*i),
        EventTime::AllDay(d) => EventZone::Iana(qtz).resolve_lenient(d.and_hms_opt(0, 0, 0)?),
    }
}

fn format_span(start: &EventTime, end: &EventTime, tz: Tz) -> String {
    match (start, end) {
        (EventTime::AllDay(s), EventTime::AllDay(e)) => {
            let last = e.pred_opt().filter(|l| l > s);
            match last {
                Some(l) => format!("{s}–{l} (all day)"),
                None => format!("{s} (all day)"),
            }
        }
        (EventTime::Instant(s), EventTime::Instant(e)) => {
            let (ls, le) = (s.with_timezone(&tz), e.with_timezone(&tz));
            let offset = ls.offset().fix();
            let tail = format!("{} ({offset})", tz.name());
            if ls.date_naive() == le.date_naive() {
                if s == e {
                    format!("{} {tail}", ls.format("%Y-%m-%d %H:%M"))
                } else {
                    format!(
                        "{}–{} {tail}",
                        ls.format("%Y-%m-%d %H:%M"),
                        le.format("%H:%M")
                    )
                }
            } else {
                format!(
                    "{}–{} {tail}",
                    ls.format("%Y-%m-%d %H:%M"),
                    le.format("%Y-%m-%d %H:%M")
                )
            }
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(start: Option<RawTime>, end: Option<RawTime>) -> RawEvent {
        RawEvent {
            source: "caldav".into(),
            calendar_id: "work".into(),
            uid: "u1".into(),
            summary: "Standup".into(),
            start,
            end,
            recurrence: None,
        }
    }

    fn t(v: &str, tz: Option<&str>) -> Option<RawTime> {
        Some(RawTime::new(v, tz))
    }

    fn utc(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn same_instant_from_utc_offset_and_iana() {
        let a = CalendarEvent::normalize(raw(t("2026-07-01T08:00:00Z", None), None), None).unwrap();
        let b = CalendarEvent::normalize(raw(t("2026-07-01T10:00:00+02:00", None), None), None)
            .unwrap();
        let c = CalendarEvent::normalize(
            raw(t("2026-07-01T10:00:00", Some("Europe/Berlin")), None),
            None,
        )
        .unwrap();
        assert_eq!(a.start(), b.start());
        assert_eq!(a.start(), c.start());
        assert_eq!(
            c.original_zone(),
            Some(EventZone::Iana(chrono_tz::Europe::Berlin))
        );
        assert_eq!(b.original_zone().unwrap().label(), "+02:00");
        assert_eq!(
            a.display_in("America/New_York").unwrap(),
            "Standup: 2026-07-01 04:00 America/New_York (-04:00)"
        );
    }

    #[test]
    fn floating_needs_default_zone() {
        let r = raw(t("20260701T100000", None), None);
        assert!(matches!(
            CalendarEvent::normalize(r.clone(), None),
            Err(CalendarError::FloatingTimeWithoutZone(_))
        ));
        let e = CalendarEvent::normalize(r, Some("Asia/Tokyo")).unwrap();
        assert_eq!(e.start(), EventTime::Instant(utc("2026-07-01T01:00:00Z")));
        assert_eq!(e.original_zone().unwrap().label(), "floating (Asia/Tokyo)");
    }

    #[test]
    fn all_day_dates_are_exclusive_and_zone_independent() {
        let e = CalendarEvent::normalize(raw(t("2026-12-24", None), t("2026-12-26", None)), None)
            .unwrap();
        assert!(e.is_all_day());
        assert_eq!(e.original_zone(), None);
        for tz in ["Pacific/Auckland", "America/Los_Angeles"] {
            assert_eq!(
                e.display_in(tz).unwrap(),
                "Standup: 2026-12-24–2026-12-25 (all day)"
            );
        }
        let single = CalendarEvent::normalize(raw(t("20261224", None), None), None).unwrap();
        assert_eq!(
            single.end(),
            EventTime::AllDay("2026-12-25".parse().unwrap())
        );
        assert_eq!(
            single.display_in("UTC").unwrap(),
            "Standup: 2026-12-24 (all day)"
        );

        // 24 Dec in Los Angeles starts at 08:00Z, after this window.
        let w = Window::new(utc("2026-12-24T00:00:00Z"), utc("2026-12-24T05:00:00Z")).unwrap();
        assert_eq!(single.occurrences_in(&w, "UTC").unwrap().len(), 1);
        assert!(
            single
                .occurrences_in(&w, "America/Los_Angeles")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn dst_gap_rejected_and_ambiguous_takes_earlier() {
        let gap = raw(t("2026-03-29T02:30:00", Some("Europe/Berlin")), None);
        assert!(matches!(
            CalendarEvent::normalize(gap, None),
            Err(CalendarError::NonexistentLocalTime(_))
        ));
        let amb = CalendarEvent::normalize(
            raw(t("2026-10-25T02:30:00", Some("Europe/Berlin")), None),
            None,
        )
        .unwrap();
        assert_eq!(amb.start(), EventTime::Instant(utc("2026-10-25T00:30:00Z")));
    }

    #[test]
    fn recurrence_keeps_wall_time_across_dst() {
        let mut r = raw(
            t("2026-03-27T09:00:00", Some("Europe/Berlin")),
            t("2026-03-27T09:30:00", Some("Europe/Berlin")),
        );
        r.recurrence = Some(Recurrence {
            frequency: Frequency::Daily,
            interval: 1,
            limit: RecurrenceLimit::Count(4),
        });
        let e = CalendarEvent::normalize(r, None).unwrap();
        let w = Window::new(utc("2026-03-01T00:00:00Z"), utc("2026-04-30T00:00:00Z")).unwrap();
        let occ = e.occurrences_in(&w, "Europe/Berlin").unwrap();
        let shown: Vec<_> = occ
            .iter()
            .map(|o| o.display_in("Europe/Berlin").unwrap())
            .collect();
        assert_eq!(shown.len(), 4);
        assert_eq!(
            shown[0],
            "Standup: 2026-03-27 09:00–09:30 Europe/Berlin (+01:00)"
        );
        assert_eq!(
            shown[3],
            "Standup: 2026-03-30 09:00–09:30 Europe/Berlin (+02:00)"
        );
        assert_eq!(
            occ[3].start,
            EventTime::Instant(utc("2026-03-30T07:00:00Z"))
        );
    }

    #[test]
    fn window_filtering_is_half_open_and_ordered() {
        let mk = |uid: &str, s: &str, e: Option<&str>| {
            let mut r = raw(t(s, None), e.and_then(|e| t(e, None)));
            r.uid = uid.into();
            CalendarEvent::normalize(r, None).unwrap()
        };
        let events = vec![
            mk("late", "2026-05-01T12:00:00Z", Some("2026-05-01T13:00:00Z")),
            mk(
                "early",
                "2026-05-01T09:00:00Z",
                Some("2026-05-01T10:00:00Z"),
            ),
            mk(
                "ends-at-start",
                "2026-05-01T08:00:00Z",
                Some("2026-05-01T09:00:00Z"),
            ),
            mk(
                "starts-at-end",
                "2026-05-01T14:00:00Z",
                Some("2026-05-01T15:00:00Z"),
            ),
            mk("instant-at-start", "2026-05-01T09:00:00Z", None),
            mk(
                "spanning",
                "2026-04-30T00:00:00Z",
                Some("2026-05-02T00:00:00Z"),
            ),
        ];
        let w = Window::new(utc("2026-05-01T09:00:00Z"), utc("2026-05-01T14:00:00Z")).unwrap();
        let uids: Vec<_> = occurrences_in_window(&events, &w, "UTC")
            .unwrap()
            .iter()
            .map(|o| o.event.uid().to_string())
            .collect();
        assert_eq!(uids, ["spanning", "early", "instant-at-start", "late"]);
        assert_eq!(
            Window::new(w.end, w.start),
            Err(CalendarError::InvalidWindow)
        );
    }

    #[test]
    fn until_bounded_weekly_recurrence() {
        let mut r = raw(
            t("2026-05-04T10:00:00Z", None),
            t("2026-05-04T11:00:00Z", None),
        );
        r.recurrence = Some(Recurrence {
            frequency: Frequency::Weekly,
            interval: 2,
            limit: RecurrenceLimit::Until(utc("2026-06-01T10:00:00Z")),
        });
        let e = CalendarEvent::normalize(r, None).unwrap();
        let w = Window::new(utc("2026-01-01T00:00:00Z"), utc("2027-01-01T00:00:00Z")).unwrap();
        // May 4, May 18, Jun 1 (inclusive).
        assert_eq!(e.occurrences_in(&w, "UTC").unwrap().len(), 3);
    }

    #[test]
    fn malformed_and_incomplete_events_are_rejected() {
        let n = |r| CalendarEvent::normalize(r, Some("UTC"));
        assert_eq!(
            n(raw(None, None)),
            Err(CalendarError::MissingField("start"))
        );
        let mut r = raw(t("2026-05-01", None), None);
        r.uid = "  ".into();
        assert_eq!(n(r), Err(CalendarError::MissingField("uid")));
        assert!(matches!(
            n(raw(t("tomorrow", None), None)),
            Err(CalendarError::InvalidTime(_))
        ));
        assert!(matches!(
            n(raw(t("2026-05-01T10:00:00", Some("Mars/Base")), None)),
            Err(CalendarError::UnknownTimezone(_))
        ));
        assert!(matches!(
            n(raw(t("2026-05-01T10:00:00Z", Some("UTC")), None)),
            Err(CalendarError::TimezoneConflict(_))
        ));
        assert!(matches!(
            n(raw(t("2026-05-01", Some("UTC")), None)),
            Err(CalendarError::TimezoneConflict(_))
        ));
        assert_eq!(
            n(raw(t("2026-05-01", None), t("2026-05-01T10:00:00Z", None))),
            Err(CalendarError::MixedAllDayAndTimed)
        );
        assert_eq!(
            n(raw(
                t("2026-05-02T10:00:00Z", None),
                t("2026-05-01T10:00:00Z", None)
            )),
            Err(CalendarError::EndBeforeStart)
        );
        let mut r = raw(t("2026-05-01", None), None);
        r.recurrence = Some(Recurrence {
            frequency: Frequency::Daily,
            interval: 1,
            limit: RecurrenceLimit::Count(MAX_OCCURRENCES + 1),
        });
        assert!(matches!(n(r), Err(CalendarError::InvalidRecurrence(_))));
    }
}
