//! Read-only agenda and calendar planning helpers.
//!
//! Agenda entries combine calendar events with open inbox tasks and reminders
//! due on the selected local day. Inbox due times are points in time, not
//! inferred work intervals. Since inbox records have no interval model,
//! overlap analysis is deliberately limited to calendar event occurrences.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

use crate::{
    calendar::{
        CalendarError, CalendarEvent, EventTime, Occurrence, Window, occurrences_in_window,
    },
    inbox::{Kind, Record, State},
};

/// Errors while selecting a local-day agenda or analyzing calendar events.
#[derive(Debug, thiserror::Error)]
pub enum AgendaError {
    #[error(transparent)]
    Calendar(#[from] CalendarError),
    #[error("local midnight does not exist in timezone {timezone} on {date}")]
    InvalidDayBoundary { date: NaiveDate, timezone: String },
}

/// The kind of item shown in an agenda.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgendaItemKind {
    Event,
    TaskDue,
    ReminderDue,
}

/// One display-ready agenda item. For task and reminder due items, `start`
/// equals `end`: due times do not imply a work duration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgendaItem {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
    pub kind: AgendaItemKind,
    pub source_label: String,
    pub description: String,
    stable_key: String,
}

/// Today's agenda in one explicitly selected IANA timezone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agenda {
    pub date: NaiveDate,
    pub timezone: String,
    pub items: Vec<AgendaItem>,
}

impl Agenda {
    /// Render a deterministic agenda with the selected timezone visible even
    /// when every item is all-day or the agenda is empty.
    pub fn format(&self) -> String {
        let mut output = format!("Agenda for {} ({})", self.date, self.timezone);
        if self.items.is_empty() {
            output.push_str("\n  No items.");
            return output;
        }
        for item in &self.items {
            output.push_str(&format!("\n  [{}] {}", item.source_label, item.description));
        }
        output
    }
}

/// Return calendar events and open inbox tasks/reminders due on `now`'s local
/// day in `timezone`. Every rendered item includes a source label.
///
/// The returned records are only read. A task or reminder with a due time is
/// represented as a point, never as a duration or busy interval.
pub fn today_agenda(
    events: &[CalendarEvent],
    inbox_records: &[Record],
    now: DateTime<Utc>,
    timezone: &str,
) -> Result<Agenda, AgendaError> {
    let tz = parse_timezone(timezone)?;
    let date = now.with_timezone(&tz).date_naive();
    let window = local_day_window(date, timezone, tz)?;
    let mut items = Vec::new();

    for occurrence in occurrences_in_window(events, &window, timezone)? {
        let (start, end) = occurrence_instants(&occurrence, tz, timezone)?;
        items.push(AgendaItem {
            start,
            end,
            all_day: occurrence.event.is_all_day(),
            kind: AgendaItemKind::Event,
            source_label: calendar_source_label(occurrence.event),
            description: occurrence.display_in(timezone)?,
            stable_key: format!(
                "event:{}:{}:{}",
                occurrence.event.uid(),
                occurrence.index,
                occurrence.event.calendar_id()
            ),
        });
    }

    for record in inbox_records.iter().filter(|record| {
        record.state == State::Open
            && matches!(record.kind, Kind::Task | Kind::Reminder)
            && record
                .due_at
                .is_some_and(|due_at| due_at >= window.start && due_at < window.end)
    }) {
        let due_at = record.due_at.expect("filtered to records with a due time");
        let (kind, kind_label) = match record.kind {
            Kind::Task => (AgendaItemKind::TaskDue, "task"),
            Kind::Reminder => (AgendaItemKind::ReminderDue, "reminder"),
            _ => unreachable!("filtered to tasks and reminders"),
        };
        let source_label = record.source.channel.as_deref().map_or_else(
            || format!("Inbox {kind_label}"),
            |channel| format!("Inbox {kind_label} via {channel}"),
        );
        items.push(AgendaItem {
            start: due_at,
            end: due_at,
            all_day: false,
            kind,
            source_label,
            description: format!(
                "{} — due {} {}",
                record.text,
                due_at.with_timezone(&tz).format("%Y-%m-%d %H:%M"),
                tz.name()
            ),
            stable_key: format!("inbox:{}", record.id),
        });
    }

    items.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| right.all_day.cmp(&left.all_day))
            .then_with(|| left.source_label.cmp(&right.source_label))
            .then_with(|| left.description.cmp(&right.description))
            .then_with(|| left.stable_key.cmp(&right.stable_key))
    });

    Ok(Agenda {
        date,
        timezone: tz.name().to_owned(),
        items,
    })
}

