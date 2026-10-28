//! An MCP server that offers a fixed set of folders as tools.
//!
//! The server is deliberately small: it offers file access and nothing else,
//! and it offers it only inside the folders it was started with. A path that
//! leaves them is refused, whatever the caller meant by it.
//!
//! It speaks MCP over its standard streams, one JSON-RPC message per line,
//! which is the transport lia's own client already speaks.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::PROTOCOL_VERSION;
use crate::tool::filesystem::{
    EditFileTool, ListDirectoryTool, ReadFileTool, SearchTool, WorkspaceSet, WriteFileTool,
    expand_tilde,
};
use crate::tool::{Result as ToolResult, Tool, ToolError, ToolOutput};

/// The name the server calls itself in its handshake.
pub const SERVER_NAME: &str = "lia-files";

/// One of the tools the server offers.
struct ToolSpec {
    name: &'static str,
    description: &'static str,
    /// Whether the tool only reads.
    read_only: bool,
    /// Whether the tool destroys what was there.
    destructive: bool,
}

/// The tools the server offers.
const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "read_file",
        description: "Read a text file from one of the allowed folders. Returns the contents with line numbers.",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "write_file",
        description: "Write text to a file in one of the allowed folders, overwriting it if it is already there. Creates parent directories.",
        read_only: false,
        destructive: true,
    },
    ToolSpec {
        name: "edit_file",
        description: "Replace an exact string in a file in one of the allowed folders. The old string must appear exactly once, unless replace_all is set.",
        read_only: false,
        destructive: true,
    },
    ToolSpec {
        name: "list_directory",
        description: "List the files and directories in one of the allowed folders, with their sizes.",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "search_files",
        description: "Search the allowed folders. With `pattern`, matches file paths by glob. With `content`, searches file contents with a regular expression.",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "create_directory",
        description: "Create a directory in one of the allowed folders, along with any parent directories that are missing.",
        read_only: false,
        destructive: false,
    },
    ToolSpec {
        name: "move_file",
        description: "Move or rename a file inside the allowed folders.",
        read_only: false,
        destructive: true,
    },
    ToolSpec {
        name: "get_file_info",
        description: "Describe a file in one of the allowed folders: what it is, how large it is, and when it was last changed.",
        read_only: true,
        destructive: false,
    },
];

/// A file server: MCP tools that work inside a fixed set of folders.
pub struct FileServer {
    roots: WorkspaceSet,
    read_only: bool,
}

impl FileServer {
    /// Build a server over the given folders. Each folder must exist.
    pub fn new(folders: Vec<PathBuf>) -> anyhow::Result<Self> {
        Ok(FileServer {
            roots: WorkspaceSet::new(folders)?,
            read_only: false,
        })
    }

    /// Build a server that cannot change anything.
    pub fn read_only(folders: Vec<PathBuf>) -> anyhow::Result<Self> {
        Ok(FileServer {
            roots: WorkspaceSet::new(folders)?,
            read_only: true,
        })
    }

    /// The folders the server was given.
    pub fn folders(&self) -> Vec<PathBuf> {
        self.roots.folders()
    }

    /// Whether the server refuses to change anything.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// What the model is told about the server.
    fn instructions(&self) -> String {
        let folders: Vec<String> = self
            .roots
            .folders()
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        let mode = if self.read_only {
            " It is read-only: nothing may be written, moved or created."
        } else {
            ""
        };
        format!(
            "The folders this server may work in: {}. A path is relative to one of these folders, and an absolute path must be inside one of them. A path that leaves them is refused.{mode}",
            folders.join(", ")
        )
    }

