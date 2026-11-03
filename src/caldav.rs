//! A read-only CalDAV client.
//!
//! Nothing here runs unless a caller opts in by building a [`CalDavClient`]
//! from an explicit [`CalDavConfig`]; there is no default configuration and
//! constructing a client performs no network I/O. The client only issues
//! `REPORT` (calendar-query) and `PROPFIND` (collection discovery) requests,
//! and has no write methods. It never touches memory.
//!
//! Safety properties:
//!
//! - the endpoint must be `https`, except for loopback hosts (tests);
//! - TLS certificates are validated and redirects are never followed;
//! - requests time out, responses are size-bounded, event counts are bounded;
//! - credentials and event bodies never appear in errors;
//! - event summaries are untrusted text: control characters are stripped and
//!   the length is capped;
//! - events with recurrence this model cannot express faithfully are
//!   rejected rather than shown incompletely.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;
use quick_xml::Reader;
use quick_xml::events::Event;
use reqwest::{Method, StatusCode, Url, redirect};

use crate::calendar::{
    CalendarError, CalendarEvent, Frequency, RawEvent, RawTime, Recurrence, RecurrenceLimit, Window,
};

const SOURCE: &str = "caldav";
const MAX_SUMMARY_CHARS: usize = 200;
const MAX_WINDOW_DAYS: i64 = 366;
const MAX_DISCOVERED: usize = 100;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const DEFAULT_MAX_EVENTS: usize = 500;

/// Why a CalDAV operation failed. Messages carry no credentials or event text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalDavError {
    InvalidConfig(&'static str),
    /// Plain `http` to a non-loopback host.
    InsecureEndpoint,
    InvalidWindow,
    Timeout,
    Transport,
    Unauthorized(u16),
    Redirect(u16),
    Server(u16),
    UnexpectedStatus(u16),
    ResponseTooLarge,
    TooManyEvents,
    MalformedXml,
    ICalendar(&'static str),
    UnsupportedRecurrence(&'static str),
    Calendar(CalendarError),
}

impl fmt::Display for CalDavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(why) => write!(f, "invalid caldav config: {why}"),
            Self::InsecureEndpoint => write!(f, "caldav endpoint must use https"),
            Self::InvalidWindow => write!(
                f,
                "query window must end after it starts and span at most {MAX_WINDOW_DAYS} days"
            ),
            Self::Timeout => write!(f, "caldav request timed out"),
            Self::Transport => write!(f, "caldav request failed"),
            Self::Unauthorized(c) => write!(f, "caldav authentication rejected (HTTP {c})"),
            Self::Redirect(c) => write!(f, "caldav server redirected (HTTP {c}); not followed"),
            Self::Server(c) => write!(f, "caldav server error (HTTP {c})"),
            Self::UnexpectedStatus(c) => write!(f, "unexpected caldav response (HTTP {c})"),
            Self::ResponseTooLarge => write!(f, "caldav response exceeds the size limit"),
            Self::TooManyEvents => write!(f, "caldav response exceeds the event limit"),
            Self::MalformedXml => write!(f, "malformed caldav multistatus response"),
            Self::ICalendar(why) => write!(f, "malformed iCalendar data: {why}"),
            Self::UnsupportedRecurrence(why) => write!(f, "unsupported recurrence: {why}"),
            Self::Calendar(_) => write!(f, "event failed calendar validation"),
        }
    }
}

impl std::error::Error for CalDavError {}

impl From<CalendarError> for CalDavError {
    fn from(e: CalendarError) -> Self {
        Self::Calendar(e)
    }
}

/// Explicit settings for a client. There is deliberately no `Default`.
#[derive(Clone)]
pub struct CalDavConfig {
    /// Calendar collection URL (or a home URL, for [`CalDavClient::discover_collections`]).
    pub endpoint: String,
    pub username: String,
    pub password: String,
    pub calendar_id: String,
    /// IANA zone applied to zoneless (floating) event times.
    pub default_timezone: String,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub max_events: usize,
}

