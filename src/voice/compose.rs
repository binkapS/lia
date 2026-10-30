//! The words the voice says, composed like a person's.
//!
//! What the transcript shows is not what the listener should hear: the
//! print is detailed, the speech is composed. Lists are read in the order
//! a person would give them, code and wide tables are pointed at instead
//! of recited, the marks a screen can skim become the words an ear needs,
//! and the first sentence is opened with the quiet courtesy of a
//! household that has kept its butler a very long time.
//!
//! The browser composes with the same turns (the page's `voice.js`), so
//! the terminal and the window sound like the same presence.

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

/// The natural courtesies the voice opens a reply with, in the manner of
/// a helpful assistant. Each is used bare, and rarely the honorific follows.
const OPENERS: [&str; 7] = [
    "I'll get right on that",
    "Certainly",
    "Of course",
    "Very good",
    "Very well",
    "Let me handle that",
    "Going right ahead",
];

/// Honorifics, said far less often than not.
const ADDRESS: [&str; 5] = ["", "", "", "", ""];

/// Words a reply that already opens with an answer is not bowed onto
/// twice ("Certainly. Certainly, the build is green.").
const ACKNOWLEDGED: [&str; 20] = [
    "okay",
    "alright",
    "right",
    "sure",
    "so",
    "well",
    "here",
    "yes",
    "no",
    "hi",
    "hey",
    "hello",
    "certainly",
    "of course",
    "very good",
    "very well",
    "on it",
    "absolutely",
    "definitely",
    "for sure",
];

/// What is said when the answer puts code on the screen.
const CODE_CUE: &str = "I've laid the code out on screen — open it when you're ready.";

/// What is said when the answer puts a wide table on the screen.
const TABLE_CUE: &str = "I have placed the detailed table overview on the console";

/// The words the roster of a long list ends with.
const CONNECTORS: [&str; 5] = ["Then,", "Also,", "Next,", "And,", "Besides,"];

/// A reply whose first words are a list gets its courtesy once, and a
/// list is said five deep at most; the rest stay on screen.
const ROSTER_SPOKEN: usize = 5;

/// A line of a markdown list: a mark, or a short number, then the words.
fn bullet() -> &'static Regex {
    static BULLET: OnceLock<Regex> = OnceLock::new();
    BULLET.get_or_init(|| Regex::new(r"^(?:[-*+•·]|\d{1,2}[.)])\s+(.+)$").unwrap())
}

/// A labelled link: the words between the brackets stand on their own.
fn labelled_link() -> &'static Regex {
    static LINK: OnceLock<Regex> = OnceLock::new();
    LINK.get_or_init(|| Regex::new(r"\[([^\]]+)\]\([^)]*\)").unwrap())
}

/// A picture link says nothing.
fn picture_link() -> &'static Regex {
    static PICTURE: OnceLock<Regex> = OnceLock::new();
    PICTURE.get_or_init(|| Regex::new(r"!\[[^\]]*\]\([^)]*\)").unwrap())
}

/// A bare address: pointed at, not spelled out.
fn bare_link() -> &'static Regex {
    static LINK: OnceLock<Regex> = OnceLock::new();
    LINK.get_or_init(|| {
        Regex::new(r"(?i)(^|[\s(])(?:https?://|www\.)[^\s>)\]]+").unwrap()
    })
}

/// A heading mark at the front of a line.
fn heading() -> &'static Regex {
    static HEADING: OnceLock<Regex> = OnceLock::new();
    HEADING.get_or_init(|| Regex::new(r"^#{1,6}\s+").unwrap())
}

/// A blockquote mark at the front of a line.
fn quote() -> &'static Regex {
    static QUOTE: OnceLock<Regex> = OnceLock::new();
    QUOTE.get_or_init(|| Regex::new(r"^\s*>\s*").unwrap())
}

/// A number followed by a unit a screen can skim.
fn unit() -> &'static Regex {
    static UNIT: OnceLock<Regex> = OnceLock::new();
    UNIT.get_or_init(|| {
        Regex::new(r"(?i)(\d+(?:[.,]\d+)?)\s*(GB/s|MB/s|KB/s|ms|GB|MB|KB)\b").unwrap()
    })
}

fn unit_words(unit: &str) -> &'static str {
    match unit.to_ascii_lowercase().as_str() {
        "gb/s" => "gigabytes per second",
        "mb/s" => "megabytes per second",
        "kb/s" => "kilobytes per second",
        "ms" => "milliseconds",
        "gb" => "gigabytes",
        "mb" => "megabytes",
        "kb" => "kilobytes",
        _ => "",
    }
}