    /// The tools the server offers, as MCP tool descriptions.
    pub fn tools(&self) -> Vec<Value> {
        TOOLS
            .iter()
            .filter(|tool| !(self.read_only && !tool.read_only))
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": schema_of(tool.name),
                    "annotations": {
                        "readOnlyHint": tool.read_only,
                        "destructiveHint": tool.destructive,
                    },
                })
            })
            .collect()
    }

    /// Run one tool by name.
    ///
    /// An unknown tool is [`ToolError::Unknown`], which the caller turns into
    /// a protocol error; everything else is a failure of the tool itself,
    /// which the model is told about so it can try again.
    pub async fn call(&self, name: &str, arguments: Value) -> ToolResult<ToolOutput> {
        let spec = match TOOLS.iter().find(|tool| tool.name == name) {
            Some(spec) => spec,
            None => return Err(ToolError::Unknown(name.to_string())),
        };

        if self.read_only && !spec.read_only {
            return Err(ToolError::Failed(
                name.to_string(),
                "this server is read-only: it may not change anything".into(),
            ));
        }

        match name {
            "read_file" => self.read_file(&arguments).await,
            "write_file" => self.write_file(&arguments).await,
            "edit_file" => self.edit_file(&arguments).await,
            "list_directory" => self.list_directory(&arguments).await,
            "search_files" => self.search_files(&arguments).await,
            "create_directory" => self.create_directory(&arguments).await,
            "move_file" => self.move_file(&arguments).await,
            "get_file_info" => self.get_file_info(&arguments).await,
            _ => Err(ToolError::Unknown(name.to_string())),
        }
    }

    /// The path an argument names, resolved to a real file inside a folder.
    fn resolve(&self, input: &str) -> ToolResult<(usize, PathBuf)> {
        self.roots.resolve(input)
    }

    /// The path an argument names, before any link is followed.
    fn raw(&self, input: &str, index: usize) -> PathBuf {
        let expanded = expand_tilde(input, dirs::home_dir().as_deref());
        let candidate = Path::new(&expanded);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.roots.root(index).root().join(candidate)
        }
    }

    async fn read_file(&self, arguments: &Value) -> ToolResult<ToolOutput> {
        let input = path_argument(arguments, "read_file")?;
        let (index, path) = self.resolve(&input)?;
        let tool = ReadFileTool::new(self.roots.root(index).clone());
        tool.run(forward(arguments, &path, &["offset", "limit"]))
            .await
    }

    async fn write_file(&self, arguments: &Value) -> ToolResult<ToolOutput> {
        let input = path_argument(arguments, "write_file")?;
        let (index, path) = self.resolve(&input)?;
        let tool = WriteFileTool::new(self.roots.root(index).clone());
        tool.run(forward(arguments, &path, &["content"])).await
    }

    async fn edit_file(&self, arguments: &Value) -> ToolResult<ToolOutput> {
        let input = path_argument(arguments, "edit_file")?;
        let (index, path) = self.resolve(&input)?;
        let tool = EditFileTool::new(self.roots.root(index).clone());
        tool.run(forward(
            arguments,
            &path,
            &["old_string", "new_string", "replace_all"],
        ))
        .await
    }

    async fn list_directory(&self, arguments: &Value) -> ToolResult<ToolOutput> {
        let input = arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .to_string();
        let (index, path) = self.resolve(&input)?;
        let tool = ListDirectoryTool::new(self.roots.root(index).clone());
        tool.run(forward(arguments, &path, &["recursive"])).await
    }

    async fn search_files(&self, arguments: &Value) -> ToolResult<ToolOutput> {
        let directory = arguments.get("path").and_then(Value::as_str);

        // A directory given names the folder to search; without one, every
        // folder is searched.
        let only = match directory {
            Some(dir) => Some(self.resolve(dir)?.0),
            None => None,
        };

        let mut results: Vec<String> = Vec::new();
        for index in 0..self.roots.len() {
            if only.is_some_and(|wanted| wanted != index) {
                continue;
            }
            let workspace = self.roots.root(index).clone();
            let tool = SearchTool::new(workspace);
            let mut args = json!({});
            for key in ["pattern", "content", "max_results"] {
                if let Some(value) = arguments.get(key) {
                    args[key] = value.clone();
                }
            }
            if let Some(dir) = directory {
                let (_, path) = self.resolve(dir)?;
                args["path"] = json!(path.to_string_lossy());
            }

            let output = tool.run(args).await?;
            if output.content == "(no matches)" {
                continue;
            }
            for line in output.content.lines() {
                // The hits of two folders look the same otherwise, because
                // each is written relative to its own folder.
                if self.roots.len() > 1 {
                    results.push(format!(
                        "{}: {}",
                        self.roots.root(index).root().display(),
                        line
                    ));
                } else {
                    results.push(line.to_string());
                }
            }
        }

        if results.is_empty() {
            return Ok(ToolOutput::ok("(no matches)"));
        }

        // The limit is applied to each folder on its own, so it is applied
        // again to the whole list.
        let max = arguments
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(100) as usize;
        results.truncate(max);

        Ok(ToolOutput::ok(results.join("\n")).with_metadata(json!({ "matches": results.len() })))
    }

    async fn create_directory(&self, arguments: &Value) -> ToolResult<ToolOutput> {
        let input = path_argument(arguments, "create_directory")?;
        let (_, path) = self.resolve(&input)?;
        tokio::fs::create_dir_all(&path)
            .await
            .map_err(|e| ToolError::Failed("create_directory".into(), e.to_string()))?;
        Ok(ToolOutput::ok(format!("Created {}", path.display())))
    }

    async fn move_file(&self, arguments: &Value) -> ToolResult<ToolOutput> {
        let source = arguments
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs {
                tool: "move_file".into(),
                reason: "missing required parameter 'source'".into(),
            })?;
        let destination = arguments
            .get("destination")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs {
                tool: "move_file".into(),
                reason: "missing required parameter 'destination'".into(),
            })?;

        let (_, from) = self.resolve(source)?;
        let (_, to) = self.resolve(destination)?;

        tokio::fs::rename(&from, &to)
            .await
            .map_err(|e| ToolError::Failed("move_file".into(), e.to_string()))?;
        Ok(ToolOutput::ok(format!(
            "Moved {} to {}",
            from.display(),
            to.display()
        )))
    }

    async fn get_file_info(&self, arguments: &Value) -> ToolResult<ToolOutput> {
        let input = path_argument(arguments, "get_file_info")?;
        let (index, path) = self.resolve(&input)?;

        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|e| ToolError::Failed("get_file_info".into(), e.to_string()))?;
        let is_link = std::fs::symlink_metadata(self.raw(&input, index))
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);

        let kind = if meta.is_dir() {
            "directory"
        } else if meta.is_file() {
            "file"
        } else {
            "other"
        };

        let mut out = format!("{}: {kind}, {} bytes", path.display(), meta.len());
        if let Ok(modified) = meta.modified() {
            let when = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            out.push_str(&format!(", changed at {when}"));
        }
        if is_link {
            out.push_str(", reached through a symbolic link");
        }

        Ok(ToolOutput::ok(out))
    }

    /// Answer one JSON-RPC message.
    ///
    /// Returns the response to send, or `None` for a notification, which is
    /// not answered.
    pub async fn handle(&self, message: Value) -> Option<Value> {
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(json!({}));

        // A notification is never answered. The one exception is a
        // notification that arrives with an id, which a client that got the
        // protocol wrong would otherwise wait for the answer to forever.
        if method.starts_with("notifications/") {
            return id.map(|id| result(id, json!({})));
        }
        let id = id?;

        let response = match method {
            "initialize" => result(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
                    "instructions": self.instructions(),
                }),
            ),
            "ping" => result(id, json!({})),
            "tools/list" => result(id, json!({ "tools": self.tools() })),
            "tools/call" => match self.call_tool(&params).await {
                Ok(value) => result(id, value),
                Err((code, message)) => failure(id, code, &message),
            },
            other => failure(id, -32601, &format!("unknown method: {other}")),
        };

        Some(response)
    }

    /// Run the tool a `tools/call` names.
    async fn call_tool(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((-32602, "the tool name is missing".to_string()))?;
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

        match self.call(name, arguments).await {
            Ok(output) => Ok(json!({
                "content": [{ "type": "text", "text": output.content }],
                "isError": output.is_error,
            })),
            // A tool that does not exist is a protocol error; a tool that
            // failed is a result the model can read and recover from.
            Err(ToolError::Unknown(_)) => Err((-32602, format!("unknown tool: {name}"))),
            Err(e) => Ok(json!({
                "content": [{ "type": "text", "text": e.to_string() }],
                "isError": true,
            })),
        }
    }
}

