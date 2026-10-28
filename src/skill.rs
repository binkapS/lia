//! Skills: reusable instructions the assistant can load when it needs them.
//!
//! A skill is a directory holding a `SKILL.md` file. The frontmatter of that
//! file says what the skill is for; the body says how to do it. Only the
//! catalogue — the names and descriptions — goes into the system prompt, so
//! the model knows what is available without paying for the instructions
//! until it decides one applies.
//!
//! ```text
//!   <workspace>/skills/<name>/SKILL.md
//!   <workspace>/.agents/skills/<name>/SKILL.md
//!   ~/.agents/skills/<name>/SKILL.md
//!   <state>/skills/<name>/SKILL.md
//! ```
//!
//! Earlier sources win, so a project can override a personal skill by
//! giving it the same name.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Result, Tool, ToolError, ToolOutput};

/// The largest a `SKILL.md` may be.
pub const MAX_SKILL_BYTES: u64 = 256 * 1024;

/// The largest number of skills loaded from all sources together.
pub const MAX_SKILLS: usize = 200;

/// How deep to look inside a skills directory.
const MAX_DEPTH: usize = 3;

/// Skills that travel inside the binary.
///
/// These are the procedures that make sense on any machine, so they are
/// available even in a workspace that has no `skills` directory. A skill
/// found on disk with the same name wins, which lets a project or a person
/// replace one without changing the program.
pub const BUILTIN_SKILLS: &[(&str, &str)] = &[
    (
        "system-health",
        include_str!("../skills/system-health/SKILL.md"),
    ),
    (
        "system-diagnostics",
        include_str!("../skills/system-diagnostics/SKILL.md"),
    ),
    (
        "security-audit",
        include_str!("../skills/security-audit/SKILL.md"),
    ),
    (
        "deployment-readiness",
        include_str!("../skills/deployment-readiness/SKILL.md"),
    ),
];

/// How much of a description goes into the prompt.
pub const MAX_DESCRIPTION_CHARS: usize = 220;

/// Where a skill was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    /// `<workspace>/skills` — belongs to the project.
    Workspace,
    /// `<workspace>/.agents/skills` — the project's agent directory.
    Project,
    /// `~/.agents/skills` — belongs to the person.
    Personal,
    /// `<state>/skills` — belongs to the installation.
    Managed,
    /// A directory the user named in the config.
    Extra,
    /// A skill that travels inside the binary.
    Builtin,
}

impl SkillSource {
    /// A short label for the interface.
    pub fn as_str(&self) -> &'static str {
        match self {
            SkillSource::Workspace => "workspace",
            SkillSource::Project => "project",
            SkillSource::Personal => "personal",
            SkillSource::Managed => "managed",
            SkillSource::Extra => "extra",
            SkillSource::Builtin => "builtin",
        }
    }
}

/// A skill the assistant can use.
#[derive(Debug, Clone)]
pub struct Skill {
    /// The name the model refers to the skill by.
    pub name: String,
    /// One line saying when to use the skill.
    pub description: String,
    /// The directory holding the skill.
    pub directory: PathBuf,
    /// The `SKILL.md` file.
    pub file: PathBuf,
    /// The instructions, with the frontmatter removed.
    pub body: String,
    /// Where it was found.
    pub source: SkillSource,
}

/// The skills available to an assistant.
#[derive(Debug, Clone, Default)]
pub struct SkillSet {
    skills: Vec<Skill>,
}

impl SkillSet {
    pub fn new(skills: Vec<Skill>) -> Self {
        SkillSet { skills }
    }

    /// Find the skills in a set of roots, in order. Earlier roots win.
    pub fn discover(roots: &[(SkillSource, PathBuf)]) -> Self {
        let mut skills: Vec<Skill> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for (source, root) in roots {
            for file in find_skill_files(root) {
                if skills.len() >= MAX_SKILLS {
                    break;
                }
                let Some(skill) = load(&file, *source) else {
                    continue;
                };
                // A name already claimed by an earlier root keeps it.
                if !seen.insert(skill.name.to_lowercase()) {
                    continue;
                }
                skills.push(skill);
            }
        }

        SkillSet { skills }
    }

    /// Add the skills that travel inside the binary.
    ///
    /// A skill already found on disk keeps its name: the built-in is only a
    /// fallback, never an override.
    pub fn add_builtins(&mut self) {
        for (name, text) in BUILTIN_SKILLS {
            if self.get(name).is_some() {
                continue;
            }
            let (fields, body) = parse_frontmatter(text);
            let name = field(&fields, "name")
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| (*name).to_string());
            if self.get(&name).is_some() {
                continue;
            }
            let description = field(&fields, "description")
                .filter(|description| !description.trim().is_empty())
                .unwrap_or_else(|| first_line(&body));
            self.skills.push(Skill {
                name: name.clone(),
                description,
                directory: PathBuf::from("<builtin>"),
                file: PathBuf::from(format!("builtin:{name}")),
                body,
                source: SkillSource::Builtin,
            });
        }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Skill> {
        self.skills.iter()
    }

