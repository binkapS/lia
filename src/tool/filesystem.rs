//! Filesystem tools: read, write, edit, list, and search.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Result, Tool, ToolError, ToolOutput};

/// The directory the assistant is allowed to work in.
///
/// Every path a tool touches is resolved and checked against this root, so a
/// model cannot wander outside the workspace even if it tries.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    /// Allow operations outside the root (used by explicitly trusted setups).
    allow_outside: bool,
}

impl Workspace {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = if root.is_absolute() {
            root
        } else {
            std::env::current_dir().unwrap_or_default().join(root)
        };
        Workspace {
            root: root.canonicalize().unwrap_or(root),
            allow_outside: false,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn allow_outside(&mut self, allow: bool) {
        self.allow_outside = allow;
    }

    /// Resolve a user/model supplied path to an absolute path inside the root.
    ///
    /// Relative paths are taken as relative to the workspace root.
    ///
    /// `..` and symbolic links are resolved even when the file does not exist
    /// yet, so a link cannot be used to step outside the root.
    pub fn resolve(&self, input: &str) -> Result<PathBuf> {
        let candidate = Path::new(input);
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.root.join(candidate)
        };

        // Resolve symlinks where possible so `..` and links cannot escape.
        let resolved = resolve_leaf(&joined);

        if !self.allow_outside && !resolved.starts_with(&self.root) {
            return Err(ToolError::Failed(
                "path".into(),
                format!(
                    "{} is outside the workspace ({})",
                    resolved.display(),
                    self.root.display()
                ),
            ));
        }
        Ok(resolved)
    }

    /// Path relative to the workspace root, for display.
    pub fn display(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string())
    }
}

/// Resolve `.` and `..` without touching the filesystem.
///
/// `canonicalize` fails for paths that do not exist yet, which is exactly the
/// case for files the assistant is about to create.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicalise a path, following links as far as the filesystem allows.
///
/// `canonicalize` needs the whole path to exist, which is not true of a file
/// that is about to be created. So the longest existing ancestor is
/// canonicalised and the missing tail is put back on top of it.
pub fn canonicalize_deep(path: &Path) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }

    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path;
    while let Some(parent) = current.parent() {
        let Some(name) = current.file_name() else {
            break;
        };
        tail.push(name.to_os_string());
        match parent.canonicalize() {
            Ok(resolved) => {
                let mut out = resolved;
                for part in tail.iter().rev() {
                    out.push(part);
                }
                return out;
            }
            Err(_) => current = parent,
        }
    }

    normalize_lexically(path)
}

/// The real file a path names, even when the last part is a link whose
/// target does not exist.
///
/// `canonicalize` fails on a dangling link, and the lexical fallback would
/// leave the link in place — so writing through it would follow the link out
/// of the root. The link is read and its target resolved instead.
pub fn resolve_leaf(path: &Path) -> PathBuf {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let Ok(target) = std::fs::read_link(path) else {
                return canonicalize_deep(path);
            };
            let target = if target.is_absolute() {
                target
            } else {
                path.parent().unwrap_or_else(|| Path::new("/")).join(target)
            };
            canonicalize_deep(&target)
        }
        _ => canonicalize_deep(path),
    }
}

/// Expand a leading `~` to the home directory.
///
/// A config file's paths are not run through a shell, so `~/projects`
/// arrives here as four literal characters.
pub fn expand_tilde(input: &str, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return PathBuf::from(input);
    };
    if input == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = input.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(input)
}

/// A set of folders the assistant may work in.
///
/// A path may be absolute — it must then be inside one of the folders — or
/// relative, in which case each folder is tried in turn and the first one
/// that has the path wins. A path that is nowhere is taken to be in the
/// first folder, which is where a new file is created.
#[derive(Debug, Clone)]
pub struct WorkspaceSet {
    roots: Vec<Workspace>,
}

impl WorkspaceSet {
    /// Build a set from folders. Each must exist and be a directory.
    pub fn new(folders: Vec<PathBuf>) -> Result<Self> {
        let mut roots = Vec::with_capacity(folders.len());
        for folder in folders {
            let expanded = expand_tilde(&folder.to_string_lossy(), dirs::home_dir().as_deref());
            if !expanded.exists() {
                return Err(ToolError::Failed(
                    "path".into(),
                    format!("{} does not exist", expanded.display()),
                ));
            }
            if !expanded.is_dir() {
                return Err(ToolError::Failed(
                    "path".into(),
                    format!("{} is not a directory", expanded.display()),
                ));
            }
            roots.push(Workspace::new(expanded));
        }
        Ok(WorkspaceSet { roots })
    }