/// A JSON-RPC success.
fn result(id: Value, value: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": value })
}

/// A JSON-RPC error.
fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// The `path` an argument names.
fn path_argument(arguments: &Value, tool: &str) -> ToolResult<String> {
    arguments
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ToolError::InvalidArgs {
            tool: tool.into(),
            reason: "missing required parameter 'path'".into(),
        })
}

/// The arguments a tool is given, with the path replaced by the real one.
///
/// The path the model wrote is never handed on: the server resolves it
/// first, so the tool works on a real file inside a folder and cannot be
/// pointed elsewhere.
fn forward(arguments: &Value, path: &Path, keys: &[&str]) -> Value {
    let mut out = json!({ "path": path.to_string_lossy() });
    for key in keys {
        if let Some(value) = arguments.get(*key) {
            out[*key] = value.clone();
        }
    }
    out
}

/// The JSON Schema of a tool.
fn schema_of(name: &str) -> Value {
    let path = json!({
        "type": "string",
        "description": "The path, relative to one of the allowed folders or absolute inside one of them."
    });
    match name {
        "read_file" => json!({
            "type": "object",
            "properties": {
                "path": path,
                "offset": { "type": "integer", "description": "First line to read (1-based). Optional." },
                "limit": { "type": "integer", "description": "Maximum number of lines to read. Optional." },
            },
            "required": ["path"]
        }),
        "write_file" => json!({
            "type": "object",
            "properties": {
                "path": path,
                "content": { "type": "string", "description": "The full content to write." },
            },
            "required": ["path", "content"]
        }),
        "edit_file" => json!({
            "type": "object",
            "properties": {
                "path": path,
                "old_string": { "type": "string", "description": "The exact text to replace." },
                "new_string": { "type": "string", "description": "The text to put in its place." },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence. Defaults to false." },
            },
            "required": ["path", "old_string", "new_string"]
        }),
        "list_directory" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The directory to list, relative to one of the allowed folders. Optional." },
                "recursive": { "type": "boolean", "description": "Walk subdirectories. Defaults to false." },
            }
        }),
        "search_files" => json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern matched against paths, e.g. '**/*.rs'." },
                "content": { "type": "string", "description": "Regular expression searched inside files." },
                "path": { "type": "string", "description": "The directory to search in. Optional: every allowed folder is searched." },
                "max_results": { "type": "integer", "description": "Maximum number of results. Defaults to 100." },
            }
        }),
        "create_directory" => json!({
            "type": "object",
            "properties": { "path": path },
            "required": ["path"]
        }),
        "move_file" => json!({
            "type": "object",
            "properties": {
                "source": { "type": "string", "description": "The file to move." },
                "destination": { "type": "string", "description": "Where it should go." },
            },
            "required": ["source", "destination"]
        }),
        "get_file_info" => json!({
            "type": "object",
            "properties": { "path": path },
            "required": ["path"]
        }),
        _ => json!({ "type": "object", "properties": {} }),
    }
}

