//! Read-only, bounded collection of context for a short briefing.
//!
//! Briefings are assembled from the caller-provided stores and returned as
//! transient text. This module never writes to those stores or to memory.

use chrono::{DateTime, Duration, Utc};

use crate::{
    inbox::{Filter, InboxStore, Kind, Owner, Provenance, Record, State},
    schedule::JobStore,
};

/// Which sources the collector may include.
#[derive(Debug, Clone)]
pub struct BriefingSources {
    /// Include open tasks and reminders whose due time has arrived.
    pub due_items: bool,
    /// Include enabled scheduled jobs running within `upcoming_window`.
    pub upcoming_jobs: bool,
    /// Include user-authored notes. This is deliberately off by default.
    pub approved_notes: bool,
}

impl Default for BriefingSources {
    fn default() -> Self {
        Self {
            due_items: true,
            upcoming_jobs: true,
            approved_notes: false,
        }
    }
}

/// Limits and source selection for a briefing.
#[derive(Debug, Clone)]
pub struct BriefingOptions {
    pub sources: BriefingSources,
    /// Maximum number of UTF-8 bytes returned.
    pub max_bytes: usize,
    /// How far ahead to include scheduled jobs.
    pub upcoming_window: Duration,
}

impl Default for BriefingOptions {
    fn default() -> Self {
        Self {
            sources: BriefingSources::default(),
            max_bytes: 8 * 1024,
            upcoming_window: Duration::hours(24),
        }
    }
}

/// Assemble a deterministic briefing for `owner` at `now`.
///
/// Pass `None` for a selected store that could not be opened; the output will
/// identify that source as unavailable and still include any usable sources.
/// Unselected stores are not inspected. Only open, due tasks and reminders
/// are included. Notes are restricted to user-authored records and require
/// `sources.approved_notes` to be explicitly enabled.
pub fn collect_briefing(
    owner: &Owner,
    inbox: Option<&InboxStore>,
    jobs: Option<&JobStore>,
    now: DateTime<Utc>,
    options: &BriefingOptions,
) -> String {
    let mut lines = Vec::new();
    let wants_inbox = options.sources.due_items || options.sources.approved_notes;
    if wants_inbox && inbox.is_none() {
        lines.push("[Inbox unavailable] Selected inbox sources could not be read.".to_string());
    }
    if options.sources.upcoming_jobs && jobs.is_none() {
        lines.push(
            "[Scheduled jobs unavailable] Selected schedule source could not be read.".to_string(),
        );
    }

    if wants_inbox {
        if let Some(inbox) = inbox {
            if options.sources.due_items {
                let records = inbox.list(
                    owner,
                    &Filter {
                        state: Some(State::Open),
                        ..Filter::default()
                    },
                );
                let mut due: Vec<_> = records
                    .into_iter()
                    .filter(|record| {
                        matches!(record.kind, Kind::Task | Kind::Reminder)
                            && record.due_at.is_some_and(|due_at| due_at <= now)
                            && record
                                .snoozed_until
                                .is_none_or(|snoozed_until| snoozed_until <= now)
                    })
                    .collect();
                due.sort_by(|left, right| {
                    left.due_at
                        .cmp(&right.due_at)
                        .then_with(|| kind_order(left.kind).cmp(&kind_order(right.kind)))
                        .then_with(|| left.id.cmp(&right.id))
                });
                lines.extend(due.iter().map(|record| format_due_item(record)));
            }

            if options.sources.approved_notes {
                let notes = inbox.list(
                    owner,
                    &Filter {
                        kind: Some(Kind::Note),
                        state: Some(State::Open),
                        ..Filter::default()
                    },
                );
                lines.extend(
                    notes
                        .iter()
                        .filter(|record| record.provenance == Provenance::User)
                        .map(|record| format!("[Approved note] {}", record.text)),
                );
            }
        }
    }

    if options.sources.upcoming_jobs {
        if let Some(jobs) = jobs {
            let end = now
                .checked_add_signed(options.upcoming_window)
                .unwrap_or(now);
            let mut upcoming: Vec<_> = jobs
                .list()
                .iter()
                .filter(|job| job.enabled && job.next_run >= now && job.next_run <= end)
                .collect();
            upcoming.sort_by(|left, right| {
                left.next_run
                    .cmp(&right.next_run)
                    .then_with(|| left.id.cmp(&right.id))
            });
            lines.extend(upcoming.into_iter().map(|job| {
                format!(
                    "[Scheduled job] {} — {} ({}, next {})",
                    job.id,
                    job.prompt,
                    job.when.describe(),
                    job.next_run.format("%Y-%m-%d %H:%M UTC")
                )
            }));
        }
    }

    if lines.is_empty() {
        lines.push("No briefing items.".to_string());
    }

    bounded_join(&lines, options.max_bytes)
}