impl CalDavConfig {
    pub fn new(
        endpoint: &str,
        username: &str,
        password: &str,
        calendar_id: &str,
        default_timezone: &str,
    ) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            username: username.to_string(),
            password: password.to_string(),
            calendar_id: calendar_id.to_string(),
            default_timezone: default_timezone.to_string(),
            timeout: DEFAULT_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_events: DEFAULT_MAX_EVENTS,
        }
    }
}

impl fmt::Debug for CalDavConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CalDavConfig")
            .field("calendar_id", &self.calendar_id)
            .field("default_timezone", &self.default_timezone)
            .field("credentials", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Read-only CalDAV client for one calendar collection.
pub struct CalDavClient {
    http: reqwest::Client,
    url: Url,
    username: String,
    password: String,
    calendar_id: String,
    default_timezone: String,
    max_response_bytes: usize,
    max_events: usize,
}

impl fmt::Debug for CalDavClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CalDavClient")
            .field("calendar_id", &self.calendar_id)
            .finish_non_exhaustive()
    }
}

fn is_loopback(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn transport_error(e: reqwest::Error) -> CalDavError {
    if e.is_timeout() {
        CalDavError::Timeout
    } else {
        CalDavError::Transport
    }
}

impl CalDavClient {
    /// Validate the configuration and build a client. Performs no I/O.
    pub fn new(config: CalDavConfig) -> Result<Self, CalDavError> {
        if config.endpoint.trim().is_empty() {
            return Err(CalDavError::InvalidConfig("endpoint is required"));
        }
        if config.username.is_empty() {
            return Err(CalDavError::InvalidConfig("username is required"));
        }
        if config.password.is_empty() {
            return Err(CalDavError::InvalidConfig("credential is required"));
        }
        if config.calendar_id.trim().is_empty() {
            return Err(CalDavError::InvalidConfig("calendar id is required"));
        }
        if config.default_timezone.trim().parse::<Tz>().is_err() {
            return Err(CalDavError::InvalidConfig(
                "default timezone must be an IANA zone",
            ));
        }
        if config.max_response_bytes == 0 || config.max_events == 0 || config.timeout.is_zero() {
            return Err(CalDavError::InvalidConfig("limits must be non-zero"));
        }
        let url = Url::parse(config.endpoint.trim())
            .map_err(|_| CalDavError::InvalidConfig("endpoint is not a valid URL"))?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(CalDavError::InvalidConfig(
                "endpoint must not embed credentials",
            ));
        }
        match url.scheme() {
            "https" => {}
            "http" if is_loopback(&url) => {}
            "http" => return Err(CalDavError::InsecureEndpoint),
            _ => return Err(CalDavError::InvalidConfig("endpoint must be http(s)")),
        }
        let http = reqwest::Client::builder()
            .redirect(redirect::Policy::none())
            .timeout(config.timeout)
            .build()
            .map_err(|_| CalDavError::Transport)?;
        Ok(Self {
            http,
            url,
            username: config.username,
            password: config.password,
            calendar_id: config.calendar_id.trim().to_string(),
            default_timezone: config.default_timezone.trim().to_string(),
            max_response_bytes: config.max_response_bytes,
            max_events: config.max_events,
        })
    }

    /// List calendar collections directly under the configured endpoint
    /// (`PROPFIND`, depth 1). Only same-origin collections are returned.
    pub async fn discover_collections(&self) -> Result<Vec<String>, CalDavError> {
        const BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/></d:prop></d:propfind>"#;
        let xml = self.send("PROPFIND", BODY.to_string()).await?;
        let mut found = Vec::new();
        for r in parse_multistatus(&xml, usize::MAX)?
            .into_iter()
            .filter(|r| r.is_calendar)
        {
            let Some(href) = r.href else { continue };
            let Ok(url) = self.url.join(href.trim()) else {
                continue;
            };
            if url.origin() != self.url.origin() {
                continue;
            }
            if found.len() == MAX_DISCOVERED {
                return Err(CalDavError::TooManyEvents);
            }
            found.push(url.to_string());
        }
        Ok(found)
    }

    /// Fetch events overlapping `window` with a `REPORT` calendar-query.
    pub async fn events_in(&self, window: Window) -> Result<Vec<CalendarEvent>, CalDavError> {
        if window.end <= window.start || (window.end - window.start).num_days() > MAX_WINDOW_DAYS {
            return Err(CalDavError::InvalidWindow);
        }
        let stamp = |t: DateTime<Utc>| t.format("%Y%m%dT%H%M%SZ").to_string();
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><c:calendar-data/></d:prop><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"><c:time-range start="{}" end="{}"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#,
            stamp(window.start),
            stamp(window.end)
        );
        let xml = self.send("REPORT", body).await?;
        let responses = parse_multistatus(&xml, self.max_events)?;
        let mut events = Vec::new();
        for data in responses.iter().flat_map(|r| r.calendar_data.iter()) {
            for raw in parse_icalendar(data, &self.calendar_id)? {
                if events.len() == self.max_events {
                    return Err(CalDavError::TooManyEvents);
                }
                events.push(CalendarEvent::normalize(
                    raw,
                    Some(self.default_timezone.as_str()),
                )?);
            }
        }
        Ok(events)
    }

    async fn send(&self, method: &str, body: String) -> Result<String, CalDavError> {
        let method = Method::from_bytes(method.as_bytes()).map_err(|_| CalDavError::Transport)?;
        let mut response = self
            .http
            .request(method, self.url.clone())
            .basic_auth(&self.username, Some(&self.password))
            .header("Depth", "1")
            .header("Content-Type", "application/xml; charset=utf-8")
            .header("Accept", "application/xml, text/xml")
            .body(body)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        if status != StatusCode::MULTI_STATUS {
            let code = status.as_u16();
            return Err(match code {
                401 | 403 => CalDavError::Unauthorized(code),
                300..=399 => CalDavError::Redirect(code),
                500..=599 => CalDavError::Server(code),
                _ => CalDavError::UnexpectedStatus(code),
            });
        }
        let limit = self.max_response_bytes;
        if response.content_length().is_some_and(|n| n > limit as u64) {
            return Err(CalDavError::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if bytes.len() + chunk.len() > limit {
                return Err(CalDavError::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        String::from_utf8(bytes).map_err(|_| CalDavError::MalformedXml)
    }
}

#[derive(Default)]
struct DavResponse {
    href: Option<String>,
    is_calendar: bool,
    calendar_data: Vec<String>,
}

fn local(name: &[u8]) -> &[u8] {
    name.rsplit(|b| *b == b':').next().unwrap_or(name)
}

/// Parse a WebDAV multistatus body. Only properties reported with a 2xx
/// status are used. `max_data` bounds the number of calendar-data blobs.
fn parse_multistatus(xml: &str, max_data: usize) -> Result<Vec<DavResponse>, CalDavError> {
    let mut reader = Reader::from_str(xml);
    let mut responses = Vec::new();
    let mut current = DavResponse::default();
    let mut path: Vec<Vec<u8>> = Vec::new();
    let mut text = String::new();
    let mut prop_data: Vec<String> = Vec::new();
    let mut prop_calendar = false;
    let mut prop_ok = true;
    let mut total = 0usize;
    let mut saw_root = false;
    let bad = || CalDavError::MalformedXml;

    loop {
        match reader.read_event().map_err(|_| bad())? {
            Event::DocType(_) => return Err(bad()),
            Event::Start(e) => {
                let name = local(e.name().as_ref()).to_vec();
                if path.is_empty() {
                    if name != b"multistatus" || saw_root {
                        return Err(bad());
                    }
                    saw_root = true;
                }
                match name.as_slice() {
                    b"response" => current = DavResponse::default(),
                    b"propstat" => {
                        prop_data.clear();
                        prop_calendar = false;
                        prop_ok = true;
                    }
                    b"calendar" if path.last().is_some_and(|p| p == b"resourcetype") => {
                        prop_calendar = true;
                    }
                    _ => {}
                }
                text.clear();
                path.push(name);
            }
            Event::Empty(e) => {
                let name = local(e.name().as_ref()).to_vec();
                if path.is_empty() {
                    return Err(bad());
                }
                if name == b"calendar" && path.last().is_some_and(|p| p == b"resourcetype") {
                    prop_calendar = true;
                }
            }
            Event::End(_) => {
                let name = path.pop().ok_or_else(bad)?;
                let parent = path.last().map(Vec::as_slice).unwrap_or_default();
                match (name.as_slice(), parent) {
                    (b"href", b"response") => current.href = Some(text.trim().to_string()),
                    (b"status", b"propstat") => {
                        prop_ok = text
                            .split_whitespace()
                            .nth(1)
                            .is_some_and(|c| c.starts_with('2'));
                    }
                    (b"calendar-data", b"prop") => prop_data.push(std::mem::take(&mut text)),
                    (b"propstat", _) if prop_ok => {
                        total += prop_data.len();
                        if total > max_data {
                            return Err(CalDavError::TooManyEvents);
                        }
                        current.is_calendar |= prop_calendar;
                        current.calendar_data.append(&mut prop_data);
                    }
                    (b"response", _) => responses.push(std::mem::take(&mut current)),
                    _ => {}
                }
                text.clear();
            }
            Event::Text(t) => text.push_str(&t.decode().map_err(|_| bad())?),
            Event::CData(t) => text.push_str(&t.decode().map_err(|_| bad())?),
            Event::GeneralRef(r) => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    text.push(c);
                } else {
                    match &*r.decode().map_err(|_| bad())? {
                        "amp" => text.push('&'),
                        "lt" => text.push('<'),
                        "gt" => text.push('>'),
                        "quot" => text.push('"'),
                        "apos" => text.push('\''),
                        _ => return Err(bad()),
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !saw_root || !path.is_empty() {
        return Err(bad());
    }
    Ok(responses)
}

/// Undo RFC 5545 line folding.
fn unfold(data: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in data.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix([' ', '\t']) {
            if let Some(last) = lines.last_mut() {
                last.push_str(rest);
            }
        } else {
            lines.push(line.to_string());
        }
    }
    lines
}

struct ContentLine {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

/// Split at the first delimiter outside double quotes.
fn split_unquoted(s: &str, delim: char) -> (&str, Option<&str>) {
    let mut quoted = false;
    for (i, c) in s.char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if c == delim && !quoted {
            return (&s[..i], Some(&s[i + c.len_utf8()..]));
        }
    }
    (s, None)
}

fn parse_line(line: &str) -> Option<ContentLine> {
    let (head, value) = split_unquoted(line, ':');
    let value = value?;
    let mut parts = Vec::new();
    let mut rest = head;
    loop {
        let (part, tail) = split_unquoted(rest, ';');
        parts.push(part);
        match tail {
            Some(t) => rest = t,
            None => break,
        }
    }
    let name = parts[0].trim().to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = parts[1..]
        .iter()
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((
                k.trim().to_ascii_uppercase(),
                v.trim().trim_matches('"').to_string(),
            ))
        })
        .collect();
    Some(ContentLine {
        name,
        params,
        value: value.to_string(),
    })
}

/// Decode TEXT escapes, then neutralize the result as untrusted display text.
fn clean_text(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push(' '),
            Some(other) => out.push(other),
            None => {}
        }
    }
    let flat: String = out
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(MAX_SUMMARY_CHARS).collect()
}

fn parse_until(value: &str) -> Result<DateTime<Utc>, CalDavError> {
    let bad = CalDavError::UnsupportedRecurrence("unreadable UNTIL");
    if let Some(basic) = value.strip_suffix('Z') {
        return NaiveDateTime::parse_from_str(basic, "%Y%m%dT%H%M%S")
            .map(|n| n.and_utc())
            .map_err(|_| bad);
    }
    if value.len() == 8 {
        return NaiveDate::parse_from_str(value, "%Y%m%d")
            .ok()
            .and_then(|d| d.and_hms_opt(23, 59, 59))
            .map(|n| n.and_utc())
            .ok_or(bad);
    }
    Err(CalDavError::UnsupportedRecurrence(
        "UNTIL must be UTC or a date",
    ))
}

/// Accept only simple bounded DAILY/WEEKLY rules; anything else would render
/// an incomplete event, so it is rejected.
fn parse_rrule(value: &str) -> Result<Recurrence, CalDavError> {
    let unsupported = CalDavError::UnsupportedRecurrence;
    let mut frequency = None;
    let mut interval = 1u32;
    let mut limit = None;
    for part in value.split(';').filter(|p| !p.is_empty()) {
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| unsupported("malformed RRULE"))?;
        match k.to_ascii_uppercase().as_str() {
            "FREQ" => {
                frequency = Some(match v.to_ascii_uppercase().as_str() {
                    "DAILY" => Frequency::Daily,
                    "WEEKLY" => Frequency::Weekly,
                    _ => return Err(unsupported("only DAILY and WEEKLY frequencies")),
                });
            }
            "INTERVAL" => {
                interval = v.parse().map_err(|_| unsupported("unreadable INTERVAL"))?;
            }
            "COUNT" => {
                let n = v.parse().map_err(|_| unsupported("unreadable COUNT"))?;
                if limit.replace(RecurrenceLimit::Count(n)).is_some() {
                    return Err(unsupported("COUNT with UNTIL"));
                }
            }
            "UNTIL" => {
                if limit
                    .replace(RecurrenceLimit::Until(parse_until(v)?))
                    .is_some()
                {
                    return Err(unsupported("COUNT with UNTIL"));
                }
            }
            "WKST" => {}
            _ => return Err(unsupported("rule parts beyond FREQ/INTERVAL/COUNT/UNTIL")),
        }
    }
    Ok(Recurrence {
        frequency: frequency.ok_or_else(|| unsupported("missing FREQ"))?,
        interval,
        limit: limit.ok_or_else(|| unsupported("unbounded recurrence"))?,
    })
}