    /// Find a skill by name, ignoring case.
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name.trim()))
    }

    /// The catalogue for the system prompt.
    ///
    /// The XML shape is deliberate: it keeps the catalogue apart from the
    /// prose around it, so the model can tell where the list ends.
    pub fn catalog_prompt(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }

        let mut out = String::from("<available_skills>\n");
        for skill in &self.skills {
            out.push_str("  <skill>\n");
            out.push_str(&format!("    <name>{}</name>\n", escape(&skill.name)));
            out.push_str(&format!(
                "    <description>{}</description>\n",
                escape(&compact(&skill.description, MAX_DESCRIPTION_CHARS))
            ));
            out.push_str(&format!(
                "    <location>{}</location>\n",
                escape(&skill.file.to_string_lossy())
            ));
            out.push_str("  </skill>\n");
        }
        out.push_str("</available_skills>");
        out
    }

    /// A one-line summary per skill, for the interface.
    pub fn summary(&self) -> String {
        if self.skills.is_empty() {
            return "no skills found".into();
        }
        self.skills
            .iter()
            .map(|s| {
                format!(
                    "{}  {}  ({})",
                    s.name,
                    compact(&s.description, MAX_DESCRIPTION_CHARS),
                    s.source.as_str()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Find every `SKILL.md` under a root, shallowest first.
fn find_skill_files(root: &Path) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }

    let mut found = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .max_depth(MAX_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name() == "SKILL.md" {
            found.push(entry.path().to_path_buf());
        }
    }
    found.sort();
    found
}

/// Read a skill file.
fn load(file: &Path, source: SkillSource) -> Option<Skill> {
    let metadata = std::fs::metadata(file).ok()?;
    if metadata.len() > MAX_SKILL_BYTES {
        tracing::warn!(
            "skipping skill {}: it is larger than {} bytes",
            file.display(),
            MAX_SKILL_BYTES
        );
        return None;
    }

    let text = std::fs::read_to_string(file).ok()?;
    let (fields, body) = parse_frontmatter(&text);

    let directory = file.parent().unwrap_or(file).to_path_buf();
    // The directory name is a reasonable fallback when the frontmatter is
    // missing or does not name the skill.
    let name = field(&fields, "name")
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| {
            directory
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "skill".to_string())
        });

    let description = field(&fields, "description")
        .filter(|d| !d.trim().is_empty())
        .unwrap_or_else(|| first_line(&body));

    Some(Skill {
        name,
        description,
        directory,
        file: file.to_path_buf(),
        body,
        source,
    })
}

/// Split a document into its frontmatter fields and its body.
///
/// The frontmatter is the block between the first `---` and the next one.
/// Only flat `key: value` pairs are understood, which is all a skill needs.
pub fn parse_frontmatter(text: &str) -> (Vec<(String, String)>, String) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();

    if lines.next().map(str::trim) != Some("---") {
        return (Vec::new(), text.to_string());
    }

    let mut fields = Vec::new();
    let mut body = String::new();
    let mut in_frontmatter = true;

    for line in lines {
        if in_frontmatter {
            if line.trim() == "---" {
                in_frontmatter = false;
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim();
                if key.is_empty() || key.starts_with('#') {
                    continue;
                }
                fields.push((key.to_string(), unquote(value.trim())));
            }
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }

    // A file with no closing fence is all frontmatter and no body.
    if in_frontmatter {
        return (Vec::new(), text.to_string());
    }

    (fields, body.trim_start().to_string())
}

/// Look up a frontmatter field.
fn field(fields: &[(String, String)], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

/// Strip quotes from a frontmatter value.
fn unquote(value: &str) -> String {
    let value = value.trim();
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

/// The first non-empty line of a body, used as a fallback description.
fn first_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .trim_start_matches('#')
        .trim()
        .to_string()
}

/// Shorten a string to a limit, on a character boundary.
pub fn compact(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Escape text for an XML element.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A tool that lists the skills and loads their instructions.
pub struct SkillTool {
    skills: Arc<SkillSet>,
}

impl SkillTool {
    pub fn new(skills: Arc<SkillSet>) -> Self {
        SkillTool { skills }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "List the available skills, or read the instructions for one. \
         Call with a skill name to load it, or with no name to see what there is."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The skill to load. Leave it out to list the available skills."
                }
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty());

        match name {
            None => {
                if self.skills.is_empty() {
                    return Ok(ToolOutput::ok("There are no skills available."));
                }
                let mut out = String::from("Available skills:\n");
                for skill in self.skills.iter() {
                    out.push_str(&format!(
                        "- {}: {}\n",
                        skill.name,
                        compact(&skill.description, MAX_DESCRIPTION_CHARS)
                    ));
                }
                Ok(ToolOutput::ok(out))
            }
            Some(name) => {
                let Some(skill) = self.skills.get(name) else {
                    let available: Vec<&str> =
                        self.skills.iter().map(|s| s.name.as_str()).collect();
                    return Err(ToolError::InvalidArgs {
                        tool: "skill".into(),
                        reason: if available.is_empty() {
                            "there are no skills available".into()
                        } else {
                            format!(
                                "no skill named '{name}' (available: {})",
                                available.join(", ")
                            )
                        },
                    });
                };

                let mut out = format!("# {}\n\n", skill.name);
                if !skill.description.trim().is_empty() {
                    out.push_str(&skill.description);
                    out.push_str("\n\n");
                }
                out.push_str(&skill.body);
                Ok(ToolOutput::ok(out))
            }
        }
    }
}