fn format_due_item(record: &Record) -> String {
    let source = match record.kind {
        Kind::Task => "Due task",
        Kind::Reminder => "Due reminder",
        _ => unreachable!("only tasks and reminders are formatted as due items"),
    };
    let due_at = record
        .due_at
        .expect("due items are filtered to records with a due time");
    format!(
        "[{source}] {} (due {})",
        record.text,
        due_at.format("%Y-%m-%d %H:%M UTC")
    )
}

fn kind_order(kind: Kind) -> u8 {
    match kind {
        Kind::Task => 0,
        Kind::Reminder => 1,
        _ => u8::MAX,
    }
}

fn bounded_join(lines: &[String], max_bytes: usize) -> String {
    let mut output = String::new();
    for line in lines {
        let separator = if output.is_empty() { "" } else { "\n" };
        let remaining = max_bytes.saturating_sub(output.len());
        let addition = format!("{separator}{line}");
        let end = utf8_boundary_at_or_before(&addition, remaining);
        output.push_str(&addition[..end]);
        if end < addition.len() {
            break;
        }
    }
    output
}

fn utf8_boundary_at_or_before(text: &str, max_bytes: usize) -> usize {
    let mut end = max_bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        inbox::{NewRecord, Priority},
        schedule::{Job, Target, When},
    };

    fn owner() -> Owner {
        Owner::new("briefing-test").unwrap()
    }

    fn instant(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn create(
        store: &mut InboxStore,
        owner: &Owner,
        kind: Kind,
        text: &str,
        due_at: Option<DateTime<Utc>>,
        provenance: Provenance,
    ) {
        let mut new = NewRecord::new(kind, text, provenance);
        new.due_at = due_at;
        new.priority = Priority::Normal;
        store.create(owner, new).unwrap();
    }

    #[test]
    fn selection_filters_sources_and_notes_require_explicit_opt_in() {
        let owner = owner();
        let now = instant("2026-01-01T12:00:00Z");
        let mut inbox = InboxStore::in_memory();
        create(
            &mut inbox,
            &owner,
            Kind::Task,
            "due work",
            Some(now),
            Provenance::User,
        );
        create(
            &mut inbox,
            &owner,
            Kind::Task,
            "future work",
            Some(now + Duration::hours(1)),
            Provenance::User,
        );
        create(
            &mut inbox,
            &owner,
            Kind::Note,
            "private note",
            None,
            Provenance::User,
        );
        create(
            &mut inbox,
            &owner,
            Kind::Note,
            "unapproved suggestion",
            None,
            Provenance::Assistant,
        );

        let mut jobs = JobStore::in_memory();
        let mut job = Job::new(
            "job-1",
            "check status",
            When::Once {
                at: now + Duration::hours(1),
            },
            Target::Session,
            now,
        );
        job.next_run = now + Duration::hours(1);
        jobs.add(job);

        let briefing = collect_briefing(
            &owner,
            Some(&inbox),
            Some(&jobs),
            now,
            &BriefingOptions::default(),
        );
        assert!(briefing.contains("due work"));
        assert!(!briefing.contains("future work"));
        assert!(!briefing.contains("private note"));
        assert!(!briefing.contains("unapproved suggestion"));
        assert!(briefing.contains("check status"));

        let options = BriefingOptions {
            sources: BriefingSources {
                due_items: false,
                upcoming_jobs: true,
                approved_notes: true,
            },
            ..BriefingOptions::default()
        };
        let briefing = collect_briefing(&owner, Some(&inbox), Some(&jobs), now, &options);
        assert!(!briefing.contains("due work"));
        assert!(briefing.contains("private note"));
        assert!(!briefing.contains("unapproved suggestion"));
        assert!(briefing.contains("check status"));
    }

    #[test]
    fn output_attributes_each_source_and_respects_utf8_byte_limit() {
        let owner = owner();
        let now = instant("2026-01-01T12:00:00Z");
        let mut inbox = InboxStore::in_memory();
        create(
            &mut inbox,
            &owner,
            Kind::Reminder,
            "café reminder",
            Some(now),
            Provenance::User,
        );
        let mut jobs = JobStore::in_memory();
        let mut job = Job::new(
            "job-2",
            "refresh cache",
            When::Once {
                at: now + Duration::minutes(20),
            },
            Target::Session,
            now,
        );
        job.next_run = now + Duration::minutes(20);
        jobs.add(job);

        let briefing = collect_briefing(
            &owner,
            Some(&inbox),
            Some(&jobs),
            now,
            &BriefingOptions::default(),
        );
        assert!(briefing.contains("[Due reminder]"));
        assert!(briefing.contains("[Scheduled job]"));

        let limited = collect_briefing(
            &owner,
            Some(&inbox),
            Some(&jobs),
            now,
            &BriefingOptions {
                max_bytes: 17,
                ..BriefingOptions::default()
            },
        );
        assert!(limited.len() <= 17);
        assert!(limited.is_char_boundary(limited.len()));
    }

    #[test]
    fn empty_and_unavailable_sources_are_deliberate() {
        let owner = owner();
        let now = instant("2026-01-01T12:00:00Z");
        let inbox = InboxStore::in_memory();
        let jobs = JobStore::in_memory();
        assert_eq!(
            collect_briefing(
                &owner,
                Some(&inbox),
                Some(&jobs),
                now,
                &BriefingOptions::default()
            ),
            "No briefing items."
        );

        let partial = collect_briefing(&owner, None, Some(&jobs), now, &BriefingOptions::default());
        assert!(partial.contains("[Inbox unavailable]"));
        assert!(partial.len() <= BriefingOptions::default().max_bytes);
    }

    #[test]
    fn collection_does_not_save_briefing_text_or_mutate_inbox() {
        let owner = owner();
        let now = instant("2026-01-01T12:00:00Z");
        let dir = tempfile::tempdir().unwrap();
        let mut inbox = InboxStore::open(dir.path()).unwrap();
        create(
            &mut inbox,
            &owner,
            Kind::Task,
            "persisted task",
            Some(now),
            Provenance::User,
        );
        let before = std::fs::read(dir.path().join("inbox.json")).unwrap();
        let before_count = inbox.list(&owner, &Filter::default()).len();

        let briefing = collect_briefing(
            &owner,
            Some(&inbox),
            None,
            now,
            &BriefingOptions {
                sources: BriefingSources {
                    due_items: true,
                    upcoming_jobs: false,
                    approved_notes: false,
                },
                ..BriefingOptions::default()
            },
        );

        assert!(briefing.contains("persisted task"));
        assert_eq!(inbox.list(&owner, &Filter::default()).len(), before_count);
        assert_eq!(
            std::fs::read(dir.path().join("inbox.json")).unwrap(),
            before
        );
        assert!(
            !std::fs::read_to_string(dir.path().join("inbox.json"))
                .unwrap()
                .contains(&briefing)
        );
    }
}