/// An overlap between two calendar event occurrences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventOverlap {
    pub first: String,
    pub second: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// Find overlaps among calendar event occurrences inside `window`.
///
/// Intervals use half-open semantics: events that meet exactly at an end/start
/// boundary do not overlap. Zero-length events do not occupy busy time.
/// This helper accepts only calendar events; inbox tasks/reminders are not
/// analyzed because they currently have no explicit interval fields.
pub fn event_overlaps(
    events: &[CalendarEvent],
    window: &Window,
    timezone: &str,
) -> Result<Vec<EventOverlap>, AgendaError> {
    let tz = parse_timezone(timezone)?;
    let mut occurrences = Vec::new();
    for occurrence in occurrences_in_window(events, window, timezone)? {
        let (start, end) = occurrence_instants(&occurrence, tz, timezone)?;
        occurrences.push((
            start,
            end,
            calendar_occurrence_label(&occurrence),
            occurrence.event.uid().to_owned(),
            occurrence.index,
        ));
    }
    occurrences.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.4.cmp(&right.4))
    });

    let mut overlaps = Vec::new();
    for i in 0..occurrences.len() {
        let (left_start, left_end, left_label, _, _) = &occurrences[i];
        if left_start == left_end {
            continue;
        }
        for (right_start, right_end, right_label, _, _) in occurrences.iter().skip(i + 1) {
            if right_start >= left_end {
                break;
            }
            if right_start < right_end && left_start < right_end && right_start < left_end {
                overlaps.push(EventOverlap {
                    first: left_label.clone(),
                    second: right_label.clone(),
                    start: (*left_start).max(*right_start),
                    end: (*left_end).min(*right_end),
                });
            }
        }
    }
    overlaps.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.end.cmp(&right.end))
            .then_with(|| left.first.cmp(&right.first))
            .then_with(|| left.second.cmp(&right.second))
    });
    Ok(overlaps)
}

/// A non-binding planning note derived from calendar event overlaps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisorySuggestion {
    pub message: String,
}

/// Return advisory-only notes for event conflicts; this function never
/// changes calendar, inbox, or memory state.
pub fn advisory_suggestions(
    events: &[CalendarEvent],
    window: &Window,
    timezone: &str,
) -> Result<Vec<AdvisorySuggestion>, AgendaError> {
    let tz = parse_timezone(timezone)?;
    Ok(event_overlaps(events, window, timezone)?
        .into_iter()
        .map(|overlap| AdvisorySuggestion {
            message: format!(
                "Advisory only: {} overlaps {} from {} to {} {}. Review the calendar if useful.",
                overlap.first,
                overlap.second,
                overlap.start.with_timezone(&tz).format("%Y-%m-%d %H:%M"),
                overlap.end.with_timezone(&tz).format("%Y-%m-%d %H:%M"),
                tz.name()
            ),
        })
        .collect())
}

fn calendar_source_label(event: &CalendarEvent) -> String {
    format!("Calendar {}/{}", event.source(), event.calendar_id())
}

fn calendar_occurrence_label(occurrence: &Occurrence<'_>) -> String {
    format!(
        "{} — {}",
        calendar_source_label(occurrence.event),
        occurrence.event.summary()
    )
}

fn parse_timezone(timezone: &str) -> Result<Tz, CalendarError> {
    timezone
        .parse::<Tz>()
        .map_err(|_| CalendarError::UnknownTimezone(timezone.to_owned()))
}

fn local_day_window(date: NaiveDate, timezone: &str, tz: Tz) -> Result<Window, AgendaError> {
    let next_day = date
        .succ_opt()
        .ok_or_else(|| AgendaError::InvalidDayBoundary {
            date,
            timezone: timezone.to_owned(),
        })?;
    let start = local_midnight(date, timezone, tz)?;
    let end = local_midnight(next_day, timezone, tz)?;
    Ok(Window::new(start, end)?)
}