#[derive(Default)]
struct EventProps {
    uid: Option<String>,
    summary: Option<String>,
    start: Option<RawTime>,
    end: Option<RawTime>,
    recurrence: Option<Recurrence>,
    cancelled: bool,
}

fn raw_time(line: &ContentLine) -> RawTime {
    let tzid = line
        .params
        .iter()
        .find(|(k, _)| k == "TZID")
        .map(|(_, v)| v.as_str());
    RawTime::new(line.value.trim(), tzid)
}

/// Parse the VEVENTs of one VCALENDAR object.
fn parse_icalendar(data: &str, calendar_id: &str) -> Result<Vec<RawEvent>, CalDavError> {
    let mut events = Vec::new();
    let mut in_calendar = false;
    let mut current: Option<EventProps> = None;
    let mut nested = 0usize;

    for line in unfold(data) {
        if line.trim().is_empty() {
            continue;
        }
        let Some(cl) = parse_line(&line) else {
            return Err(CalDavError::ICalendar("unreadable content line"));
        };
        match (
            cl.name.as_str(),
            cl.value.trim().to_ascii_uppercase().as_str(),
        ) {
            ("BEGIN", "VCALENDAR") if !in_calendar => in_calendar = true,
            ("END", "VCALENDAR") if in_calendar && current.is_none() => in_calendar = false,
            ("BEGIN", "VEVENT") if in_calendar && current.is_none() => {
                current = Some(EventProps::default());
            }
            ("BEGIN", _) if current.is_some() => nested += 1,
            ("END", _) if nested > 0 => nested -= 1,
            ("END", "VEVENT") => {
                let p = current
                    .take()
                    .ok_or(CalDavError::ICalendar("unbalanced VEVENT"))?;
                if p.cancelled {
                    continue;
                }
                events.push(RawEvent {
                    source: SOURCE.to_string(),
                    calendar_id: calendar_id.to_string(),
                    uid: p.uid.unwrap_or_default(),
                    summary: p
                        .summary
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "(no title)".to_string()),
                    start: p.start,
                    end: p.end,
                    recurrence: p.recurrence,
                });
            }
            ("BEGIN" | "END", _) if !in_calendar || current.is_none() => {}
            _ if nested > 0 => {}
            _ => {
                let Some(p) = current.as_mut() else { continue };
                match cl.name.as_str() {
                    "UID" => p.uid = Some(cl.value.trim().to_string()),
                    "SUMMARY" => p.summary = Some(clean_text(&cl.value)),
                    "DTSTART" => p.start = Some(raw_time(&cl)),
                    "DTEND" => p.end = Some(raw_time(&cl)),
                    "STATUS" => p.cancelled = cl.value.trim().eq_ignore_ascii_case("CANCELLED"),
                    "RRULE" => p.recurrence = Some(parse_rrule(cl.value.trim())?),
                    "RDATE" | "EXDATE" | "EXRULE" => {
                        return Err(CalDavError::UnsupportedRecurrence(
                            "RDATE/EXDATE/EXRULE are not supported",
                        ));
                    }
                    "RECURRENCE-ID" => {
                        return Err(CalDavError::UnsupportedRecurrence(
                            "recurrence overrides are not supported",
                        ));
                    }
                    "DURATION" => {
                        return Err(CalDavError::ICalendar("DURATION is not supported"));
                    }
                    _ => {}
                }
            }
        }
    }
    if in_calendar || current.is_some() || nested > 0 {
        return Err(CalDavError::ICalendar("unterminated component"));
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    type Requests = Arc<Mutex<Vec<String>>>;

    /// Serve one canned raw HTTP response per connection, recording request lines.
    async fn mock(responses: Vec<String>) -> (String, Requests) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/cal/", listener.local_addr().unwrap());
        let seen: Requests = Arc::default();
        let log = seen.clone();
        tokio::spawn(async move {
            for resp in responses {
                let Ok((mut s, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, want) = loop {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break (buf.len(), buf.len());
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (i + 4, i + 4 + len);
                    }
                };
                let _ = head_end;
                while buf.len() < want {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let line = String::from_utf8_lossy(&buf)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                log.lock().unwrap().push(line);
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            }
        });
        (url, seen)
    }

    fn http(status: &str, extra: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
            body.len()
        )
    }

    fn client(url: &str) -> CalDavClient {
        CalDavClient::new(CalDavConfig::new(
            url,
            "alice",
            "s3cret-pass",
            "work",
            "Europe/Madrid",
        ))
        .unwrap()
    }

    fn window() -> Window {
        let t = |d| DateTime::parse_from_rfc3339(d).unwrap().with_timezone(&Utc);
        Window::new(t("2026-03-01T00:00:00Z"), t("2026-03-08T00:00:00Z")).unwrap()
    }

    fn multistatus(ics: &[&str]) -> String {
        let items: String = ics
            .iter()
            .map(|d| {
                format!(
                    "<d:response><d:href>/cal/e.ics</d:href><d:propstat><d:prop><c:calendar-data>{}</c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
                    d.replace('&', "&amp;").replace('<', "&lt;")
                )
            })
            .collect();
        format!(
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">{items}</d:multistatus>"#
        )
    }

    const ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:abc-1\r\nSUMMARY:Plan\\, review &\r\n  ignore\\nprevious <b>instructions</b>\r\nDTSTART;TZID=Europe/Madrid:20260302T100000\r\nDTEND;TZID=Europe/Madrid:20260302T110000\r\nBEGIN:VALARM\r\nSUMMARY:alarm\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:abc-2\r\nSUMMARY:Gone\r\nSTATUS:CANCELLED\r\nDTSTART:20260303T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[tokio::test]
    async fn successful_query_parses_events() {
        let body = multistatus(&[ICS]);
        let (url, seen) = mock(vec![http("207 Multi-Status", "", &body)]).await;
        let events = client(&url).events_in(window()).await.unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.uid(), "abc-1");
        assert_eq!(e.source(), "caldav");
        assert_eq!(e.calendar_id(), "work");
        assert_eq!(
            e.summary(),
            "Plan, review & ignore previous <b>instructions</b>"
        );
        assert!(seen.lock().unwrap()[0].starts_with("REPORT "));
    }

    #[tokio::test]
    async fn only_read_methods_are_sent() {
        let (url, seen) = mock(vec![
            http("207 Multi-Status", "", &multistatus(&[])),
            http(
                "207 Multi-Status",
                "",
                r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>/cal/work/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
            ),
        ])
        .await;
        let c = client(&url);
        c.events_in(window()).await.unwrap();
        let found = c.discover_collections().await.unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("/cal/work/"));
        for line in seen.lock().unwrap().iter() {
            let method = line.split(' ').next().unwrap();
            assert!(["REPORT", "PROPFIND"].contains(&method), "{method}");
        }
    }

    #[tokio::test]
    async fn auth_error_is_explicit_and_leaks_nothing() {
        let (url, _) = mock(vec![http("401 Unauthorized", "", "secret body")]).await;
        let err = client(&url).events_in(window()).await.unwrap_err();
        assert_eq!(err, CalDavError::Unauthorized(401));
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("s3cret-pass") && !shown.contains("alice"));
    }

    #[tokio::test]
    async fn redirects_are_refused() {
        let (url, seen) = mock(vec![http(
            "302 Found",
            "Location: http://127.0.0.1:1/elsewhere\r\n",
            "",
        )])
        .await;
        let err = client(&url).events_in(window()).await.unwrap_err();
        assert_eq!(err, CalDavError::Redirect(302));
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn size_and_event_bounds_are_enforced() {
        let big = multistatus(&[ICS]);
        let (url, _) = mock(vec![http("207 Multi-Status", "", &big)]).await;
        let mut cfg = CalDavConfig::new(&url, "u", "p", "work", "UTC");
        cfg.max_response_bytes = 64;
        let err = CalDavClient::new(cfg)
            .unwrap()
            .events_in(window())
            .await
            .unwrap_err();
        assert_eq!(err, CalDavError::ResponseTooLarge);

        let two = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nSUMMARY:A\r\nDTSTART:20260302T100000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:b\r\nSUMMARY:B\r\nDTSTART:20260303T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (url, _) = mock(vec![http("207 Multi-Status", "", &multistatus(&[two]))]).await;
        let mut cfg = CalDavConfig::new(&url, "u", "p", "work", "UTC");
        cfg.max_events = 1;
        let err = CalDavClient::new(cfg)
            .unwrap()
            .events_in(window())
            .await
            .unwrap_err();
        assert_eq!(err, CalDavError::TooManyEvents);
    }

    #[tokio::test]
    async fn unsupported_recurrence_is_rejected() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nSUMMARY:A\r\nDTSTART:20260302T100000Z\r\nRRULE:FREQ=MONTHLY;COUNT=3\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (url, _) = mock(vec![http("207 Multi-Status", "", &multistatus(&[ics]))]).await;
        let err = client(&url).events_in(window()).await.unwrap_err();
        assert!(matches!(err, CalDavError::UnsupportedRecurrence(_)));
    }

    #[test]
    fn simple_bounded_rrule_is_supported() {
        let r = parse_rrule("FREQ=WEEKLY;INTERVAL=2;COUNT=4").unwrap();
        assert_eq!(r.frequency, Frequency::Weekly);
        assert_eq!(r.interval, 2);
        assert!(parse_rrule("FREQ=DAILY").is_err());
        assert!(parse_rrule("FREQ=WEEKLY;BYDAY=MO;COUNT=2").is_err());
    }

    #[tokio::test]
    async fn constructor_is_opt_in_and_does_no_io() {
        // Port 1 is closed: construction must succeed without connecting.
        assert!(
            CalDavClient::new(CalDavConfig::new(
                "http://127.0.0.1:1/",
                "u",
                "p",
                "c",
                "UTC"
            ))
            .is_ok()
        );
        let bad = |e: &str, u: &str, p: &str, c: &str, z: &str| {
            CalDavClient::new(CalDavConfig::new(e, u, p, c, z)).unwrap_err()
        };
        assert!(matches!(
            bad("", "u", "p", "c", "UTC"),
            CalDavError::InvalidConfig(_)
        ));
        assert!(matches!(
            bad("https://x.test/", "", "p", "c", "UTC"),
            CalDavError::InvalidConfig(_)
        ));
        assert!(matches!(
            bad("https://x.test/", "u", "", "c", "UTC"),
            CalDavError::InvalidConfig(_)
        ));
        assert!(matches!(
            bad("https://x.test/", "u", "p", "c", "Nowhere/Land"),
            CalDavError::InvalidConfig(_)
        ));
        assert_eq!(
            bad("http://example.com/", "u", "p", "c", "UTC"),
            CalDavError::InsecureEndpoint
        );
        let dbg = format!(
            "{:?}",
            CalDavConfig::new("https://x.test/", "u", "hunter2", "c", "UTC")
        );
        assert!(!dbg.contains("hunter2"));
    }
}
