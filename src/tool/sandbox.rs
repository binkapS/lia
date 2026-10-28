//! Running commands in a sandbox.
//!
//! The shell tool runs whatever the model asks for, and the approval policy
//! decides whether it may. A sandbox is the other half of the answer: it
//! makes the command harmless rather than merely permitted, by giving it a
//! world of its own where the rest of the disk can only be read.
//!
//! The sandbox is [bubblewrap](https://github.com/containers/bubblewrap): a
//! small program that sets up a namespace and mounts things into it. It is
//! not always there, and it does not always work — inside a container, or
//! where the kernel says no — so it is asked first, once, and the command
//! runs as it did before if the answer is no. The assistant keeps working
//! either way.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

/// How commands are confined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    /// Whether commands are run in a sandbox, when one can be made.
    pub enabled: bool,
    /// Whether a sandboxed command may reach the network.
    pub network: bool,
    /// Whether everything in the home directory is hidden, apart from the
    /// workspace. This keeps the assistant away from keys and other secrets
    /// that are kept there, but it also hides the tools it is used to
    /// having — a Rust toolchain, for one.
    pub hide_home: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        SandboxConfig {
            enabled: false,
            network: true,
            hide_home: false,
        }
    }
}

/// The sandbox a command is run in.
#[derive(Debug, Clone)]
pub struct Sandbox {
    /// How the sandbox is set up.
    config: SandboxConfig,
    /// The program that sets it up.
    program: PathBuf,
    /// The directory the command may write in.
    workspace: PathBuf,
    /// The answer of the probe, worked out once.
    probe: OnceCell<bool>,
}

impl Sandbox {
    /// A sandbox for the given workspace.
    pub fn new(config: SandboxConfig, workspace: impl Into<PathBuf>) -> Sandbox {
        Sandbox {
            config,
            program: PathBuf::from("bwrap"),
            workspace: workspace.into(),
            probe: OnceCell::new(),
        }
    }

    /// Whether the sandbox is switched on.
    ///
    /// Whether it works here is a separate question, asked when a command
    /// is run.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Use a different program to set the sandbox up.
    ///
    /// This is for the tests, and for a machine where bubblewrap is called
    /// something else.
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Sandbox {
        self.program = program.into();
        self
    }

    /// Whether the sandbox can be used here.
    ///
    /// The question is asked once, by trying the sandbox out on a harmless
    /// command. A machine that cannot do it says so, and the commands run
    /// as they did before.
    pub async fn available(&self) -> bool {
        if !self.config.enabled {
            return false;
        }
        *self.probe.get_or_init(|| self.probe_now()).await
    }

    /// The command a shell command is run as.
    ///
    /// `None` means the command runs as it is: either the sandbox is off, or
    /// it does not work here.
    pub async fn wrap(&self, command: &str) -> Option<Vec<String>> {
        if !self.available().await {
            return None;
        }
        Some(self.argv(command))
    }

    /// The command line of a sandboxed run: the program, then its options,
    /// then the command itself.
    fn argv(&self, command: &str) -> Vec<String> {
        let mut args: Vec<String> = vec![
            self.program.to_string_lossy().to_string(),
            "--ro-bind".into(),
            "/".into(),
            "/".into(),
            "--dev".into(),
            "/dev".into(),
            "--proc".into(),
            "/proc".into(),
            "--tmpfs".into(),
            "/tmp".into(),
        ];

        // The home directory is hidden first, and the workspace after it:
        // what is mounted last is what is seen, so a workspace that lives
        // in the home directory stays reachable.
        if self.config.hide_home
            && let Some(home) = home_directory()
        {
            args.push("--tmpfs".into());
            args.push(home.to_string_lossy().to_string());
        }

        // The workspace is the one place the command may write.
        if self.workspace.is_absolute() && self.workspace.is_dir() {
            args.push("--bind".into());
            args.push(self.workspace.to_string_lossy().to_string());
            args.push(self.workspace.to_string_lossy().to_string());
        }

        // A command of its own: its own processes, its own terminal
        // settings, and — unless it is allowed out — its own network.
        args.push("--unshare-pid".into());
        args.push("--unshare-ipc".into());
        args.push("--unshare-uts".into());
        if !self.config.network {
            args.push("--unshare-net".into());
        }
        args.push("--die-with-parent".into());

        args.push("bash".into());
        args.push("-lc".into());
        args.push(command.to_string());
        args
    }

