//! Scheduled work: the things the assistant promises to do later.
//!
//! "Check the build every half hour" is a request that cannot be
//! answered now — it is a promise about later. A job is a prompt and a
//! time: when the moment comes, the job runs a turn of its own, and the
//! answer goes back to wherever the request came from.
//!
//! The jobs live in the state directory, so they survive a restart: a
//! promise made on Monday is still there on Tuesday.

pub mod cron;
pub mod when;
pub mod zoned;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};

use crate::agent::{AgentEvent, SessionRunner};
use crate::channel::{APPROVAL_TIMEOUT, Approvers, Channel, OutboundMessage};

pub use cron::{CronSpec, parse_cron};
pub use when::{When, parse_when};
pub use zoned::ZonedSchedule;

/// How often the scheduler looks for work.
const TICK: std::time::Duration = std::time::Duration::from_secs(30);

/// Where a job's answer goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// Back to the conversation that asked for it.
    Chat {
        /// Which platform.
        channel: String,
        /// Which conversation on that platform.
        conversation: String,
    },
    /// Nowhere in particular: the answer stays in the job's own session.
    Session,
}

impl Target {
    /// The target for a session, if the session belongs to a chat.
    ///
    /// A session id is `channel:conversation`, so a job asked for in a
    /// Telegram chat goes back to that chat. A session of the terminal
    /// interface has no chat to go back to.
    pub fn for_session(session_id: &str) -> Target {
        match session_id.split_once(':') {
            Some((channel, conversation)) if !channel.is_empty() && !conversation.is_empty() => {
                Target::Chat {
                    channel: channel.to_string(),
                    conversation: conversation.to_string(),
                }
            }
            _ => Target::Session,
        }
    }
}

/// A promise to do something later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// A short id, so the job can be named and removed.
    pub id: String,
    /// What to do when the time comes.
    pub prompt: String,
    /// When it comes round.
    pub when: When,
    /// Where the answer goes.
    pub target: Target,
    /// The session the job runs in, so it remembers what it did last time.
    pub session_id: String,
    /// When the job was made.
    pub created_at: DateTime<Utc>,
    /// When it last ran.
    pub last_run: Option<DateTime<Utc>>,
    /// When it runs next.
    pub next_run: DateTime<Utc>,
    /// What it said last time.
    pub last_result: Option<String>,
    /// Whether the job is still wanted.
    pub enabled: bool,
}

impl Job {
    /// A new job, ready to be scheduled.
    pub fn new(
        id: impl Into<String>,
        prompt: impl Into<String>,
        when: When,
        target: Target,
        now: DateTime<Utc>,
    ) -> Job {
        let next_run = when.next_after(now).unwrap_or(now);
        let id = id.into();
        Job {
            session_id: format!("schedule:{id}"),
            id,
            prompt: prompt.into(),
            when,
            target,
            created_at: now,
            last_run: None,
            next_run,
            last_result: None,
            enabled: true,
        }
    }

    /// A line describing the job, for the model and for the user.
    pub fn describe(&self) -> String {
        let mut line = format!("{} — {} ({})", self.id, self.prompt, self.when.describe());
        if let Some(result) = &self.last_result {
            line.push_str(&format!("\n  last: {}", single_line(result)));
        }
        line
    }
}

/// A job that has run, and what it said.
#[derive(Debug, Clone)]
pub struct Delivery {
    /// Which job.
    pub job_id: String,
    /// What it was asked to do.
    pub prompt: String,
    /// What it said.
    pub answer: String,
}

/// The jobs, kept on disk.
pub struct JobStore {
    /// Where the file is; `None` for a store that is not written down.
    path: Option<PathBuf>,
    /// The jobs themselves.
    jobs: Vec<Job>,
}

impl JobStore {
    /// A store that keeps its jobs in memory.
    pub fn in_memory() -> JobStore {
        JobStore {
            path: None,
            jobs: Vec::new(),
        }
    }

    /// A store that keeps its jobs in a file in `dir`.
    ///
    /// The file is read if it is there; a store that cannot be read is an
    /// error, because a promise that is quietly forgotten is worse than a
    /// failure to start.
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<JobStore> {
        let dir = dir.as_ref();
        let path = dir.join("jobs.json");
        let mut store = JobStore {
            path: Some(path.clone()),
            jobs: Vec::new(),
        };
        if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            store.jobs = serde_json::from_str(&text).unwrap_or_default();
        }
        Ok(store)
    }

    /// Every job, in the order they were made.
    pub fn list(&self) -> &[Job] {
        &self.jobs
    }

    /// The job with this id.
    pub fn get(&self, id: &str) -> Option<&Job> {
        self.jobs.iter().find(|job| job.id == id)
    }

    /// Add a job.
    pub fn add(&mut self, job: Job) {
        self.jobs.retain(|existing| existing.id != job.id);
        self.jobs.push(job);
    }

    /// Remove a job, saying whether there was one.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.jobs.len();
        self.jobs.retain(|job| job.id != id);
        self.jobs.len() != before
    }

    /// Change a job, saying whether there was one to change.
    pub fn update(&mut self, id: &str, change: impl FnOnce(&mut Job)) -> bool {
        match self.jobs.iter_mut().find(|job| job.id == id) {
            Some(job) => {
                change(job);
                true
            }
            None => false,
        }
    }

    /// The jobs that are due at this moment.
    pub fn due(&self, now: DateTime<Utc>) -> Vec<&Job> {
        self.jobs
            .iter()
            .filter(|job| job.enabled && job.next_run <= now)
            .collect()
    }

    /// Write the jobs down.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&self.jobs)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        // The file is written and then moved into place, so an interrupted
        // write cannot leave a half-written list of promises behind.
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    }
}