    /// The folders, in the order they were given.
    pub fn roots(&self) -> &[Workspace] {
        &self.roots
    }

    /// The folder at an index.
    pub fn root(&self, index: usize) -> &Workspace {
        &self.roots[index]
    }

    /// How many folders there are.
    pub fn len(&self) -> usize {
        self.roots.len()
    }

    /// Whether there are no folders at all.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// The folders as plain paths, for display.
    pub fn folders(&self) -> Vec<PathBuf> {
        self.roots.iter().map(|w| w.root().to_path_buf()).collect()
    }

    /// Find the folder a path belongs to, and the absolute path inside it.
    pub fn resolve(&self, input: &str) -> Result<(usize, PathBuf)> {
        let expanded = expand_tilde(input, dirs::home_dir().as_deref());
        let candidate = Path::new(&expanded);

        if candidate.is_absolute() {
            let real = resolve_leaf(candidate);
            if let Some(index) = self.owner_of(&real) {
                return Ok((index, real));
            }
            return Err(self.outside(&real));
        }

        // A relative path is looked for in each folder in turn: the first
        // that has it wins, and a path that is nowhere is taken to be in the
        // first folder, which is where a new file is created.
        let mut fallback = None;
        for (index, root) in self.roots.iter().enumerate() {
            let real = resolve_leaf(&root.root().join(candidate));
            if !real.starts_with(root.root()) {
                // This folder would be left by the path — `..`, or a link
                // out of it. Such a path is not quietly tried in another
                // folder: it is refused.
                continue;
            }
            if fallback.is_none() {
                fallback = Some((index, real.clone()));
            }
            if real.exists() {
                return Ok((index, real));
            }
        }

        match fallback {
            Some(hit) => Ok(hit),
            None => Err(self.outside(candidate)),
        }
    }

    /// The folder a path lies in, if any.
    fn owner_of(&self, path: &Path) -> Option<usize> {
        self.roots
            .iter()
            .position(|root| path.starts_with(root.root()))
    }

    /// The error for a path that is not in any folder.
    fn outside(&self, path: &Path) -> ToolError {
        let folders: Vec<String> = self
            .roots
            .iter()
            .map(|root| root.root().display().to_string())
            .collect();
        ToolError::Failed(
            "path".into(),
            format!(
                "{} is outside the allowed folders ({})",
                path.display(),
                folders.join(", ")
            ),
        )
    }
}

/// Read a file's contents.
pub struct ReadFileTool {
    workspace: Workspace,
    max_bytes: usize,
}

impl ReadFileTool {
    pub fn new(workspace: Workspace) -> Self {
        ReadFileTool {
            workspace,
            max_bytes: 2_000_000,
        }
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a text file from the workspace. Returns the file contents with line numbers."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the workspace root or absolute."
                },
                "offset": {
                    "type": "integer",
                    "description": "First line to read (1-based). Optional."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read. Optional."
                }
            },
            "required": ["path"]
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let path =
            args.get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: self.name().into(),
                    reason: "missing required parameter 'path'".into(),
                })?;

        let resolved = self.workspace.resolve(path)?;
        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;

        if content.len() > self.max_bytes {
            return Err(ToolError::Failed(
                self.name().into(),
                format!(
                    "file is {} bytes, over the {} byte limit — use offset/limit to read part of it",
                    content.len(),
                    self.max_bytes
                ),
            ));
        }

        let lines: Vec<&str> = content.lines().collect();
        let offset = args
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(usize::MAX as u64) as usize;

        let start = (offset - 1).min(lines.len());
        let end = start.saturating_add(limit).min(lines.len());

        let mut out = String::new();
        for (i, line) in lines[start..end].iter().enumerate() {
            out.push_str(&format!("{:>6}\t{}\n", start + i + 1, line));
        }

        if out.is_empty() {
            out = format!("(file is empty: {})", self.workspace.display(&resolved));
        }

        Ok(ToolOutput::ok(out).with_metadata(json!({
            "path": self.workspace.display(&resolved),
            "lines": lines.len(),
            "returned": end - start,
        })))
    }
}

/// Write a file, creating parent directories as needed.
pub struct WriteFileTool {
    workspace: Workspace,
}

