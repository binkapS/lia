//! Tests of the work the assistant promises to do later.
//!
//! A job is a prompt and a time. When the time comes the job runs a turn
//! of its own, and the answer goes back to the conversation that asked for
//! it.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lia::agent::session::SessionStore;
use lia::agent::{Agent, AgentConfig, SessionRunner};
use lia::channel::{Approvers, ChannelDispatcher, InboundMessage};
use lia::schedule::{JobStore, Scheduler, Target};
use lia::tool::schedule::{ScheduleTool, SchedulerSlot};
use lia::tool::{Tool, ToolContext, ToolRegistry};

/// Everything a scheduled job needs: the scheduler, the channel it answers
/// in, and the tool the assistant uses to make jobs.
struct Bench {
    scheduler: Arc<Scheduler>,
    runner: Arc<SessionRunner>,
    channel: Arc<common::FakeChannel>,
    tool: ScheduleTool,
    approvers: Approvers,
    calls: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Bench {
    /// A bench whose assistant says the same thing every time.
    fn new() -> Bench {
        Bench::with_provider(Arc::new(common::MockProvider::answering(
            "the build is fine",
        )))
    }

    /// A bench whose assistant asks for a tool before it answers.
    fn with_provider(provider: Arc<common::MockProvider>) -> Bench {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let tool = Arc::new(common::RecordingTool {
            name: "do_thing",
            calls: calls.clone(),
            requires_approval: true,
        });

        let mut registry = ToolRegistry::new();
        registry.register(tool);

        let agent = Agent::new(provider, Arc::new(registry), AgentConfig::default());
        let runner = SessionRunner::new(
            Arc::new(agent),
            Arc::new(tokio::sync::Mutex::new(SessionStore::in_memory())),
        );

        let channel = Arc::new(common::FakeChannel::new());
        let approvers = Approvers::new();
        let scheduler = Arc::new(Scheduler::with_approvers(
            JobStore::in_memory(),
            runner.clone(),
            approvers.clone(),
        ));
        // The answer goes back through the channel the job was made in.
        scheduler.add_channel(channel.clone());

        let slot = SchedulerSlot::new();
        slot.set(scheduler.clone());
        let tool = ScheduleTool::new(slot);

        Bench {
            scheduler,
            runner,
            channel,
            tool,
            approvers,
            calls,
        }
    }

    /// The messages the assistant sent to the chat.
    fn sent(&self) -> Vec<String> {
        self.channel
            .sent
            .lock()
            .unwrap()
            .iter()
            .map(|m| m.text.clone())
            .collect()
    }
}

/// Ask the assistant's tool to make a job, as if it had been asked in a chat.
async fn make_job(bench: &Bench, prompt: &str, when: &str) -> String {
    let args = serde_json::json!({
        "action": "create",
        "prompt": prompt,
        "when": when,
        "__session": "fake:chat1",
    });
    let context = ToolContext {
        session_id: "fake:chat1".into(),
        ..Default::default()
    };
    let args = bench.tool.with_context(args, &context);
    let output = bench.tool.run(args).await.unwrap();
    assert!(!output.is_error, "the job was refused: {}", output.content);

    // The tool says which job it made.
    output
        .content
        .rsplit(' ')
        .next()
        .unwrap()
        .trim_matches(|c| c == '(' || c == ')')
        .to_string()
}

/// A moment later than any job made now.
fn later() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + chrono::Duration::hours(2)
}

#[tokio::test]
async fn a_job_is_made_and_kept() {
    let bench = Bench::new();

    let id = make_job(&bench, "check the build", "every 30 minutes").await;

    let jobs = bench.scheduler.list().await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, id);
    assert_eq!(jobs[0].prompt, "check the build");
    assert_eq!(
        jobs[0].target,
        Target::Chat {
            channel: "fake".into(),
            conversation: "chat1".into()
        }
    );
}

#[tokio::test]
async fn a_job_runs_when_its_time_comes_and_answers_in_the_chat() {
    let bench = Bench::new();
    make_job(&bench, "check the build", "every 30 minutes").await;

    // Nothing is due yet.
    let early = bench.scheduler.tick(chrono::Utc::now()).await;
    assert!(early.is_empty());

    // The time comes.
    let deliveries = bench.scheduler.tick(later()).await;
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].answer, "the build is fine");

    // The answer went to the chat that asked for the work.
    let sent = bench.sent();
    assert!(
        sent.iter().any(|text| text.contains("the build is fine")),
        "the chat was not told: {sent:?}"
    );
    let message = bench.channel.sent.lock().unwrap().last().cloned().unwrap();
    assert_eq!(message.conversation, "chat1");
}