/// "at ≥ 80" and "at least 80" say it once.
fn doubled_at() -> &'static Regex {
    static DOUBLED: OnceLock<Regex> = OnceLock::new();
    DOUBLED.get_or_init(|| Regex::new(r"(?i)\bat\s+at\s+(least|most)\b").unwrap())
}

/// The commas a table row or a separator left behind.
fn loose_commas() -> &'static Regex {
    static COMMAS: OnceLock<Regex> = OnceLock::new();
    COMMAS.get_or_init(|| Regex::new(r"\s*,\s*").unwrap())
}

/// Icons have no voice; the words around them are enough.
fn icons() -> &'static Regex {
    static ICONS: OnceLock<Regex> = OnceLock::new();
    ICONS.get_or_init(|| Regex::new(r"\p{Extended_Pictographic}|\p{Emoji_Modifier}").unwrap())
}

/// A fenced block of code, read whole rather than line by line.
fn code_block() -> &'static Regex {
    static CODE: OnceLock<Regex> = OnceLock::new();
    CODE.get_or_init(|| Regex::new(r"(?s)```.*?```").unwrap())
}

/// Turn what the transcript shows into what the voice should say.
///
/// The transcript keeps every detail; the ear only wants the shape of
/// it. Symbols and units become words, marks become pauses, and nothing
/// the ear cannot follow is left standing.
pub fn speakable(text: &str) -> String {
    let text = labelled_link().replace_all(text, "$1");
    let text = bare_link().replace_all(&text, "${1}the link on your screen ");
    let text = heading().replace(&text, "");
    let text = quote().replace(&text, "");
    let text = text
        .replace("→", " becomes ")
        .replace("->", " becomes ")
        .replace("=>", " becomes ")
        .replace('%', " percent")
        .replace('&', " and ")
        .replace("°C", " degrees")
        .replace("°c", " degrees")
        .replace("°F", " degrees Fahrenheit")
        .replace("°f", " degrees Fahrenheit")
        .replace('°', " degrees")
        .replace("≈", " about ")
        .replace('×', " by ")
        .replace("≥", " at least ")
        .replace("≤", " at most ")
        /* Mid-sentence separators pause like a spoken breath, not a
           beep. */
        .replace(['·', '•', '–'], ", ")
        .replace("—", " — ")
        .replace('~', " about ")
        .replace('|', ", ");
    let text = unit()
        .replace_all(&text, |caps: &regex::Captures| {
            format!("{} {}", &caps[1], unit_words(&caps[2]))
        })
        .into_owned();
    let text = doubled_at().replace_all(&text, "at $1");
    let text = loose_commas().replace_all(&text, ", ");
    let text = text.trim_matches(|ch| matches!(ch, ',' | ':' | ';' | ' '));
    let text = icons().replace_all(text, "");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Strip the marks that should not be read aloud.
fn clean(text: &str) -> String {
    let text = picture_link().replace_all(text, "");
    let text = labelled_link().replace_all(&text, "$1");
    let text = text.replace(['`', '*', '_', '#', '>'], "");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the reply already answers in kind, so it is not bowed onto.
fn acknowledges(out: &str) -> bool {
    let low = out.to_lowercase();
    ACKNOWLEDGED.iter().any(|word| {
        low.starts_with(word)
            && low[word.len()..]
                .chars()
                .next()
                .is_none_or(|ch| !ch.is_alphanumeric())
    })
}

/// The words the voice says, and the state it keeps while an answer is
/// still arriving.
///
/// The interface feeds the composer a line at a time, or a sentence at a
/// time for prose that runs on; the composer answers with what should be
/// said now, and keeps the roster of a list until the list is over.
pub struct SpeechComposer {
    /// Whether the courtesy of the house is still owed.
    lead: bool,
    /// The rows of a list that is still being read.
    bullets: Vec<String>,
    /// Whether the words now arriving belong to a table on screen.
    table: bool,
    /// Whether the answer is inside a fenced code block.
    in_code: bool,
    /// The turns of a private coin for picking courtesies.
    rng: u64,
}

impl Default for SpeechComposer {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeechComposer {
    /// A composer, for the first word of a first reply.
    pub fn new() -> Self {
        let mut rng = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15);
        rng ^= rng >> 33;
        rng = rng.wrapping_mul(0xFF51AFD7ED558CCD);
        rng ^= rng >> 33;
        Self {
            lead: true,
            bullets: Vec::new(),
            table: false,
            in_code: false,
            rng: rng | 1,
        }
    }

    /// A fresh reply, so nothing of the last one carries over.
    pub fn begin(&mut self) {
        self.lead = true;
        self.bullets.clear();
        self.table = false;
        self.in_code = false;
    }

    /// Whether the answer is inside a code block, so a sentence that is
    /// still arriving is held whole rather than split.
    pub fn in_code(&self) -> bool {
        self.in_code
    }

    /// The next turn of the private coin.
    fn next(&mut self) -> u64 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        self.rng
            .wrapping_mul(0x2545F4914F6CDD1D)
            .swap_bytes()
    }

    fn pick(&mut self, options: &[&'static str]) -> &'static str {
        let at = (self.next() % options.len() as u64) as usize;
        options[at]
    }

    /// The courtesy the reply is opened with.
    fn opener(&mut self) -> String {
        let courtesy = self.pick(&OPENERS);
        let honorific = self.pick(&ADDRESS);
        if honorific.is_empty() {
            format!("{courtesy}.")
        } else {
            format!("{courtesy}, {honorific}.")
        }
    }

    /// One complete line of the answer, as it becomes speech.
    ///
    /// Most lines say one sentence. A fence says where the code lives; a
    /// wide table is named once and then left to the eye; a list row
    /// waits, so the roster can be read the way a person would give it.
    pub fn line(&mut self, line: &str) -> Vec<String> {
        let text = line.trim();
        if text.starts_with("```") || text.starts_with("~~~") {
            let mut said = self.flush();
            self.in_code = !self.in_code;
            self.table = false;
            if self.in_code {
                said.push(CODE_CUE.to_string());
            }
            return said;
        }
        if self.in_code || text.is_empty() {
            return Vec::new();
        }
        /* A table's rule row is punctuation, not prose. */
        if text
            .chars()
            .all(|ch| matches!(ch, '|' | '-' | ':' | ' '))
        {
            return Vec::new();
        }
        if let Some(item) = bullet().captures(text).and_then(|caps| caps.get(1)) {
            self.bullets.push(item.as_str().to_string());
            self.table = false;
            return Vec::new();
        }
        let mut said = self.flush();
        /* A wide table is a picture in the transcript, not a paragraph.
           The first row names it; the rest are the same picture. */
        let cells = text
            .split('|')
            .filter(|part| !part.trim().is_empty())
            .count();
        if cells > 3 {
            if !self.table {
                self.table = true;
                said.push(TABLE_CUE.to_string());
            }
            return said;
        }
        self.table = false;
        said.push(self.spoken_line(text));
        said
    }

    /// A piece of prose that ended mid-line, said now that its sentence
    /// is complete.
    pub fn spoken_chunk(&mut self, chunk: &str) -> String {
        self.spoken_line(chunk.trim())
    }

    /// Whatever is left to say when the answer is over: the roster of a
    /// list that was never followed by prose.
    pub fn flush(&mut self) -> Vec<String> {
        let items = std::mem::take(&mut self.bullets);
        if items.is_empty() {
            return Vec::new();
        }
        /* A list is said five deep at most; the rest stay on screen. */
        let total = items.len();
        let said: Vec<String> = if total > ROSTER_SPOKEN + 1 {
            items.into_iter().take(ROSTER_SPOKEN).collect()
        } else {
            items
        };
        let rest = total.saturating_sub(said.len());
        /* "First, …" already opens like a person would, but a list that
           opens the whole reply deserves its own courtesy first. */
        let opened = self.lead && said.len() > 1;
        self.lead = false;
        let mut out = Vec::new();
        if opened {
            out.push(self.opener());
        }
        let last = said.len().saturating_sub(1);
        for (index, item) in said.iter().enumerate() {
            let words = clean(&speakable(item));
            if words.is_empty() {
                continue;
            }
            let prefix = if said.len() == 1 {
                "One more thing,"
            } else if index == 0 {
                "First,"
            } else if index == last && said.len() >= 3 {
                "And finally,"
            } else {
                CONNECTORS[(index - 1).min(CONNECTORS.len() - 1)]
            };
            out.push(format!("{prefix} {words}."));
        }
        if rest > 0 {
            out.push(format!(
                "And there are {rest} more, which I've left on your screen."
            ));
        }
        out
    }

    /// A line the person asked to hear immediately, outside a turn: the
    /// marks are taken off and the words are said as they are.
    pub fn direct(&self, text: &str) -> String {
        let text = code_block().replace_all(text, " code block ");
        clean(&speakable(&text))
    }

    /// Compose one sentence of prose for the voice.
    fn spoken_line(&mut self, text: &str) -> String {
        let mut out = clean(&speakable(text));
        /* A line that ends in a colon is a heading; the voice drops it
           there. */
        out = out.trim_end_matches([':', ';']).to_string();
        if out.is_empty() {
            return String::new();
        }
        if self.lead {
            self.lead = false;
            if !acknowledges(&out) {
                out = format!("{} {out}", self.opener());
            }
        }
        if !out.ends_with(['.', '!', '?']) {
            out.push('.');
        }
        out
    }
}