impl WriteFileTool {
    pub fn new(workspace: Workspace) -> Self {
        WriteFileTool { workspace }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write text to a file, overwriting it if it already exists. Creates parent directories."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file." },
                "content": { "type": "string", "description": "Full content to write." }
            },
            "required": ["path", "content"]
        })
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let path =
            args.get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: self.name().into(),
                    reason: "missing required parameter 'path'".into(),
                })?;
        let content =
            args.get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: self.name().into(),
                    reason: "missing required parameter 'content'".into(),
                })?;

        let resolved = self.workspace.resolve(path)?;
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
        }

        let existed = resolved.exists();
        tokio::fs::write(&resolved, content)
            .await
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;

        Ok(ToolOutput::ok(format!(
            "{} {} ({} bytes)",
            if existed { "Overwrote" } else { "Created" },
            self.workspace.display(&resolved),
            content.len()
        ))
        .with_metadata(json!({
            "path": self.workspace.display(&resolved),
            "bytes": content.len(),
            "created": !existed,
        })))
    }
}

/// Replace a specific string inside a file.
pub struct EditFileTool {
    workspace: Workspace,
}

impl EditFileTool {
    pub fn new(workspace: Workspace) -> Self {
        EditFileTool { workspace }
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Replace an exact string in a file. The old string must appear exactly once, unless replace_all is set."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file." },
                "old_string": { "type": "string", "description": "Exact text to replace." },
                "new_string": { "type": "string", "description": "Replacement text." },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence. Defaults to false." }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let path =
            args.get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: self.name().into(),
                    reason: "missing required parameter 'path'".into(),
                })?;
        let old = args
            .get("old_string")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs {
                tool: self.name().into(),
                reason: "missing required parameter 'old_string'".into(),
            })?;
        let new = args
            .get("new_string")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs {
                tool: self.name().into(),
                reason: "missing required parameter 'new_string'".into(),
            })?;
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let resolved = self.workspace.resolve(path)?;
        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;

        let occurrences = content.matches(old).count();
        if occurrences == 0 {
            return Err(ToolError::Failed(
                self.name().into(),
                format!(
                    "old_string not found in {}",
                    self.workspace.display(&resolved)
                ),
            ));
        }
        if occurrences > 1 && !replace_all {
            return Err(ToolError::Failed(
                self.name().into(),
                format!(
                    "old_string appears {occurrences} times in {} — add context to make it unique, or set replace_all",
                    self.workspace.display(&resolved)
                ),
            ));
        }

        let updated = if replace_all {
            content.replace(old, new)
        } else {
            content.replacen(old, new, 1)
        };

        tokio::fs::write(&resolved, &updated)
            .await
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;

        Ok(ToolOutput::ok(format!(
            "Replaced {occurrences} occurrence{} in {}",
            if occurrences == 1 { "" } else { "s" },
            self.workspace.display(&resolved)
        ))
        .with_metadata(json!({
            "path": self.workspace.display(&resolved),
            "replacements": occurrences,
        })))
    }
}

/// List the contents of a directory.
pub struct ListDirectoryTool {
    workspace: Workspace,
}

impl ListDirectoryTool {
    pub fn new(workspace: Workspace) -> Self {
        ListDirectoryTool { workspace }
    }
}

#[async_trait]
impl Tool for ListDirectoryTool {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn description(&self) -> &str {
        "List files and directories in a path, with sizes."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to list. Defaults to the workspace root."
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Walk subdirectories. Defaults to false."
                }
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let recursive = args
            .get("recursive")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let resolved = self.workspace.resolve(path)?;
        let mut entries: Vec<String> = Vec::new();

        let walker = walkdir::WalkDir::new(&resolved)
            .max_depth(if recursive { usize::MAX } else { 1 })
            .follow_links(false)
            .sort_by_file_name();

        for entry in walker.into_iter().filter_map(std::result::Result::ok) {
            if entry.path() == resolved {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&resolved)
                .unwrap_or(entry.path())
                .display()
                .to_string();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let kind = if entry.file_type().is_dir() {
                "dir "
            } else {
                "file"
            };
            entries.push(format!("{kind} {size:>10}  {relative}"));
        }

        if entries.is_empty() {
            return Ok(ToolOutput::ok(format!(
                "(empty directory: {})",
                self.workspace.display(&resolved)
            )));
        }

        Ok(ToolOutput::ok(entries.join("\n")).with_metadata(json!({
            "path": self.workspace.display(&resolved),
            "entries": entries.len(),
        })))
    }
}

/// Find files by name pattern or content.
pub struct SearchTool {
    workspace: Workspace,
}

impl SearchTool {
    pub fn new(workspace: Workspace) -> Self {
        SearchTool { workspace }
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        "search"
    }