/// Serve MCP over the standard streams until the input ends.
///
/// Nothing but JSON-RPC may be written to the output stream: a line of
/// anything else would break the conversation with the client.
pub async fn serve_stdio(server: FileServer) -> anyhow::Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => server.handle(message).await,
            Err(e) => Some(failure(
                Value::Null,
                -32700,
                &format!("the line is not JSON: {e}"),
            )),
        };
        if let Some(response) = response {
            let mut text = serde_json::to_string(&response)?;
            text.push('\n');
            out.write_all(text.as_bytes()).await?;
            out.flush().await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folders() -> Vec<PathBuf> {
        let dir = tempfile::tempdir().expect("a temporary folder");
        // The folder has to outlive the server, so it is leaked on purpose.
        let path = dir.keep();
        vec![path]
    }

    #[tokio::test]
    async fn the_handshake_names_the_folders() {
        let server = FileServer::new(folders()).expect("a server");
        let answer = server
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": PROTOCOL_VERSION },
            }))
            .await
            .expect("an answer");

        assert_eq!(answer["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(answer["result"]["serverInfo"]["name"], SERVER_NAME);
        let instructions = answer["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains(&server.folders()[0].display().to_string()));
    }

    #[tokio::test]
    async fn a_notification_is_not_answered() {
        let server = FileServer::new(folders()).expect("a server");
        let answer = server
            .handle(json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {},
            }))
            .await;
        assert!(answer.is_none());
    }

    #[tokio::test]
    async fn an_unknown_method_is_refused() {
        let server = FileServer::new(folders()).expect("a server");
        let answer = server
            .handle(json!({ "jsonrpc": "2.0", "id": 1, "method": "nope" }))
            .await
            .expect("an answer");
        assert_eq!(answer["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn an_unknown_tool_is_refused() {
        let server = FileServer::new(folders()).expect("a server");
        let answer = server
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "nope", "arguments": {} },
            }))
            .await
            .expect("an answer");
        assert_eq!(answer["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn a_failing_tool_is_a_result_and_not_an_error() {
        let server = FileServer::new(folders()).expect("a server");
        let answer = server
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "read_file", "arguments": { "path": "missing.txt" } },
            }))
            .await
            .expect("an answer");
        assert!(answer.get("error").is_none());
        assert_eq!(answer["result"]["isError"], true);
    }

    #[test]
    fn a_read_only_server_offers_no_write_tools() {
        let server = FileServer::read_only(folders()).expect("a server");
        let names: Vec<String> = server
            .tools()
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_string))
            .collect();
        assert!(names.contains(&"read_file".to_string()));
        assert!(!names.contains(&"write_file".to_string()));
    }

    #[tokio::test]
    async fn a_read_only_server_refuses_a_write() {
        let server = FileServer::read_only(folders()).expect("a server");
        let output = server
            .call("write_file", json!({ "path": "a.txt", "content": "x" }))
            .await
            .expect_err("the write is refused");
        assert!(output.to_string().contains("read-only"));
    }
}
