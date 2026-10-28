//! Hand a piece of work to another assistant.
//!
//! Some work does not belong in the conversation: it is long, it is
//! mechanical, or it is a distraction from what the user actually asked.
//! The model can hand such a task to a fresh assistant with a clear brief,
//! and get back the answer.
//!
//! The other assistant works in a conversation of its own, with the same
//! tools and the same rules. Its risky tools are answered by the same
//! person who is talking to the main one, so permission means the same
//! thing there as it does here. It cannot hand work on again: the tool is
//! not offered to it, so the work cannot nest for ever.

use std::sync::{Arc, RwLock, Weak};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Result, Tool, ToolError, ToolOutput};

/// The prefix of the session a sub-assistant works in.
///
/// A session with this prefix is the sub-assistant's own, and the task
/// tool is not offered to it.
pub const SUB_SESSION_PREFIX: &str = "sub:";

/// Something that can run a task in a conversation of its own.
#[async_trait]
pub trait TaskRunner: Send + Sync {
    /// Run the task, and answer with what the assistant said.
    ///
    /// The `label` is a few words saying what the work is, for the
    /// transcript of the conversation that handed it over.
    async fn run_task(&self, session_id: &str, prompt: &str, label: &str) -> Result<String>;
}

/// A handle to a runner that does not keep it alive.
///
/// The tool must not be what keeps the agent alive, so the handle is weak:
/// a runner that has gone is a runner the tool cannot use.
#[derive(Clone)]
pub struct TaskRunnerHandle {
    inner: Weak<dyn TaskRunner>,
}

impl TaskRunnerHandle {
    /// A handle to a runner.
    pub fn new(runner: &Arc<dyn TaskRunner>) -> TaskRunnerHandle {
        TaskRunnerHandle {
            inner: Arc::downgrade(runner),
        }
    }

    /// The runner, if it is still there.
    pub fn get(&self) -> Option<Arc<dyn TaskRunner>> {
        self.inner.upgrade()
    }
}

/// The tool that hands work to another assistant.
pub struct TaskTool {
    /// Where the work is run. Filled in once the runner exists.
    runner: Arc<RwLock<Option<TaskRunnerHandle>>>,
}

impl TaskTool {
    /// A tool that cannot run anything yet.
    pub fn new() -> TaskTool {
        TaskTool {
            runner: Arc::new(RwLock::new(None)),
        }
    }

    /// Tell the tool where to run the work.
    pub fn set_runner(&self, runner: TaskRunnerHandle) {
        if let Ok(mut slot) = self.runner.write() {
            *slot = Some(runner);
        }
    }
}

impl Default for TaskTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Hand a self-contained piece of work to another assistant, and get \
         back what it found. Use it for work that would crowd out the \
         conversation: a search that takes a while, a question that needs \
         reading three pages, a job that is one long chain of steps. Give \
         the brief in full — the other assistant cannot see this \
         conversation, so say everything it needs. It has the same tools \
         and the same rules, and it answers the result, not a report of \
         what it would do."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task, in full. The other assistant sees only this."
                },
                "label": {
                    "type": "string",
                    "description": "A few words saying what the task is, for the transcript."
                }
            },
            "required": ["prompt"]
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let prompt =
            args.get("prompt")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: self.name().into(),
                    reason: "missing required parameter 'prompt'".into(),
                })?;
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(ToolError::InvalidArgs {
                tool: self.name().into(),
                reason: "the task is empty".into(),
            });
        }

        // The session the work belongs to, which the model does not choose.
        let session = args
            .get("__session")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        // The work cannot nest for ever: a sub-assistant is given no way
        // to hand work on.
        if session.starts_with(SUB_SESSION_PREFIX) {
            return Err(ToolError::Failed(
                self.name().into(),
                "another assistant is already working on a task of its own".into(),
            ));
        }

        let handle = self.runner.read().ok().and_then(|slot| slot.clone());
        let Some(runner) = handle.and_then(|handle| handle.get()) else {
            return Err(ToolError::Failed(
                self.name().into(),
                "there is nowhere to run a task of its own".into(),
            ));
        };

        let brief = format!(
            "You are working on one self-contained task, handed to you by \
             another assistant. Do the work with the tools you have, then \
             answer with the result. Your answer is read by the caller, not \
             by a person, so give the result itself rather than a \
             description of what you would do.\n\nThe task:\n{prompt}"
        );

        // A few words saying what the work is, so the transcript can say
        // what the other assistant was up to.
        let label = args
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();

        let answer = runner.run_task(&session, &brief, &label).await?;
        if answer.trim().is_empty() {
            return Err(ToolError::Failed(
                self.name().into(),
                "the other assistant did not answer".into(),
            ));
        }

        Ok(ToolOutput::ok(answer).with_metadata(json!({
            "label": label,
            // The answer came from the model, not from the network, but it
            // came from somewhere else: the turn that used it should know
            // that the words are not the user's.
            "source": "assistant",
        })))
    }

    /// Take the handle to the runner.
    fn set_task_runner(&self, runner: TaskRunnerHandle) {
        self.set_runner(runner);
    }

    /// The session comes from the turn, not from the model.
    fn with_context(&self, mut args: Value, context: &crate::tool::ToolContext) -> Value {
        let Some(object) = args.as_object_mut() else {
            return args;
        };
        object.insert(
            "__session".into(),
            Value::String(context.session_id.clone()),
        );
        args
    }

    /// The tool is not offered to a sub-assistant, so the work cannot nest.
    fn is_enabled(&self, session_id: &str) -> bool {
        !session_id.starts_with(SUB_SESSION_PREFIX)
    }
}
