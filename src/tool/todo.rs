//! The list of things to do.
//!
//! Work that takes more than a step needs somewhere to keep the steps. The
//! model writes the list down, ticks items off as it goes, and the user can
//! see what the assistant thinks it is doing — and what it has not got to
//! yet.
//!
//! The list belongs to the conversation it was made in, so two chats do not
//! share one, and it is kept beside the sessions, so a plan survives a
//! restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Result, Tool, ToolContext, ToolError, ToolOutput};

/// How far along a thing is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Not started.
    #[serde(alias = "not_started", alias = "todo")]
    Pending,
    /// Being worked on now.
    #[serde(alias = "in-progress", alias = "in progress", alias = "active")]
    InProgress,
    /// Finished.
    #[serde(alias = "completed", alias = "complete")]
    Done,
}

impl TodoStatus {
    /// How the status is shown.
    pub fn mark(&self) -> &'static str {
        match self {
            TodoStatus::Pending => " ",
            TodoStatus::InProgress => ">",
            TodoStatus::Done => "+",
        }
    }
}

/// One thing to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    /// What the thing is.
    pub content: String,
    /// How far along it is.
    pub status: TodoStatus,
}

impl TodoItem {
    /// A thing to do, not yet started.
    pub fn new(content: impl Into<String>) -> TodoItem {
        TodoItem {
            content: content.into(),
            status: TodoStatus::Pending,
        }
    }
}

/// The list of things to do.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoList {
    /// The things, in the order they were listed.
    #[serde(default)]
    pub items: Vec<TodoItem>,
}

impl TodoList {
    /// A list of things to do.
    pub fn new(items: Vec<TodoItem>) -> TodoList {
        TodoList { items }
    }

    /// How many are left to do.
    pub fn remaining(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status != TodoStatus::Done)
            .count()
    }

    /// The list as text, for the model and for the user.
    pub fn render(&self) -> String {
        if self.items.is_empty() {
            return "the list is empty.".to_string();
        }
        let mut out = String::new();
        for (index, item) in self.items.iter().enumerate() {
            out.push_str(&format!(
                "{} [{}] {}\n",
                index + 1,
                item.status.mark(),
                item.content
            ));
        }
        out.trim_end().to_string()
    }
}

/// The lists, one per conversation.
///
/// Kept in memory, and written to a file so a plan outlives the process.
#[derive(Debug, Default)]
pub struct TodoStore {
    /// Where the lists are written; `None` keeps them in memory only.
    path: Option<PathBuf>,
    /// The lists, by conversation.
    lists: HashMap<String, TodoList>,
}

impl TodoStore {
    /// A store that forgets everything when it is dropped.
    pub fn in_memory() -> TodoStore {
        TodoStore::default()
    }

    /// A store that keeps the lists in `dir`, in one file.
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<TodoStore> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let path = dir.join("todos.json");

        let mut lists = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            // A file that cannot be read is not worth failing over: the
            // lists are a convenience, not a ledger.
            if let Ok(parsed) = serde_json::from_str::<HashMap<String, TodoList>>(&text) {
                lists = parsed;
            }
        }

        Ok(TodoStore {
            path: Some(path),
            lists,
        })
    }

    /// The list of a conversation.
    pub fn list(&self, session_id: &str) -> TodoList {
        self.lists.get(session_id).cloned().unwrap_or_default()
    }

    /// Replace the list of a conversation.
    pub fn set(&mut self, session_id: &str, list: TodoList) -> std::io::Result<()> {
        self.lists.insert(session_id.to_string(), list);
        self.save()
    }

    /// Empty the list of a conversation.
    pub fn clear(&mut self, session_id: &str) -> std::io::Result<()> {
        self.lists.remove(session_id);
        self.save()
    }

    /// Write the lists out.
    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text = serde_json::to_string_pretty(&self.lists).unwrap_or_else(|_| "{}".into());
        // Write to one side and move it into place, so an interrupted
        // write cannot leave a half-written file behind.
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)
    }
}

/// The tool the model writes the list with.
pub struct TodoTool {
    store: Arc<Mutex<TodoStore>>,
}

impl TodoTool {
    /// A tool whose lists are forgotten when the process ends.
    pub fn new() -> TodoTool {
        TodoTool {
            store: Arc::new(Mutex::new(TodoStore::in_memory())),
        }
    }

    /// A tool that keeps the lists in `dir`.
    pub fn with_dir(dir: impl AsRef<Path>) -> std::io::Result<TodoTool> {
        Ok(TodoTool {
            store: Arc::new(Mutex::new(TodoStore::open(dir)?)),
        })
    }

    /// The list of a conversation.
    pub fn list(&self, session_id: &str) -> TodoList {
        match self.store.lock() {
            Ok(store) => store.list(session_id),
            Err(poisoned) => poisoned.into_inner().list(session_id),
        }
    }
}