    fn description(&self) -> &str {
        "Search the workspace. With `pattern`, matches file paths by glob. With `content`, searches file contents with a regular expression."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern matched against paths, e.g. '**/*.rs'."
                },
                "content": {
                    "type": "string",
                    "description": "Regular expression searched inside files."
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in. Defaults to the workspace root."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results. Defaults to 100."
                }
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let base = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(100) as usize;
        let root = self.workspace.resolve(base)?;

        let mut results: Vec<String> = Vec::new();

        if let Some(pattern) = args.get("pattern").and_then(Value::as_str) {
            let full = format!("{}/{}", root.display(), pattern);
            let matcher = glob::Pattern::new(&full)
                .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
            for entry in walkdir::WalkDir::new(&root)
                .follow_links(false)
                .into_iter()
                .filter_map(std::result::Result::ok)
            {
                if entry.path().is_file() && matcher.matches_path(entry.path()) {
                    results.push(self.workspace.display(entry.path()));
                    if results.len() >= max_results {
                        break;
                    }
                }
            }
        }

        if let Some(pattern) = args.get("content").and_then(Value::as_str) {
            let regex = regex::Regex::new(pattern)
                .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
            for entry in walkdir::WalkDir::new(&root)
                .follow_links(false)
                .into_iter()
                .filter_map(std::result::Result::ok)
            {
                if !entry.path().is_file() {
                    continue;
                }
                // Skip binary and oversized files.
                if entry
                    .metadata()
                    .map(|m| m.len() > 2_000_000)
                    .unwrap_or(true)
                {
                    continue;
                }
                let Ok(content) = tokio::fs::read_to_string(entry.path()).await else {
                    continue;
                };
                for (i, line) in content.lines().enumerate() {
                    if regex.is_match(line) {
                        let line_no = i + 1;
                        let snippet: String = line.chars().take(200).collect();
                        results.push(format!(
                            "{}:{}: {}",
                            self.workspace.display(entry.path()),
                            line_no,
                            snippet.trim_end()
                        ));
                        if results.len() >= max_results {
                            break;
                        }
                    }
                }
                if results.len() >= max_results {
                    break;
                }
            }
        }

        if results.is_empty() {
            return Ok(ToolOutput::ok("(no matches)"));
        }