    /// The arguments the probe runs.
    fn probe_argv(&self) -> Vec<String> {
        let mut args = vec![
            "--ro-bind".to_string(),
            "/".to_string(),
            "/".to_string(),
            "--dev".to_string(),
            "/dev".to_string(),
            "--proc".to_string(),
            "/proc".to_string(),
            "--unshare-pid".to_string(),
            "--unshare-ipc".to_string(),
            "--unshare-uts".to_string(),
            "--die-with-parent".to_string(),
        ];
        if !self.config.network {
            args.push("--unshare-net".to_string());
        }
        args.push("true".to_string());
        args
    }

    /// Try the sandbox out.
    ///
    /// The question is asked a few times before it is believed. Some
    /// failures are momentary — the program is busy, perhaps because
    /// something is writing to it at the time — and a sandbox that is
    /// given up on because of one bad moment leaves every later command
    /// running free. So a failure that might pass is tried again, and only
    /// a failure that keeps happening is believed.
    async fn probe_now(&self) -> bool {
        for attempt in 1..=PROBE_ATTEMPTS {
            match self.probe_once().await {
                Ok(true) => {
                    tracing::info!("shell commands will run in a sandbox");
                    return true;
                }
                Ok(false) => {
                    // The program ran and said no. That is the answer, and
                    // asking again will not change it.
                    tracing::warn!(
                        "the sandbox is not usable here, so shell commands run as they did before"
                    );
                    return false;
                }
                Err(e) => {
                    if attempt == PROBE_ATTEMPTS {
                        tracing::warn!(
                            "the sandbox could not be started ({e}), so shell commands run as they did before"
                        );
                        return false;
                    }
                    tracing::debug!("the sandbox could not be started ({e}); trying again");
                    tokio::time::sleep(PROBE_PAUSE).await;
                }
            }
        }
        false
    }

    /// Run the probe once.
    ///
    /// `Ok(true)` means the sandbox works, `Ok(false)` means it does not,
    /// and an error means the program could not be run at all.
    async fn probe_once(&self) -> std::io::Result<bool> {
        let mut command = tokio::process::Command::new(&self.program);
        command.args(self.probe_argv());
        command.stdin(std::process::Stdio::null());
        command.stdout(std::process::Stdio::null());
        command.stderr(std::process::Stdio::null());

        Ok(command.status().await?.success())
    }
}

/// How many times the sandbox is tried out before it is given up on.
const PROBE_ATTEMPTS: u32 = 3;

/// How long to wait between those tries.
const PROBE_PAUSE: Duration = Duration::from_millis(50);