#[tokio::test]
async fn a_job_runs_again_when_it_comes_round_again() {
    let bench = Bench::new();
    make_job(&bench, "check the build", "every 30 minutes").await;

    bench.scheduler.tick(later()).await;
    let job = bench.scheduler.list().await.remove(0);
    assert!(job.last_run.is_some(), "the job did not record a run");
    assert!(job.next_run > later(), "the job is not due again");

    // Half an hour after that, it runs once more.
    let next = job.next_run + chrono::Duration::minutes(1);
    let deliveries = bench.scheduler.tick(next).await;
    assert_eq!(deliveries.len(), 1);
}

#[tokio::test]
async fn a_job_can_be_cancelled() {
    let bench = Bench::new();
    let id = make_job(&bench, "check the build", "every 30 minutes").await;

    let args = serde_json::json!({ "action": "remove", "id": id });
    let output = bench.tool.run(args).await.unwrap();
    assert!(
        !output.is_error,
        "the job was not cancelled: {}",
        output.content
    );

    assert!(bench.scheduler.list().await.is_empty());
    assert!(!bench.scheduler.remove(&id).await);
}

#[tokio::test]
async fn a_job_needs_permission_and_asks_in_the_chat() {
    // The assistant wants to run a tool that needs permission.
    let provider = Arc::new(common::MockProvider::with_tool(
        "do_thing",
        serde_json::json!({ "x": 1 }),
    ));
    let bench = Bench::with_provider(provider);
    make_job(&bench, "do the thing", "every 30 minutes").await;

    // The job runs in the background, because it is waiting for an answer.
    let scheduler = bench.scheduler.clone();
    let tick = tokio::spawn(async move { scheduler.tick(later()).await });

    // The question reaches the chat.
    let mut question = String::new();
    for _ in 0..100 {
        question = bench
            .sent()
            .into_iter()
            .find(|text| text.contains("/approve"))
            .unwrap_or_default();
        if !question.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!question.is_empty(), "the chat was never asked");

    // The person who asked for the work answers.
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(16);
    let dispatcher = ChannelDispatcher::with_approvers(
        bench.runner.clone(),
        event_tx,
        bench.channel.clone(),
        bench.approvers.clone(),
    );
    let reply = dispatcher
        .handle_command(&InboundMessage::new("fake", "alice", "/approve 1", "chat1"))
        .await;
    assert_eq!(reply.as_deref(), Some("Done."));

    // The tool ran, and the job finished.
    let deliveries = tick.await.unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(bench.calls.lock().unwrap().len(), 1, "the tool did not run");
}

#[tokio::test]
async fn the_assistant_cannot_promise_anything_without_a_scheduler() {
    let tool = ScheduleTool::new(SchedulerSlot::new());
    let args = serde_json::json!({
        "action": "create",
        "prompt": "check the build",
        "when": "every 30 minutes",
    });
    let result = tool.run(args).await;
    assert!(result.is_err(), "the job was made with no scheduler");
}

#[tokio::test]
async fn a_job_can_be_made_with_a_cron_expression() {
    let bench = Bench::new();

    make_job(&bench, "check the build", "0 9 * * 1-5").await;

    let jobs = bench.scheduler.list().await;
    assert_eq!(jobs.len(), 1);
    assert!(
        matches!(jobs[0].when, lia::schedule::When::Cron { .. }),
        "the job is not a cron schedule: {:?}",
        jobs[0].when
    );
    assert_eq!(jobs[0].when.describe(), "at 09:00 on weekdays");
}

#[tokio::test]
async fn a_cron_job_runs_at_the_time_it_names() {
    let bench = Bench::new();

    // Every minute, so the job is due whatever the clock says.
    make_job(&bench, "check the build", "* * * * *").await;

    let job = bench.scheduler.list().await.remove(0);
    let now = chrono::Utc::now();
    let first_run = job.next_run;
    assert!(
        first_run > now && first_run <= now + chrono::Duration::minutes(1),
        "the job is not due within the minute: {}",
        first_run
    );

    let deliveries = bench.scheduler.tick(first_run).await;
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].answer, "the build is fine");

    // And it comes round again.
    let job = bench.scheduler.list().await.remove(0);
    assert!(job.last_run.is_some(), "the job did not record a run");
    assert!(job.next_run > first_run, "the job is not due again");
}

#[tokio::test]
async fn a_cron_expression_that_is_wrong_is_refused() {
    let bench = Bench::new();

    let args = serde_json::json!({
        "action": "create",
        "prompt": "check the build",
        "when": "0 99 * * *",
        "__session": "fake:chat1",
    });
    let context = ToolContext {
        session_id: "fake:chat1".into(),
        ..Default::default()
    };
    let args = bench.tool.with_context(args, &context);
    let output = bench.tool.run(args).await.unwrap();

    assert!(output.is_error, "the bad expression was accepted");
    assert!(bench.scheduler.list().await.is_empty());
}