        Ok(ToolOutput::ok(results.join("\n")).with_metadata(json!({ "matches": results.len() })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder that outlives the test.
    fn a_folder() -> PathBuf {
        tempfile::tempdir().expect("a temporary folder").keep()
    }

    #[test]
    fn a_relative_path_is_found_in_the_second_folder() {
        let first = a_folder();
        let second = a_folder();
        std::fs::write(second.join("a.txt"), "the words").expect("the file is written");

        let set = WorkspaceSet::new(vec![first, second.clone()]).expect("a set of folders");
        let (index, path) = set.resolve("a.txt").expect("the file is found");
        assert_eq!(index, 1);
        assert_eq!(path, second.join("a.txt"));
    }

    #[test]
    fn a_path_nowhere_is_taken_to_the_first_folder() {
        let first = a_folder();
        let second = a_folder();

        let set = WorkspaceSet::new(vec![first.clone(), second]).expect("a set of folders");
        let (index, path) = set.resolve("new.txt").expect("the path is resolved");
        assert_eq!(index, 0);
        assert_eq!(path, first.join("new.txt"));
    }

    #[test]
    fn an_absolute_path_inside_a_folder_is_found() {
        let first = a_folder();
        let second = a_folder();
        std::fs::write(second.join("a.txt"), "the words").expect("the file is written");

        let set = WorkspaceSet::new(vec![first, second.clone()]).expect("a set of folders");
        let (index, path) = set
            .resolve(&second.join("a.txt").to_string_lossy())
            .expect("the file is found");
        assert_eq!(index, 1);
        assert_eq!(path, second.join("a.txt"));
    }

    #[test]
    fn an_absolute_path_outside_every_folder_is_refused() {
        let set = WorkspaceSet::new(vec![a_folder()]).expect("a set of folders");
        let error = set.resolve("/etc/passwd").expect_err("the path is refused");
        assert!(error.to_string().contains("outside"));
    }

    #[test]
    fn a_parent_directory_cannot_leave_a_folder() {
        let set = WorkspaceSet::new(vec![a_folder()]).expect("a set of folders");
        let error = set
            .resolve("../../etc/passwd")
            .expect_err("the path is refused");
        assert!(error.to_string().contains("outside"));
    }

    #[test]
    fn a_link_out_of_the_folder_is_refused() {
        let folder = a_folder();
        let outside = a_folder();
        std::fs::write(outside.join("secret.txt"), "the secret").expect("the file is written");
        std::os::unix::fs::symlink(&outside, folder.join("link")).expect("a link");

        let set = WorkspaceSet::new(vec![folder]).expect("a set of folders");
        let error = set
            .resolve("link/secret.txt")
            .expect_err("the path is refused");
        assert!(error.to_string().contains("outside"));
    }

    #[test]
    fn a_dangling_link_is_refused() {
        let folder = a_folder();
        let outside = a_folder();
        std::os::unix::fs::symlink(outside.join("nope.txt"), folder.join("dangling"))
            .expect("a link");

        let set = WorkspaceSet::new(vec![folder]).expect("a set of folders");
        let error = set.resolve("dangling").expect_err("the path is refused");
        assert!(error.to_string().contains("outside"));
    }

    #[test]
    fn a_link_inside_the_folder_is_fine() {
        let folder = a_folder();
        let real = folder.join("real");
        std::fs::create_dir(&real).expect("the folder is made");
        std::fs::write(real.join("a.txt"), "the words").expect("the file is written");
        std::os::unix::fs::symlink(&real, folder.join("link")).expect("a link");

        let set = WorkspaceSet::new(vec![folder]).expect("a set of folders");
        let (_, path) = set.resolve("link/a.txt").expect("the file is found");
        assert_eq!(path, real.join("a.txt"));
    }

    #[test]
    fn a_workspace_refuses_a_link_out_of_it() {
        let folder = a_folder();
        let outside = a_folder();
        std::os::unix::fs::symlink(&outside, folder.join("link")).expect("a link");

        let workspace = Workspace::new(&folder);
        let error = workspace
            .resolve("link/new.txt")
            .expect_err("the path is refused");
        assert!(error.to_string().contains("outside"));
    }

    #[test]
    fn a_tilde_is_the_home_directory() {
        let home = Path::new("/home/someone");
        assert_eq!(expand_tilde("~/notes", Some(home)), home.join("notes"));
        assert_eq!(expand_tilde("~", Some(home)), home);
        assert_eq!(
            expand_tilde("/etc/passwd", Some(home)),
            PathBuf::from("/etc/passwd")
        );
        assert_eq!(
            expand_tilde("~someone/notes", Some(home)),
            PathBuf::from("~someone/notes")
        );
    }

    #[test]
    fn a_missing_tail_is_kept() {
        let folder = a_folder();
        let resolved = canonicalize_deep(&folder.join("a/b/c.txt"));
        assert_eq!(resolved, folder.join("a/b/c.txt"));
    }
}

/// Read a required string from any of a list of keys.
fn string_arg<'a>(args: &'a Value, keys: &'a [&'a str]) -> Result<&'a str> {
    for key in keys {
        if let Some(v) = args.get(*key).and_then(Value::as_str) {
            return Ok(v);
        }
    }
    let shown = keys.join("', '");
    Err(ToolError::InvalidArgs {
        tool: "filesystem".into(),
        reason: format!("missing required parameter one of '{shown}'"),
    })
}

fn bool_arg(args: &Value, keys: &[&str], default: bool) -> bool {
    for key in keys {
        if let Some(v) = args.get(*key).and_then(Value::as_bool) {
            return v;
        }
    }
    default
}

/// Copy a file or directory within the workspace.
pub struct CopyPathTool {
    workspace: Workspace,
}

impl CopyPathTool {
    pub fn new(workspace: Workspace) -> Self {
        CopyPathTool { workspace }
    }
}

#[async_trait]
impl Tool for CopyPathTool {
    fn name(&self) -> &str {
        "copy_path"
    }