/// Runs the jobs when their time comes.
pub struct Scheduler {
    /// The jobs.
    store: Arc<Mutex<JobStore>>,
    /// The agent the jobs run on.
    runner: Arc<SessionRunner>,
    /// The platforms answers can be sent through, by name.
    channels: Arc<std::sync::Mutex<HashMap<String, Arc<dyn Channel>>>>,
    /// The permission requests of the conversations, shared with the
    /// channels so a job asks in the same place the chat does.
    approvers: Approvers,
    /// Where answers go that have no chat to go to.
    local: Arc<std::sync::Mutex<Option<mpsc::Sender<Delivery>>>>,
}

impl Scheduler {
    /// A scheduler over a store of jobs.
    pub fn new(store: JobStore, runner: Arc<SessionRunner>) -> Scheduler {
        Scheduler::with_approvers(store, runner, Approvers::new())
    }

    /// A scheduler that shares its permission requests with the channels.
    pub fn with_approvers(
        store: JobStore,
        runner: Arc<SessionRunner>,
        approvers: Approvers,
    ) -> Scheduler {
        Scheduler {
            store: Arc::new(Mutex::new(store)),
            runner,
            channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            approvers,
            local: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// A scheduler whose jobs are not written down.
    pub fn in_memory(runner: Arc<SessionRunner>) -> Scheduler {
        Scheduler::new(JobStore::in_memory(), runner)
    }

    /// Let answers be sent through this platform.
    pub fn add_channel(&self, channel: Arc<dyn Channel>) {
        let mut channels = self.channels.lock().unwrap();
        channels.insert(channel.name().to_string(), channel);
    }

    /// The registered platform with this name.
    pub fn channel(&self, name: &str) -> Option<Arc<dyn Channel>> {
        self.channels.lock().unwrap().get(name).cloned()
    }

    /// Send the answers that have nowhere else to go here.
    pub fn set_local_sink(&self, tx: mpsc::Sender<Delivery>) {
        let mut local = self.local.lock().unwrap();
        *local = Some(tx);
    }

    /// Every job, with the next run of each.
    pub async fn list(&self) -> Vec<Job> {
        let store = self.store.lock().await;
        store.list().to_vec()
    }

    /// The job with this id.
    pub async fn get(&self, id: &str) -> Option<Job> {
        let store = self.store.lock().await;
        store.get(id).cloned()
    }

    /// Make a job.
    ///
    /// The `when` is in words — "every 30 minutes", "at 5pm" — and is
    /// turned into a schedule here. A job with a target of
    /// [`Target::Chat`] answers in the chat that asked for it.
    pub async fn create(
        &self,
        prompt: impl Into<String>,
        when: &str,
        target: Target,
        now: DateTime<Utc>,
    ) -> Result<Job, String> {
        let prompt = prompt.into();
        if prompt.trim().is_empty() {
            return Err("say what should be done".into());
        }
        let schedule = parse_when(when, now)?;
        let id = short_id();
        let job = Job::new(id, prompt, schedule, target, now);

        let mut store = self.store.lock().await;
        store.add(job.clone());
        if let Err(e) = store.save() {
            tracing::warn!("could not save the job '{}': {e}", job.id);
        }
        Ok(job)
    }

    /// Remove a job, saying whether there was one.
    pub async fn remove(&self, id: &str) -> bool {
        let mut store = self.store.lock().await;
        let removed = store.remove(id);
        if removed && let Err(e) = store.save() {
            tracing::warn!("could not save the jobs: {e}");
        }
        removed
    }

    /// Run the jobs that are due.
    ///
    /// Returns what they said. The turn of a job runs on its own, in the
    /// job's own session, so a job that takes a while does not hold up the
    /// others.
    pub async fn tick(&self, now: DateTime<Utc>) -> Vec<Delivery> {
        // The store is not held while a job runs: a turn can take minutes,
        // and the other jobs should not wait for it.
        let due: Vec<Job> = {
            let store = self.store.lock().await;
            store.due(now).into_iter().cloned().collect()
        };

        let mut deliveries = Vec::new();
        for job in due {
            let answer = self.run_job(&job).await;

            // The next run is worked out from the schedule, and a job that
            // was a single moment is finished.
            let next = job.when.next_after(now);
            {
                let mut store = self.store.lock().await;
                store.update(&job.id, |stored| {
                    stored.last_run = Some(now);
                    stored.next_run = next.unwrap_or(now);
                    stored.enabled = next.is_some();
                    stored.last_result = answer.clone();
                });
                if let Err(e) = store.save() {
                    tracing::warn!("could not save the jobs: {e}");
                }
            }

            let delivery = Delivery {
                job_id: job.id.clone(),
                prompt: job.prompt.clone(),
                answer: answer.clone().unwrap_or_default(),
            };
            self.deliver(&job, &delivery).await;
            deliveries.push(delivery);
        }
        deliveries
    }

    /// Run one job, and return what it said.
    async fn run_job(&self, job: &Job) -> Option<String> {
        // A job that needs permission asks in the chat that scheduled it,
        // so the person who asked for the work can let it happen.
        if let Target::Chat {
            channel,
            conversation,
        } = &job.target
        {
            let adapter = {
                let channels = self.channels.lock().unwrap();
                channels.get(channel).cloned()
            };
            if let Some(adapter) = adapter {
                let approver = self
                    .approvers
                    .get(adapter, conversation, APPROVAL_TIMEOUT)
                    .await;
                self.runner.set_approver(&job.session_id, approver).await;
            }
        }

        let (tx, mut rx) = mpsc::channel(256);
        let runner = self.runner.clone();
        let session = job.session_id.clone();
        let prompt = job.prompt.clone();

        let turn = tokio::spawn(async move { runner.run(&session, Some(prompt), tx).await });

        // The events are drained as they arrive, and the text is collected
        // as it streams — a turn that says a great deal would otherwise
        // fill the queue and stall.
        let mut answer = String::new();
        while let Some(event) = rx.recv().await {
            if let AgentEvent::TextDelta(text) = &event {
                answer.push_str(text);
            }
        }
        let _ = turn.await;

        if answer.trim().is_empty() {
            None
        } else {
            Some(answer)
        }
    }

    /// Send a job's answer where it belongs.
    async fn deliver(&self, job: &Job, delivery: &Delivery) {
        match &job.target {
            Target::Chat {
                channel,
                conversation,
            } => {
                let adapter = {
                    let channels = self.channels.lock().unwrap();
                    channels.get(channel).cloned()
                };
                match adapter {
                    Some(adapter) => {
                        let message = OutboundMessage::new(conversation.clone(), &delivery.answer);
                        if let Err(e) = adapter.send(&message).await {
                            tracing::warn!("could not deliver the job '{}': {e}", job.id);
                        }
                    }
                    None => self.deliver_locally(delivery).await,
                }
            }
            Target::Session => self.deliver_locally(delivery).await,
        }
    }

    /// Hand an answer to whoever is watching.
    async fn deliver_locally(&self, delivery: &Delivery) {
        let sink = {
            let local = self.local.lock().unwrap();
            local.clone()
        };
        if let Some(sink) = sink {
            let _ = sink.send(delivery.clone()).await;
        } else {
            tracing::info!(
                "job '{}' finished: {}",
                delivery.job_id,
                single_line(&delivery.answer)
            );
        }
    }

    /// Watch the clock, and run the jobs as they come round.
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(TICK);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let now = Utc::now();
                self.tick(now).await;
            }
        })
    }
}