impl Default for TodoTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str {
        "todo"
    }

    fn description(&self) -> &str {
        "Keep the list of things to do for the current work. Use it when a \
         task takes more than one step: write the steps down with action \
         \"write\", then mark each one as you go — status \"pending\", \
         \"in_progress\" or \"done\". The list replaces the previous one, so \
         send the whole list every time. Action \"read\" shows the list, \
         action \"clear\" empties it. One item should be \"in_progress\" at a \
         time."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["write", "read", "clear"],
                    "description": "What to do with the list."
                },
                "todos": {
                    "type": "array",
                    "description": "The whole list, in order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {
                                "type": "string",
                                "description": "What the step is."
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "done"],
                                "description": "How far along the step is."
                            }
                        },
                        "required": ["content", "status"]
                    }
                }
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("write");

        let session = args
            .get("__session")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        match action {
            "read" => {
                let list = self.list(&session);
                Ok(ToolOutput::ok(list.render()).with_metadata(todo_metadata(&list)))
            }
            "clear" => {
                self.write(|store| store.clear(&session))?;
                let list = TodoList::default();
                Ok(ToolOutput::ok("the list is empty.").with_metadata(todo_metadata(&list)))
            }
            _ => {
                let items = args.get("todos").and_then(Value::as_array).ok_or_else(|| {
                    ToolError::InvalidArgs {
                        tool: self.name().into(),
                        reason: "the list of steps is missing".into(),
                    }
                })?;

                let mut parsed = Vec::with_capacity(items.len());
                for item in items {
                    let content = item.get("content").and_then(Value::as_str).ok_or_else(|| {
                        ToolError::InvalidArgs {
                            tool: self.name().into(),
                            reason: "each step needs something to do".into(),
                        }
                    })?;
                    let content = content.trim();
                    if content.is_empty() {
                        continue;
                    }
                    let status = match item.get("status").and_then(Value::as_str) {
                        Some("in_progress") | Some("in-progress") | Some("active") => {
                            TodoStatus::InProgress
                        }
                        Some("done") | Some("completed") | Some("complete") => TodoStatus::Done,
                        _ => TodoStatus::Pending,
                    };
                    parsed.push(TodoItem {
                        content: content.to_string(),
                        status,
                    });
                }

                let list = TodoList::new(parsed);
                self.write(|store| store.set(&session, list.clone()))?;

                Ok(ToolOutput::ok(list.render()).with_metadata(todo_metadata(&list)))
            }
        }
    }

    /// The session comes from the turn, not from the model.
    fn with_context(&self, mut args: Value, context: &ToolContext) -> Value {
        let Some(object) = args.as_object_mut() else {
            return args;
        };
        object.insert(
            "__session".into(),
            Value::String(context.session_id.clone()),
        );
        args
    }
}

impl TodoTool {
    /// Change the list, keeping the store's own errors out of the way: a
    /// list that could not be written is still a list for this turn.
    fn write(&self, change: impl FnOnce(&mut TodoStore) -> std::io::Result<()>) -> Result<()> {
        let mut store = match self.store.lock() {
            Ok(store) => store,
            Err(poisoned) => poisoned.into_inner(),
        };
        change(&mut store).map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))
    }
}

/// The list, as metadata, so the interface can show the plan.
fn todo_metadata(list: &TodoList) -> Value {
    json!({
        "todos": list.items,
        "remaining": list.remaining(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_is_shown_with_its_marks() {
        let list = TodoList::new(vec![
            TodoItem {
                content: "read the file".into(),
                status: TodoStatus::Done,
            },
            TodoItem {
                content: "fix the bug".into(),
                status: TodoStatus::InProgress,
            },
            TodoItem::new("run the tests"),
        ]);

        let rendered = list.render();
        assert!(rendered.contains("1 [+] read the file"));
        assert!(rendered.contains("2 [>] fix the bug"));
        assert!(rendered.contains("3 [ ] run the tests"));
        assert_eq!(list.remaining(), 2);
    }

    #[test]
    fn an_empty_list_says_so() {
        assert_eq!(TodoList::default().render(), "the list is empty.");
    }

    #[test]
    fn lists_are_kept_apart() {
        let mut store = TodoStore::in_memory();
        store
            .set("chat:a", TodoList::new(vec![TodoItem::new("one")]))
            .unwrap();
        store
            .set("chat:b", TodoList::new(vec![TodoItem::new("two")]))
            .unwrap();

        assert_eq!(store.list("chat:a").items[0].content, "one");
        assert_eq!(store.list("chat:b").items[0].content, "two");
        assert!(store.list("chat:c").items.is_empty());
    }

    #[test]
    fn lists_are_written_out_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store = TodoStore::open(dir.path()).unwrap();
            store
                .set("chat:a", TodoList::new(vec![TodoItem::new("keep me")]))
                .unwrap();
        }

        let store = TodoStore::open(dir.path()).unwrap();
        assert_eq!(store.list("chat:a").items[0].content, "keep me");
    }
}