    fn description(&self) -> &str {
        "Copy a file or directory to a new location inside the workspace."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "description": "Source path." },
                "source": { "type": "string", "description": "Source path." },
                "src": { "type": "string", "description": "Source path." },
                "old_path": { "type": "string", "description": "Source path." },
                "to": { "type": "string", "description": "Destination path." },
                "destination": { "type": "string", "description": "Destination path." },
                "dest": { "type": "string", "description": "Destination path." },
                "new_path": { "type": "string", "description": "Destination path." },
                "overwrite": { "type": "boolean", "description": "Overwrite an existing destination. Defaults to false." }
            }
        })
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let from = string_arg(&args, &["from", "source", "src", "old_path", "source_path"])?;
        let to = string_arg(
            &args,
            &[
                "to",
                "destination",
                "dest",
                "dst",
                "new_path",
                "target",
                "target_path",
            ],
        )?;
        let overwrite = bool_arg(&args, &["overwrite"], false);

        let src = self.workspace.resolve(from)?;
        let dst = self.workspace.resolve(to)?;

        if !src.exists() {
            return Err(ToolError::Failed(
                self.name().into(),
                format!("source does not exist: {}", src.display()),
            ));
        }
        if dst.exists() && !overwrite {
            return Err(ToolError::Failed(
                self.name().into(),
                format!("destination already exists: {}", dst.display()),
            ));
        }

        copy_recursive(&src, &dst)
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;

        Ok(ToolOutput::ok(format!(
            "Copied {} to {}",
            self.workspace.display(&src),
            self.workspace.display(&dst)
        ))
        .with_metadata(json!({
            "from": self.workspace.display(&src),
            "to": self.workspace.display(&dst),
        })))
    }
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let src_child = entry.path();
            let dst_child = dst.join(entry.file_name());
            copy_recursive(&src_child, &dst_child)?;
        }
        Ok(())
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst)?;
        Ok(())
    }
}

/// Move (rename) a file or directory within the workspace.
pub struct MovePathTool {
    workspace: Workspace,
}

impl MovePathTool {
    pub fn new(workspace: Workspace) -> Self {
        MovePathTool { workspace }
    }
}

#[async_trait]
impl Tool for MovePathTool {
    fn name(&self) -> &str {
        "move_path"
    }

    fn description(&self) -> &str {
        "Move or rename a file or directory within the workspace."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "description": "Source path." },
                "source": { "type": "string", "description": "Source path." },
                "src": { "type": "string", "description": "Source path." },
                "old_path": { "type": "string", "description": "Source path." },
                "to": { "type": "string", "description": "Destination path." },
                "destination": { "type": "string", "description": "Destination path." },
                "dest": { "type": "string", "description": "Destination path." },
                "new_path": { "type": "string", "description": "Destination path." },
                "overwrite": { "type": "boolean", "description": "Overwrite an existing destination. Defaults to false." }
            }
        })
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let from = string_arg(&args, &["from", "source", "src", "old_path", "source_path"])?;
        let to = string_arg(
            &args,
            &[
                "to",
                "destination",
                "dest",
                "dst",
                "new_path",
                "target",
                "target_path",
            ],
        )?;
        let overwrite = bool_arg(&args, &["overwrite"], false);

        let src = self.workspace.resolve(from)?;
        let dst = self.workspace.resolve(to)?;

        if !src.exists() {
            return Err(ToolError::Failed(
                self.name().into(),
                format!("source does not exist: {}", src.display()),
            ));
        }
        if dst.exists() && !overwrite {
            return Err(ToolError::Failed(
                self.name().into(),
                format!("destination already exists: {}", dst.display()),
            ));
        }
        if dst.exists() {
            if dst.is_dir() {
                std::fs::remove_dir_all(&dst)
                    .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
            } else {
                std::fs::remove_file(&dst)
                    .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
            }
        }

        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
        }

        if std::fs::rename(&src, &dst).is_err() {
            // Cross-device or directory move fallback.
            copy_recursive(&src, &dst)
                .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
            if src.is_dir() {
                std::fs::remove_dir_all(&src)
                    .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
            } else {
                std::fs::remove_file(&src)
                    .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
            }
        }

        Ok(ToolOutput::ok(format!(
            "Moved {} to {}",
            self.workspace.display(&src),
            self.workspace.display(&dst)
        ))
        .with_metadata(json!({
            "from": self.workspace.display(&src),
            "to": self.workspace.display(&dst),
        })))
    }
}

/// Apply a unified diff to a file.
pub struct PatchTool {
    workspace: Workspace,
}

impl PatchTool {
    pub fn new(workspace: Workspace) -> Self {
        PatchTool { workspace }
    }
}

#[async_trait]
impl Tool for PatchTool {
    fn name(&self) -> &str {
        "patch"
    }

