//! Tools for the assistant's memory.
//!
//! Three tools: search the memory, read a file from it, and remember
//! something. The provenance of a remembered fact is decided by the agent,
//! not by the model — the model cannot promote something it read on the web
//! into a trusted fact by asking nicely.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Result, Tool, ToolContext, ToolError, ToolOutput};
use crate::memory::{MemoryStore, Origin};

impl ToolContext {
    /// The origin to record for a fact the model wants to keep.
    ///
    /// The user's own words are the owner's. Anything the assistant worked
    /// out is the agent's. And once something from the network has been seen,
    /// everything else in the turn is untrusted — the model may be repeating
    /// what it just read.
    pub fn origin_for(&self, text: &str) -> Origin {
        if self.tainted {
            return Origin::Untrusted;
        }
        let text = text.trim();
        if !text.is_empty()
            && !self.user_text.trim().is_empty()
            && self.user_text.to_lowercase().contains(&text.to_lowercase())
        {
            return Origin::Owner;
        }
        Origin::Agent
    }
}

/// Search the assistant's memory.
pub struct MemorySearchTool {
    store: Arc<MemoryStore>,
}

impl MemorySearchTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        MemorySearchTool { store }
    }
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn description(&self) -> &str {
        "Search the assistant's memory for a phrase. Returns matching lines \
         with the file and line number they came from."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The phrase to look for."
                }
            },
            "required": ["query"]
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let query =
            args.get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: "memory_search".into(),
                    reason: "missing required parameter 'query'".into(),
                })?;

        let hits = self.store.search(query);
        if hits.is_empty() {
            return Ok(ToolOutput::ok(format!(
                "nothing in memory matches '{query}'"
            )));
        }

        let mut out = format!("{} match(es) for '{query}':\n", hits.len());
        for hit in hits {
            out.push_str(&format!("{}:{}: {}\n", hit.file, hit.line, hit.text));
        }
        Ok(ToolOutput::ok(out))
    }
}

/// Read one memory file.
pub struct MemoryGetTool {
    store: Arc<MemoryStore>,
}

impl MemoryGetTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        MemoryGetTool { store }
    }
}

#[async_trait]
impl Tool for MemoryGetTool {
    fn name(&self) -> &str {
        "memory_get"
    }

    fn description(&self) -> &str {
        "Read one of the assistant's memory files, such as MEMORY.md, \
         USER.md or a daily note."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The memory file to read, for example 'MEMORY.md'."
                }
            },
            "required": ["name"]
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let name =
            args.get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: "memory_get".into(),
                    reason: "missing required parameter 'name'".into(),
                })?;

        let text = self
            .store
            .get(name)
            .map_err(|e| ToolError::Failed("memory_get".into(), e.to_string()))?;
        Ok(ToolOutput::ok(text))
    }
}

/// Remember something for later.
pub struct RememberTool {
    store: Arc<MemoryStore>,
}

impl RememberTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        RememberTool { store }
    }
}

#[async_trait]
impl Tool for RememberTool {
    fn name(&self) -> &str {
        "remember"
    }

    fn description(&self) -> &str {
        "Remember a fact for later conversations. Use it when the user tells \
         you something worth keeping, or when you work out something about \
         their project that would be expensive to rediscover."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The fact to remember, in one or two sentences."
                }
            },
            "required": ["text"]
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let text =
            args.get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: "remember".into(),
                    reason: "missing required parameter 'text'".into(),
                })?;

        // The origin is added by the agent, which knows what the turn has
        // seen. Without it the fact is the agent's own.
        let origin = args
            .get("__origin")
            .and_then(Value::as_str)
            .and_then(|o| match o {
                "owner" => Some(Origin::Owner),
                "agent" => Some(Origin::Agent),
                "untrusted" => Some(Origin::Untrusted),
                "system" => Some(Origin::System),
                _ => None,
            })
            .unwrap_or(Origin::Agent);

        let message = self
            .store
            .remember(text, origin)
            .map_err(|e| ToolError::Failed("remember".into(), e.to_string()))?;
        Ok(ToolOutput::ok(message))
    }

    /// The origin comes from the turn, not from the model.
    fn with_context(&self, args: Value, context: &ToolContext) -> Value {
        let Some(object) = args.as_object() else {
            return args;
        };
        let mut object = object.clone();
        let text = object.get("text").and_then(Value::as_str).unwrap_or("");
        object.insert(
            "__origin".into(),
            Value::String(context.origin_for(text).as_str().into()),
        );
        Value::Object(object)
    }
}

