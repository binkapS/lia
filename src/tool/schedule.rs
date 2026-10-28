//! The tool that lets the assistant promise to do something later.
//!
//! "Check the build every half hour" is not a question the model can
//! answer now. It is a request for the future, and the assistant needs a
//! way to accept it: the [`ScheduleTool`] turns the request into a job,
//! and the scheduler runs the job when its time comes.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Result, Tool, ToolContext, ToolError, ToolOutput};
use crate::schedule::{Job, Scheduler, Target};

/// Where the scheduler lives, once it has been built.
///
/// The tool is registered with the agent before the scheduler exists — the
/// agent is needed to build the scheduler — so the tool is given a place to
/// find it later.
#[derive(Clone, Default)]
pub struct SchedulerSlot {
    inner: Arc<std::sync::RwLock<Option<Arc<Scheduler>>>>,
}

impl SchedulerSlot {
    /// A slot with nothing in it yet.
    pub fn new() -> SchedulerSlot {
        SchedulerSlot::default()
    }

    /// Put the scheduler in the slot.
    pub fn set(&self, scheduler: Arc<Scheduler>) {
        let mut slot = self.inner.write().unwrap();
        *slot = Some(scheduler);
    }

    /// The scheduler, if it has been put there.
    pub fn get(&self) -> Option<Arc<Scheduler>> {
        let slot = self.inner.read().unwrap();
        slot.clone()
    }
}

/// Schedules work for later.
pub struct ScheduleTool {
    /// Where the scheduler is.
    slot: SchedulerSlot,
}

impl ScheduleTool {
    /// A tool that finds the scheduler when it is needed.
    pub fn new(slot: SchedulerSlot) -> ScheduleTool {
        ScheduleTool { slot }
    }
}

#[async_trait]
impl Tool for ScheduleTool {
    fn name(&self) -> &str {
        "schedule"
    }

    fn description(&self) -> &str {
        "Schedule work for later, and see what is scheduled. \
         To make a job: action \"create\", a prompt saying what to do, and a \
         time in words — \"every 30 minutes\", \"at 5pm\", \"daily at 09:00\", \
         \"in 2 hours\", \"every monday at 9am\" — or a cron expression, \
         \"0 9 * * 1-5\". The work runs on its own \
         later, and the answer comes back to this conversation. \
         To see the jobs: action \"list\". To cancel one: action \"remove\" \
         with its id."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "remove"],
                    "description": "What to do with the schedule."
                },
                "prompt": {
                    "type": "string",
                    "description": "What to do when the time comes, in your own words."
                },
                "when": {
                    "type": "string",
                    "description": "When it should run, in words — \"every 30 minutes\", \"at 5pm\", \"daily at 09:00\", \"in 2 hours\" — or as a cron expression, \"0 9 * * 1-5\"."
                },
                "id": {
                    "type": "string",
                    "description": "Which job to remove."
                }
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let Some(scheduler) = self.slot.get() else {
            return Err(ToolError::Failed(
                "schedule".into(),
                "the scheduler is not running, so nothing can be scheduled".into(),
            ));
        };

        let action = args
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("create");

        match action {
            "list" => {
                let jobs = scheduler.list().await;
                if jobs.is_empty() {
                    return Ok(ToolOutput::ok("nothing is scheduled."));
                }
                let lines: Vec<String> = jobs.iter().map(Job::describe).collect();
                Ok(ToolOutput::ok(lines.join("\n")))
            }
            "remove" => {
                let id = args.get("id").and_then(Value::as_str).ok_or_else(|| {
                    ToolError::InvalidArgs {
                        tool: "schedule".into(),
                        reason: "which job? give its id".into(),
                    }
                })?;
                if scheduler.remove(id).await {
                    Ok(ToolOutput::ok(format!("the job '{id}' is cancelled.")))
                } else {
                    Ok(ToolOutput::error(format!("there is no job called '{id}'.")))
                }
            }
            _ => {
                let prompt = args.get("prompt").and_then(Value::as_str).ok_or_else(|| {
                    ToolError::InvalidArgs {
                        tool: "schedule".into(),
                        reason: "what should be done later? give a prompt".into(),
                    }
                })?;
                let when = args.get("when").and_then(Value::as_str).ok_or_else(|| {
                    ToolError::InvalidArgs {
                        tool: "schedule".into(),
                        reason: "when should it run? say it in words".into(),
                    }
                })?;

                // The job answers in the conversation that asked for it.
                let session = args
                    .get("__session")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let target = Target::for_session(session);

                match scheduler
                    .create(prompt, when, target, chrono::Utc::now())
                    .await
                {
                    Ok(job) => Ok(ToolOutput::ok(format!(
                        "scheduled: {} (id {})",
                        job.when.describe(),
                        job.id
                    ))),
                    Err(e) => Ok(ToolOutput::error(e)),
                }
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