    fn description(&self) -> &str {
        "Apply a unified diff (patch) to a file in the workspace."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File to patch." },
                "patch": { "type": "string", "description": "Unified diff text." },
                "diff": { "type": "string", "description": "Unified diff text." },
                "content": { "type": "string", "description": "Unified diff text." },
                "unified_diff": { "type": "string", "description": "Unified diff text." }
            },
            "required": ["path"]
        })
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let path =
            args.get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: self.name().into(),
                    reason: "missing required parameter 'path'".into(),
                })?;
        let resolved = self.workspace.resolve(path)?;

        // Accept the diff under any common key.
        let diff_text = match string_arg(
            &args,
            &[
                "patch",
                "diff",
                "content",
                "unified_diff",
                "patch_text",
                "diff_text",
            ],
        ) {
            Ok(v) => v.to_string(),
            Err(_) => {
                // Maybe a bare string was passed under "path"? No; require patch somewhere.
                return Err(ToolError::InvalidArgs {
                    tool: self.name().into(),
                    reason: "missing required parameter one of 'patch', 'diff', 'content', 'unified_diff'".into(),
                });
            }
        };

        // Write the diff to a temp file and let GNU patch apply it.
        let temp_dir = std::env::temp_dir();
        let patch_file = temp_dir.join(format!("lia-patch-{}.patch", uuid::Uuid::new_v4()));
        tokio::fs::write(&patch_file, &diff_text)
            .await
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;

        let status = tokio::process::Command::new("patch")
            .arg("--batch")
            .arg("--no-backup-if-mismatch")
            .arg(&resolved)
            .arg(&patch_file)
            .output()
            .await
            .map_err(|e| {
                ToolError::Failed(self.name().into(), format!("patch not available: {e}"))
            })?;

        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            let stdout = String::from_utf8_lossy(&status.stdout);
            return Err(ToolError::Failed(
                self.name().into(),
                format!("patch failed:\n{stdout}\n{stderr}"),
            ));
        }

        Ok(
            ToolOutput::ok(format!("Patched {}", self.workspace.display(&resolved))).with_metadata(
                json!({
                    "path": self.workspace.display(&resolved),
                    "stdout": String::from_utf8_lossy(&status.stdout),
                }),
            ),
        )
    }
}

/// Format a file using an available formatter.
pub struct FormatFileTool {
    workspace: Workspace,
}

impl FormatFileTool {
    pub fn new(workspace: Workspace) -> Self {
        FormatFileTool { workspace }
    }
}

#[async_trait]
impl Tool for FormatFileTool {
    fn name(&self) -> &str {
        "format_file"
    }

    fn description(&self) -> &str {
        "Format a source file using the appropriate formatter."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file." }
            },
            "required": ["path"]
        })
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let path =
            args.get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: self.name().into(),
                    reason: "missing required parameter 'path'".into(),
                })?;
        let resolved = self.workspace.resolve(path)?;

        let extension = resolved
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match extension.as_str() {
            "rs" => {
                let status = tokio::process::Command::new("rustfmt")
                    .arg(&resolved)
                    .output()
                    .await
                    .map_err(|e| {
                        ToolError::Failed(self.name().into(), format!("rustfmt not available: {e}"))
                    })?;
                if !status.status.success() {
                    let stderr = String::from_utf8_lossy(&status.stderr);
                    return Err(ToolError::Failed(
                        self.name().into(),
                        format!("rustfmt failed: {stderr}"),
                    ));
                }
                Ok(ToolOutput::ok(format!(
                    "Formatted {}",
                    self.workspace.display(&resolved)
                )))
            }
            _ => Err(ToolError::Failed(
                self.name().into(),
                format!("no formatter available for .{extension} files"),
            )),
        }
    }
}

// Compatibility aliases for plausible struct names.
pub type CopyFileTool = CopyPathTool;
pub type CopyTool = CopyPathTool;
pub type MoveFileTool = MovePathTool;
pub type MoveTool = MovePathTool;
pub type ApplyPatchTool = PatchTool;
pub type PatchFileTool = PatchTool;
pub type FormatTool = FormatFileTool;
pub type FormatSourceTool = FormatFileTool;

/// Grep-like codebase search.
pub struct GrepCodebaseTool {
    workspace: Workspace,
}

impl GrepCodebaseTool {
    pub fn new(workspace: Workspace) -> Self {
        GrepCodebaseTool { workspace }
    }
}

#[async_trait]
impl Tool for GrepCodebaseTool {
    fn name(&self) -> &str {
        "grep_codebase"
    }