/// Upsert a fact into a memory file.
pub struct MemoryUpsertTool {
    store: Arc<MemoryStore>,
}

impl MemoryUpsertTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        MemoryUpsertTool { store }
    }
}

#[async_trait]
impl Tool for MemoryUpsertTool {
    fn name(&self) -> &str { "memory_upsert" }

    fn description(&self) -> &str { "Upsert a fact into memory, avoiding duplicates." }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file": { "type": "string", "description": "Memory file name. Defaults to MEMORY.md." },
                "name": { "type": "string", "description": "Memory file name." },
                "text": { "type": "string", "description": "Fact to upsert." },
                "content": { "type": "string", "description": "Fact to upsert." },
                "fact": { "type": "string", "description": "Fact to upsert." },
                "value": { "type": "string", "description": "Value to upsert." },
                "key": { "type": "string", "description": "Optional key." }
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let name = args.get("file").or_else(|| args.get("name")).or_else(|| args.get("target")).and_then(Value::as_str).unwrap_or("MEMORY.md");
        let mut text = args.get("text").or_else(|| args.get("content")).or_else(|| args.get("fact")).or_else(|| args.get("value")).and_then(Value::as_str).unwrap_or("").to_string();
        if let Some(key) = args.get("key").and_then(Value::as_str) {
            let val = args.get("value").and_then(Value::as_str).unwrap_or("");
            text = format!("{key}: {val}");
        }
        if text.trim().is_empty() {
            return Err(ToolError::InvalidArgs { tool: self.name().into(), reason: "missing text".into() });
        }
        let origin = args.get("__origin").and_then(Value::as_str).map(|o| match o { "owner"=>Origin::Owner, "agent"=>Origin::Agent, "untrusted"=>Origin::Untrusted, "system"=>Origin::System, _=>Origin::Agent }).unwrap_or(Origin::Agent);
        let msg = self.store.upsert_fact(name, &text, origin).map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
        Ok(ToolOutput::ok(msg))
    }
}

/// Import memory from JSON or text.
pub struct MemoryImportTool {
    store: Arc<MemoryStore>,
}

impl MemoryImportTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        MemoryImportTool { store }
    }
}

#[async_trait]
impl Tool for MemoryImportTool {
    fn name(&self) -> &str { "memory_import" }

    fn description(&self) -> &str { "Import memory from JSON or plain text." }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "Data to import." },
                "data": { "type": "string", "description": "Data to import." },
                "json": { "type": "string", "description": "Data to import." },
                "text": { "type": "string", "description": "Data to import." }
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let data = args.get("content").or_else(|| args.get("data")).or_else(|| args.get("json")).or_else(|| args.get("text")).and_then(Value::as_str).unwrap_or("");
        if data.trim().is_empty() {
            return Err(ToolError::InvalidArgs { tool: self.name().into(), reason: "missing content".into() });
        }
        let msg = self.store.import_memory(data, Origin::Agent).map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;
        Ok(ToolOutput::ok(msg))
    }
}

/// Export memory as JSON.
pub struct MemoryExportTool {
    store: Arc<MemoryStore>,
}

impl MemoryExportTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        MemoryExportTool { store }
    }
}

#[async_trait]
impl Tool for MemoryExportTool {
    fn name(&self) -> &str { "memory_export" }

    fn description(&self) -> &str { "Export all memory as JSON." }

    fn parameters(&self) -> Value { json!({ "type":"object", "properties":{} }) }

    async fn run(&self, _args: Value) -> Result<ToolOutput> {
        Ok(ToolOutput::ok(self.store.export_memory()))
    }
}

// Aliases
pub type MemoryUpsert = MemoryUpsertTool;
pub type MemoryImport = MemoryImportTool;
pub type MemoryExport = MemoryExportTool;