/// A short id, so a job can be named without being quoted at length.
fn short_id() -> String {
    uuid::Uuid::new_v4().to_string()[..8].to_string()
}

/// A job's answer on one line, for a log.
fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_session_gives_its_chat_back() {
        let target = Target::for_session("telegram:12345");
        assert_eq!(
            target,
            Target::Chat {
                channel: "telegram".into(),
                conversation: "12345".into()
            }
        );
    }

    #[test]
    fn a_session_with_no_chat_has_nowhere_to_go() {
        assert_eq!(Target::for_session("tui-abc"), Target::Session);
    }

    #[test]
    fn jobs_are_kept_in_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = JobStore::open(dir.path()).unwrap();
        let job = Job::new(
            "abc",
            "check the build",
            When::Every { seconds: 600 },
            Target::Session,
            Utc::now(),
        );
        store.add(job.clone());
        store.save().unwrap();

        let reread = JobStore::open(dir.path()).unwrap();
        assert_eq!(reread.list().len(), 1);
        assert_eq!(reread.get("abc").unwrap().prompt, "check the build");
    }

    #[test]
    fn a_removed_job_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = JobStore::open(dir.path()).unwrap();
        store.add(Job::new(
            "abc",
            "check the build",
            When::Every { seconds: 600 },
            Target::Session,
            Utc::now(),
        ));
        assert!(store.remove("abc"));
        assert!(!store.remove("abc"));
        assert!(store.list().is_empty());
    }
}