/// The home directory, if it is known.
fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path != Path::new("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sandbox with the given settings.
    ///
    /// The workspace is a directory that is really there, so the sandbox
    /// binds it as it would in use.
    fn sandbox(enabled: bool, network: bool, hide_home: bool) -> Sandbox {
        let workspace = std::env::current_dir().expect("there is a directory to work in");
        Sandbox::new(
            SandboxConfig {
                enabled,
                network,
                hide_home,
            },
            workspace,
        )
    }

    #[test]
    fn the_workspace_is_the_one_place_written() {
        let sandbox = sandbox(true, true, false);
        let workspace = sandbox.workspace.to_string_lossy().to_string();
        let args = sandbox.argv("ls");
        let bind = args.iter().position(|a| a == "--bind").expect("a bind");
        assert_eq!(args[bind + 1], workspace);
        assert_eq!(args[bind + 2], workspace);
        assert!(
            args.contains(&"--ro-bind".to_string()),
            "the disk is not read-only"
        );
    }

    #[test]
    fn a_workspace_in_the_home_directory_stays_reachable() {
        let home = home_directory().expect("there is a home directory");
        let sandbox = Sandbox::new(
            SandboxConfig {
                enabled: true,
                network: true,
                hide_home: true,
            },
            home.join("work"),
        )
        .with_program("/bin/true");
        // The directory has to exist for it to be bound.
        let args = {
            let mut sandbox = sandbox;
            sandbox.workspace = std::env::current_dir().expect("a directory");
            sandbox.argv("ls")
        };
        let home_at = args.iter().position(|a| a == "--tmpfs");
        let bind_at = args.iter().position(|a| a == "--bind");
        assert!(home_at.is_some(), "the home directory is not hidden");
        assert!(bind_at.is_some(), "the workspace is not bound");
        assert!(
            bind_at > home_at,
            "the workspace is hidden by the home directory"
        );
    }

    #[test]
    fn the_network_is_cut_off_when_it_should_be() {
        let with = sandbox(true, true, false).argv("ls");
        assert!(!with.contains(&"--unshare-net".to_string()));

        let without = sandbox(true, false, false).argv("ls");
        assert!(without.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn the_command_is_run_by_a_shell() {
        let args = sandbox(true, true, false).argv("cargo test | tee log");
        let tail = &args[args.len() - 3..];
        assert_eq!(tail, ["bash", "-lc", "cargo test | tee log"]);
    }

    #[tokio::test]
    async fn a_sandbox_that_works_is_used() {
        // A program that always succeeds stands in for bubblewrap.
        let sandbox = sandbox(true, true, false).with_program("/bin/true");
        assert!(sandbox.available().await);
        let args = sandbox.wrap("ls").await.expect("the command is wrapped");
        assert!(args.contains(&"--ro-bind".to_string()));
    }

    #[tokio::test]
    async fn a_sandbox_that_does_not_work_is_left_alone() {
        // A program that always fails stands in for a bubblewrap that
        // cannot set its namespaces up.
        let sandbox = sandbox(true, true, false).with_program("/bin/false");
        assert!(!sandbox.available().await);
        assert!(sandbox.wrap("ls").await.is_none());
    }

    #[tokio::test]
    async fn a_sandbox_that_is_not_there_is_left_alone() {
        let sandbox = sandbox(true, true, false).with_program("/nonexistent/bwrap");
        assert!(!sandbox.available().await);
        assert!(sandbox.wrap("ls").await.is_none());
    }

    #[tokio::test]
    async fn a_sandbox_that_is_switched_off_is_not_even_asked_about() {
        let sandbox = sandbox(false, true, false).with_program("/bin/false");
        assert!(!sandbox.available().await);
        assert!(sandbox.wrap("ls").await.is_none());
    }

    /// A program that is written and made runnable.
    fn a_program(dir: &Path, name: &str, body: &str) -> PathBuf {
        let program = dir.join(name);
        std::fs::write(&program, body).expect("the program is written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&program)
                .expect("the program is there")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&program, permissions).expect("the program may be run");
        }
        program
    }

    #[tokio::test]
    async fn a_sandbox_that_is_busy_at_first_is_asked_again() {
        let dir = std::env::temp_dir().join(format!("lia-busy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("the directory is made");

        let program = a_program(&dir, "busy-bwrap", "#!/bin/sh\nexit 0\n");

        // Something has the program open for writing, so the system will
        // not run it: it says the file is busy. That passes — the handle
        // is let go a moment later — so the sandbox is worth asking about
        // again.
        let handle = std::fs::OpenOptions::new()
            .write(true)
            .open(&program)
            .expect("the program can be opened for writing");
        let release = {
            let program = program.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                drop(handle);
                drop(program);
            })
        };

        let sandbox = sandbox(true, true, false).with_program(program);
        assert!(
            sandbox.available().await,
            "a sandbox that was only briefly busy was given up on"
        );

        let _ = release.await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_sandbox_that_says_no_is_not_asked_again() {
        let dir = std::env::temp_dir().join(format!("lia-busy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("the directory is made");

        // The program runs and says no. That is an answer, and asking
        // again would not change it.
        let counter = dir.join("count.txt");
        let program = a_program(
            &dir,
            "busy-bwrap",
            &format!("#!/bin/sh\nprintf 'x' >> {}\nexit 1\n", counter.display()),
        );

        let sandbox = sandbox(true, true, false).with_program(program);
        assert!(!sandbox.available().await);

        let runs = std::fs::read_to_string(&counter).unwrap_or_default().len();
        assert_eq!(runs, 1, "the sandbox was asked again after it said no");

        let _ = std::fs::remove_dir_all(dir);
    }
}