fn local_midnight(date: NaiveDate, timezone: &str, tz: Tz) -> Result<DateTime<Utc>, AgendaError> {
    let midnight: NaiveDateTime =
        date.and_hms_opt(0, 0, 0)
            .ok_or_else(|| AgendaError::InvalidDayBoundary {
                date,
                timezone: timezone.to_owned(),
            })?;
    tz.from_local_datetime(&midnight)
        .single()
        .map(|value| value.with_timezone(&Utc))
        .ok_or_else(|| AgendaError::InvalidDayBoundary {
            date,
            timezone: timezone.to_owned(),
        })
}

fn occurrence_instants(
    occurrence: &Occurrence<'_>,
    tz: Tz,
    timezone: &str,
) -> Result<(DateTime<Utc>, DateTime<Utc>), AgendaError> {
    let instant = |time| match time {
        EventTime::Instant(at) => Ok(at),
        EventTime::AllDay(date) => local_midnight(date, timezone, tz),
    };
    Ok((instant(occurrence.start)?, instant(occurrence.end)?))
}

/// Today's agenda for the local owner as text: due inbox items plus, when
/// `LIA_CALDAV_URL`, `LIA_CALDAV_USER` and `LIA_CALDAV_PASSWORD` are set, the
/// events of that read-only calendar.
pub async fn agenda_text(
    state_dir: &std::path::Path,
    timezone: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<String> {
    use crate::calendar::Window;

    let tz: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| anyhow::anyhow!("unknown timezone: {timezone}"))?;
    let day = now.with_timezone(&tz).date_naive();
    let local = |d: chrono::NaiveDate| {
        d.and_hms_opt(0, 0, 0)
            .and_then(|t| t.and_local_timezone(tz).earliest())
            .map(|t| t.with_timezone(&chrono::Utc))
    };
    let mut events = Vec::new();
    if let (Some(url), Some(user), Some(pass)) = (
        crate::secrets::var("LIA_CALDAV_URL"),
        crate::secrets::var("LIA_CALDAV_USER"),
        crate::secrets::var("LIA_CALDAV_PASSWORD"),
    ) {
        let calendar =
            crate::secrets::var("LIA_CALDAV_CALENDAR").unwrap_or_else(|| "calendar".into());
        let client = crate::caldav::CalDavClient::new(crate::caldav::CalDavConfig::new(
            &url, &user, &pass, &calendar, timezone,
        ))?;
        let (start, end) = match (local(day), day.succ_opt().and_then(local)) {
            (Some(start), Some(end)) => (start, end),
            _ => anyhow::bail!("cannot compute the day boundaries in {timezone}"),
        };
        events = client.events_in(Window::new(start, end)?).await?;
    }
    let owner = crate::inbox::Owner::new(crate::inbox_identity::LOCAL_OWNER_ID)?;
    let store = crate::inbox::InboxStore::open(state_dir.join("inbox"))?;
    let records = store.list(&owner, &crate::inbox::Filter::default());
    let agenda = crate::agenda::today_agenda(&events, &records, now, timezone)?;
    Ok(agenda.format())
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;
    use crate::{
        calendar::{RawEvent, RawTime},
        inbox::{Owner, Priority, Provenance, Source},
    };

    fn event(uid: &str, summary: &str, start: &str, end: &str) -> CalendarEvent {
        CalendarEvent::normalize(
            RawEvent {
                source: "work".into(),
                calendar_id: "primary".into(),
                uid: uid.into(),
                summary: summary.into(),
                start: Some(RawTime::new(start, None)),
                end: Some(RawTime::new(end, None)),
                recurrence: None,
            },
            None,
        )
        .unwrap()
    }

    fn task(text: &str, due_at: DateTime<Utc>) -> Record {
        let mut inbox = crate::inbox::InboxStore::in_memory();
        let owner = Owner::new("agenda-test").unwrap();
        let mut new = crate::inbox::NewRecord::new(Kind::Task, text, Provenance::User);
        new.priority = Priority::Normal;
        new.due_at = Some(due_at);
        new.source = Source {
            channel: Some("test".into()),
            ..Source::default()
        };
        inbox.create(&owner, new).unwrap()
    }

    #[test]
    fn agenda_shows_sources_and_explicit_timezone_for_all_day_and_timed_items() {
        let events = [
            event("holiday", "Holiday", "2026-11-03", "2026-11-04"),
            event(
                "meeting",
                "Meeting",
                "2026-11-03T08:30:00Z",
                "2026-11-03T09:00:00Z",
            ),
        ];
        let due = task(
            "Prepare",
            Utc.with_ymd_and_hms(2026, 11, 3, 10, 0, 0).unwrap(),
        );
        let agenda = today_agenda(
            &events,
            &[due],
            Utc.with_ymd_and_hms(2026, 11, 3, 0, 0, 0).unwrap(),
            "Europe/Berlin",
        )
        .unwrap();
        let rendered = agenda.format();

        assert!(rendered.contains("Agenda for 2026-11-03 (Europe/Berlin)"));
        assert!(rendered.contains("[Calendar work/primary] Holiday: 2026-11-03 (all day)"));
        assert!(rendered.contains("Meeting: 2026-11-03 09:30–10:00 Europe/Berlin"));
        assert!(
            rendered.contains("[Inbox task via test] Prepare — due 2026-11-03 11:00 Europe/Berlin")
        );
        assert!(agenda.items.iter().any(|item| item.all_day));
        let due_item = agenda
            .items
            .iter()
            .find(|item| item.kind == AgendaItemKind::TaskDue)
            .unwrap();
        assert_eq!(due_item.start, due_item.end);
    }

    #[test]
    fn agenda_order_is_deterministic_for_equal_start_times() {
        let events = [
            event("z", "Zulu", "2026-11-03T09:00:00Z", "2026-11-03T09:30:00Z"),
            event("a", "Alpha", "2026-11-03T09:00:00Z", "2026-11-03T09:30:00Z"),
        ];
        let now = Utc.with_ymd_and_hms(2026, 11, 3, 0, 0, 0).unwrap();
        let first = today_agenda(&events, &[], now, "UTC").unwrap();
        let second = today_agenda(&events, &[], now, "UTC").unwrap();

        assert_eq!(first, second);
        assert!(first.items[0].description.contains("Alpha"));
        assert!(first.items[1].description.contains("Zulu"));
    }

    #[test]
    fn event_overlaps_obey_half_open_boundaries_and_ignore_due_only_tasks() {
        let events = [
            event(
                "first",
                "First",
                "2026-11-03T09:00:00Z",
                "2026-11-03T10:00:00Z",
            ),
            event(
                "boundary",
                "Boundary",
                "2026-11-03T10:00:00Z",
                "2026-11-03T11:00:00Z",
            ),
            event(
                "overlap",
                "Overlap",
                "2026-11-03T09:30:00Z",
                "2026-11-03T10:30:00Z",
            ),
        ];
        let window = Window::new(
            Utc.with_ymd_and_hms(2026, 11, 3, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 11, 4, 0, 0, 0).unwrap(),
        )
        .unwrap();
        let conflicts = event_overlaps(&events, &window, "UTC").unwrap();

        assert_eq!(conflicts.len(), 2);
        assert!(
            conflicts
                .iter()
                .any(|item| { item.first.contains("First") && item.second.contains("Overlap") })
        );
        assert!(
            conflicts
                .iter()
                .any(|item| { item.first.contains("Overlap") && item.second.contains("Boundary") })
        );
        let _due_only = task(
            "due point",
            Utc.with_ymd_and_hms(2026, 11, 3, 9, 45, 0).unwrap(),
        );
        assert_eq!(event_overlaps(&events, &window, "UTC").unwrap(), conflicts);
    }

    #[test]
    fn advisory_suggestions_are_non_binding_and_leave_inputs_unchanged() {
        let events = [
            event("one", "One", "2026-11-03T09:00:00Z", "2026-11-03T10:00:00Z"),
            event("two", "Two", "2026-11-03T09:30:00Z", "2026-11-03T10:30:00Z"),
        ];
        let before = events.clone();
        let window = Window::new(
            Utc.with_ymd_and_hms(2026, 11, 3, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 11, 4, 0, 0, 0).unwrap(),
        )
        .unwrap();

        let suggestions = advisory_suggestions(&events, &window, "UTC").unwrap();

        assert_eq!(events, before);
        assert_eq!(suggestions.len(), 1);
        assert!(suggestions[0].message.starts_with("Advisory only:"));
        assert!(
            suggestions[0]
                .message
                .contains("Review the calendar if useful.")
        );
        assert!(suggestions[0].message.contains("UTC"));
        assert_eq!(window.end - window.start, Duration::days(1));
    }
}