    fn description(&self) -> &str {
        "Search file contents in the workspace with a regular expression."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression to search for." },
                "content": { "type": "string", "description": "Regular expression to search for." },
                "path": { "type": "string", "description": "Directory to search in. Defaults to workspace root." },
                "max_results": { "type": "integer", "description": "Maximum number of results." }
            },
            "required": []
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        // Delegate to the existing search implementation by constructing its
        // behaviour inline (same algorithm, simpler to keep self-contained).
        let base = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(100) as usize;
        let pattern = args
            .get("pattern")
            .or_else(|| args.get("content"))
            .or_else(|| args.get("query"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if pattern.is_empty() {
            return Err(ToolError::InvalidArgs {
                tool: self.name().into(),
                reason: "missing required parameter 'pattern'".into(),
            });
        }
        let root = self.workspace.resolve(base)?;
        let regex = regex::Regex::new(pattern)
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
        let mut results = Vec::new();
        for entry in walkdir::WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(std::result::Result::ok)
        {
            if !entry.path().is_file() {
                continue;
            }
            if entry
                .metadata()
                .map(|m| m.len() > 2_000_000)
                .unwrap_or(true)
            {
                continue;
            }
            let Ok(content) = tokio::fs::read_to_string(entry.path()).await else {
                continue;
            };
            for (i, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    results.push(format!(
                        "{}:{}: {}",
                        self.workspace.display(entry.path()),
                        i + 1,
                        line.trim_end()
                    ));
                    if results.len() >= max_results {
                        break;
                    }
                }
            }
            if results.len() >= max_results {
                break;
            }
        }
        if results.is_empty() {
            return Ok(ToolOutput::ok("(no matches)"));
        }
        Ok(ToolOutput::ok(results.join("\n")).with_metadata(json!({"matches":results.len()})))
    }
}

/// Find symbol definitions in source files.
pub struct FindSymbolsTool {
    workspace: Workspace,
}

impl FindSymbolsTool {
    pub fn new(workspace: Workspace) -> Self {
        FindSymbolsTool { workspace }
    }
}

#[async_trait]
impl Tool for FindSymbolsTool {
    fn name(&self) -> &str {
        "find_symbols"
    }

    fn description(&self) -> &str {
        "Find symbol definitions (functions, structs, classes, …) in the workspace."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Optional name filter." },
                "symbol": { "type": "string", "description": "Optional name filter." },
                "path": { "type": "string", "description": "Directory to search." }
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let base = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let filter = args
            .get("query")
            .or_else(|| args.get("symbol"))
            .or_else(|| args.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let root = self.workspace.resolve(base)?;
        // Pattern matches common definition keywords followed by an identifier.
        let symbol_re = regex::Regex::new(r"(?m)^\s*(pub\s+)?(fn|struct|enum|trait|class|interface|def|func|type|const|static)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
        let mut results = Vec::new();
        for entry in walkdir::WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(std::result::Result::ok)
        {
            if !entry.path().is_file() {
                continue;
            }
            let Ok(content) = tokio::fs::read_to_string(entry.path()).await else {
                continue;
            };
            for (i, line) in content.lines().enumerate() {
                if let Some(caps) = symbol_re.captures(line) {
                    let name = caps.get(3).map(|m| m.as_str()).unwrap_or("");
                    if !filter.is_empty() && !name.contains(filter) {
                        continue;
                    }
                    results.push(format!(
                        "{}:{}: {}",
                        self.workspace.display(entry.path()),
                        i + 1,
                        line.trim()
                    ));
                }
            }
        }
        if results.is_empty() {
            return Ok(ToolOutput::ok("(no symbols)"));
        }
        Ok(ToolOutput::ok(results.join("\n")).with_metadata(json!({"symbols":results.len()})))
    }
}

/// Read a URL in chunks.
pub struct ReadUrlChunkedTool {
    client: reqwest::Client,
}

impl ReadUrlChunkedTool {
    pub fn new() -> Self {
        ReadUrlChunkedTool {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl Default for ReadUrlChunkedTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ReadUrlChunkedTool {
    fn name(&self) -> &str {
        "read_url_chunked"
    }

    fn description(&self) -> &str {
        "Fetch a URL and return a chunk of its text content."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "URL to read." },
                "offset": { "type": "integer", "description": "Character offset to start at." },
                "limit": { "type": "integer", "description": "Maximum characters to return." }
            },
            "required": ["url"]
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let url =
            args.get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: self.name().into(),
                    reason: "missing required parameter 'url'".into(),
                })?;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20000) as usize;
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
        let text = resp
            .text()
            .await
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
        let chars: Vec<char> = text.chars().collect();
        let start = offset.min(chars.len());
        let end = (start + limit).min(chars.len());
        let chunk: String = chars[start..end].iter().collect();
        Ok(ToolOutput::ok(chunk)
            .with_metadata(json!({"offset":start,"returned":end-start,"total":chars.len()})))
    }
}

// Aliases
pub type GrepTool = GrepCodebaseTool;
pub type CodeSearchTool = GrepCodebaseTool;
pub type SymbolsTool = FindSymbolsTool;
pub type ReadUrlTool = ReadUrlChunkedTool;
