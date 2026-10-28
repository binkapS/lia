//! Tools for looking at the machine Lia is running on.
//!
//! The interface has a system monitor, a diagnostics button and a security
//! readout. These are the same readings, offered to the model as tools, so
//! "how is this machine doing?" can be answered from the machine rather
//! than from a guess.
//!
//! Everything here is read-only. None of these tools need approval, because
//! none of them can change anything.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::config::Config;
use crate::system;
use crate::tool::{Result, Tool, ToolOutput};

/// How long the tools sample the CPU and network for.
const TOOL_SAMPLE: Duration = Duration::from_millis(180);

fn sample_ms(args: &Value) -> Duration {
    let milliseconds = args
        .get("sample_ms")
        .and_then(Value::as_u64)
        .unwrap_or(TOOL_SAMPLE.as_millis() as u64)
        .clamp(0, 2_000);
    Duration::from_millis(milliseconds)
}

fn status_mark(status: &str) -> &'static str {
    match status {
        "ok" => "✓",
        "warn" => "!",
        "critical" => "✗",
        _ => "·",
    }
}

/// Report the machine's health.
pub struct SystemHealthTool;

#[async_trait]
impl Tool for SystemHealthTool {
    fn name(&self) -> &str {
        "system_health"
    }

    fn description(&self) -> &str {
        "Read this machine's health: CPU, memory, disk, network, temperature, \
         uptime, battery and the busiest processes. Use it when the user asks \
         how the machine is doing, before a long or heavy job, or when \
         something seems slow. It only reads; it changes nothing."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "detail": {
                    "type": "string",
                    "enum": ["summary", "full"],
                    "description": "Summary leaves out the process table and the individual checks."
                },
                "sample_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 2000,
                    "description": "How long to measure CPU and network rates over, in milliseconds."
                }
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let full = args
            .get("detail")
            .and_then(Value::as_str)
            .map(|detail| detail.eq_ignore_ascii_case("full"))
            .unwrap_or(false);
        let snapshot = system::collect(sample_ms(&args));

        let mut out = String::from("# System health\n\n");
        out.push_str(&format!(
            "**{}** on {} ({}, kernel {}) — **{}** (score {}/100)\n\n",
            snapshot.host.hostname,
            snapshot.host.os,
            snapshot.host.arch,
            snapshot.host.kernel,
            snapshot.health.status,
            snapshot.health.score
        ));
        out.push_str(&format!(
            "- Uptime: {}\n- Load: {:.2} / {:.2} / {:.2}\n",
            system::human_duration(snapshot.uptime_seconds),
            snapshot.load.one,
            snapshot.load.five,
            snapshot.load.fifteen
        ));
        out.push_str(&format!(
            "- CPU: {:.0}% of {} cores{}",
            snapshot.cpu.usage_percent,
            snapshot.cpu.cores,
            snapshot
                .cpu
                .temperature_c
                .map(|temp| format!(" · {temp:.0}°C"))
                .unwrap_or_default()
        ));
        if let Some(model) = &snapshot.cpu.model {
            out.push_str(&format!(" · {model}"));
        }
        out.push('\n');
        out.push_str(&format!(
            "- Memory: {} of {} ({:.0}%)",
            system::humans(snapshot.memory.used_bytes),
            system::humans(snapshot.memory.total_bytes),
            snapshot.memory.percent
        ));
        if snapshot.memory.swap_total_bytes > 0 {
            out.push_str(&format!(
                " · swap {} of {} ({:.0}%)",
                system::humans(snapshot.memory.swap_used_bytes),
                system::humans(snapshot.memory.swap_total_bytes),
                snapshot.memory.swap_percent
            ));
        }
        out.push('\n');

        if !snapshot.disks.is_empty() {
            out.push_str("- Disks:\n");
            for disk in &snapshot.disks {
                out.push_str(&format!(
                    "  - `{}` ({}): {} of {} ({:.0}%)\n",
                    disk.mount,
                    disk.filesystem,
                    system::humans(disk.used_bytes),
                    system::humans(disk.total_bytes),
                    disk.percent
                ));
            }
        }

        out.push_str(&format!(
            "- Network: ↓ {}/s · ↑ {}/s\n",
            system::humans(snapshot.network.rx_bytes_per_second as u64),
            system::humans(snapshot.network.tx_bytes_per_second as u64)
        ));
        if let Some(gpu) = &snapshot.gpu {
            out.push_str(&format!(
                "- GPU: {}{}",
                gpu.usage_percent
                    .map(|percent| format!("{percent:.0}%"))
                    .unwrap_or_else(|| "present".into()),
                gpu.memory_total_bytes
                    .map(|total| format!(
                        " · {} of {}",
                        system::humans(gpu.memory_used_bytes.unwrap_or(0)),
                        system::humans(total)
                    ))
                    .unwrap_or_default()
            ));
            if let Some(name) = &gpu.name {
                out.push_str(&format!(" · {name}"));
            }
            out.push('\n');
        }
        if let Some(battery) = &snapshot.battery {
            out.push_str(&format!(
                "- Battery: {:.0}% ({})\n",
                battery.percent, battery.status
            ));
        }

        if full {
            out.push_str("\n## Checks\n");
            for check in &snapshot.health.checks {
                out.push_str(&format!(
                    "- {} {}: {}\n",
                    status_mark(&check.status),
                    check.name,
                    check.detail
                ));
            }

            out.push_str("\n## Top processes\n");
            out.push_str("| PID | Process | CPU | Memory | User |\n|---:|---|---:|---:|---|\n");
            for process in &snapshot.processes {
                out.push_str(&format!(
                    "| {} | {} | {:.1}% | {} | {} |\n",
                    process.pid,
                    process.name,
                    process.cpu_percent,
                    system::humans(process.memory_bytes),
                    process.user.as_deref().unwrap_or("—")
                ));
            }
        }

        Ok(ToolOutput::ok(out).with_metadata(json!({
            "health": {
                "score": snapshot.health.score,
                "status": snapshot.health.status,
                "checks": snapshot.health.checks,
            },
            "cpu_percent": snapshot.cpu.usage_percent,
            "memory_percent": snapshot.memory.percent,
            "uptime_seconds": snapshot.uptime_seconds,
            "load": snapshot.load,
        })))
    }
}

/// List the busiest processes.
pub struct ProcessListTool;

#[async_trait]
impl Tool for ProcessListTool {
    fn name(&self) -> &str {
        "system_processes"
    }

    fn description(&self) -> &str {
        "List the processes using the most CPU or memory on this machine. Use \
         it when the system is slow and the cause is not clear, or when the \
         user asks what is running. It only reads; it changes nothing."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "sort": {
                    "type": "string",
                    "enum": ["cpu", "memory"],
                    "description": "Which column to sort by. Defaults to cpu."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "How many processes to show. Defaults to 12."
                },
                "sample_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 2000,
                    "description": "How long to measure CPU use over, in milliseconds."
                }
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let by_memory = args
            .get("sort")
            .and_then(Value::as_str)
            .map(|sort| sort.eq_ignore_ascii_case("memory"))
            .unwrap_or(false);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(12)
            .clamp(1, 50) as usize;

        let snapshot = system::collect(sample_ms(&args));
        let mut processes = snapshot.processes.clone();
        if by_memory {
            processes.sort_by_key(|a| std::cmp::Reverse(a.memory_bytes));
        }
        processes.truncate(limit);

        let mut out = format!(
            "# Processes by {}\n\n{} · CPU {:.0}% · memory {:.0}% · load {:.2}\n\n",
            if by_memory { "memory" } else { "CPU" },
            snapshot.host.hostname,
            snapshot.cpu.usage_percent,
            snapshot.memory.percent,
            snapshot.load.one
        );
        out.push_str("| PID | Process | CPU | Memory | User |\n|---:|---|---:|---:|---|\n");
        for process in &processes {
            out.push_str(&format!(
                "| {} | {} | {:.1}% | {} | {} |\n",
                process.pid,
                process.name,
                process.cpu_percent,
                system::humans(process.memory_bytes),
                process.user.as_deref().unwrap_or("—")
            ));
        }
        out.push_str(
            "\nProcess CPU is measured over the sample window; 100% is one \
             core. The full table is a snapshot, not a history.\n",
        );
        Ok(ToolOutput::ok(out).with_metadata(json!({
            "processes": processes,
            "sort": if by_memory { "memory" } else { "cpu" },
        })))
    }
}

/// Report the network's interfaces and throughput.
pub struct NetworkStatusTool;

#[async_trait]
impl Tool for NetworkStatusTool {
    fn name(&self) -> &str {
        "system_network"
    }

    fn description(&self) -> &str {
        "Read the network interfaces and their throughput: bytes received \
         and sent, per second and in total. Use it when a request is slow, \
         when the user asks whether the machine is online, or before a large \
         download. It only reads; it changes nothing."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "sample_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 2000,
                    "description": "How long to measure throughput over, in milliseconds."
                }
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let snapshot = system::collect(sample_ms(&args));
        let network = &snapshot.network;
        let mut out = format!(
            "# Network\n\nThroughput: ↓ {}/s · ↑ {}/s\nTotals: ↓ {} · ↑ {}\n\n",
            system::humans(network.rx_bytes_per_second as u64),
            system::humans(network.tx_bytes_per_second as u64),
            system::humans(network.rx_bytes_total),
            system::humans(network.tx_bytes_total)
        );
        out.push_str("| Interface | Down | Up | Received | Sent |\n|---|---:|---:|---:|---:|\n");
        for interface in &network.interfaces {
            out.push_str(&format!(
                "| {} | {}/s | {}/s | {} | {} |\n",
                interface.name,
                system::humans(interface.rx_bytes_per_second as u64),
                system::humans(interface.tx_bytes_per_second as u64),
                system::humans(interface.rx_bytes),
                system::humans(interface.tx_bytes)
            ));
        }
        out.push_str(
            "\nRates are measured over the sample window. Loopback traffic is \
             counted in the totals but left out of the throughput line.\n",
        );
        Ok(ToolOutput::ok(out).with_metadata(json!({
            "rx_bytes_per_second": network.rx_bytes_per_second,
            "tx_bytes_per_second": network.tx_bytes_per_second,
            "interfaces": network.interfaces,
        })))
    }
}

/// Run the health checks and say what they mean.
pub struct DiagnosticsTool;

#[async_trait]
impl Tool for DiagnosticsTool {
    fn name(&self) -> &str {
        "system_diagnostics"
    }

    fn description(&self) -> &str {
        "Run a short diagnostic scan of this machine and report the findings, \
         with a score and concrete next steps. Use it for \"run diagnostics\" \
         or \"scan the system\". It only reads; it changes nothing."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "sample_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 2000,
                    "description": "How long to measure CPU and network rates over, in milliseconds."
                }
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let snapshot = system::collect(sample_ms(&args));
        let mut out = String::from("# Diagnostic scan\n\n");
        out.push_str(&format!(
            "**{}** — {} (score {}/100)\n\n",
            snapshot.health.status.to_uppercase(),
            snapshot.host.hostname,
            snapshot.health.score
        ));
        out.push_str(&format!(
            "{} · CPU {:.0}% · memory {:.0}% · load {:.2} · uptime {}\n\n",
            snapshot.host.os,
            snapshot.cpu.usage_percent,
            snapshot.memory.percent,
            snapshot.load.one,
            system::human_duration(snapshot.uptime_seconds)
        ));

        out.push_str("## Checks\n");
        for check in &snapshot.health.checks {
            out.push_str(&format!(
                "- {} **{}**: {}\n",
                status_mark(&check.status),
                check.name,
                check.detail
            ));
        }

        out.push_str("\n## Findings\n");
        let problems: Vec<&system::HealthCheck> = snapshot
            .health
            .checks
            .iter()
            .filter(|check| check.status != "ok")
            .collect();
        if problems.is_empty() {
            out.push_str("- No anomalies detected. Every check is within its normal range.\n");
        } else {
            for check in &problems {
                let advice = match check.name.as_str() {
                    "memory" => "inspect `system_processes` with sort=memory; stop or restart the largest consumer before starting another big job",
                    "swap" => "memory is being pushed to disk; find the process that grew and free memory before a heavy build",
                    "load" => "inspect `system_processes` with sort=cpu; a long build or a runaway process is the usual cause",
                    "disk" => "free space on the full mount, or move the workspace to one with room",
                    "temperature" => "let the machine cool and check airflow; sustained heat will throttle it",
                    "battery" => "charge the machine before long work, or plug it in",
                    _ => "inspect the reading before deciding",
                };
                out.push_str(&format!(
                    "- **{}** ({}) — {}\n",
                    check.name, check.status, advice
                ));
            }
        }

        if let Some(busiest) = snapshot.processes.first() {
            out.push_str(&format!(
                "\nBusiest process: `{}` (pid {}) at {:.1}% CPU, {} resident.\n",
                busiest.name,
                busiest.pid,
                busiest.cpu_percent,
                system::humans(busiest.memory_bytes)
            ));
        }

        out.push_str(
            "\nA single reading is a moment, not a trend. Take another sample \
             before treating a spike as a fault.\n",
        );
        Ok(ToolOutput::ok(out).with_metadata(json!({
            "score": snapshot.health.score,
            "status": snapshot.health.status,
            "checks": snapshot.health.checks,
            "findings": problems.len(),
        })))
    }
}

/// What the security tool knows about the configuration.
///
/// It is copied out at startup rather than holding the whole configuration,
/// so the tool cannot accidentally print a secret it was never given.
pub struct SecurityFacts {
    pub token_set: bool,
    pub host: String,
    pub port: u16,
    pub approval: String,
    pub sandbox_enabled: bool,
    pub sandbox_network: bool,
    pub tools_enabled: bool,
    pub mcp_servers: usize,
    pub extra_paths: usize,
    pub workspace: String,
    pub config_path: Option<std::path::PathBuf>,
    pub provider: String,
}

impl SecurityFacts {
    pub fn from_config(config: &Config) -> Self {
        SecurityFacts {
            token_set: config
                .gateway
                .token
                .as_deref()
                .map(|token| !token.trim().is_empty())
                .unwrap_or(false),
            host: config.gateway.host.clone(),
            port: config.gateway.port,
            approval: match config.approval.policy {
                crate::tool::approval::ApprovalPolicy::Allow => "allow".into(),
                crate::tool::approval::ApprovalPolicy::Deny => "deny".into(),
                crate::tool::approval::ApprovalPolicy::Ask => "ask".into(),
            },
            sandbox_enabled: config.sandbox.enabled,
            sandbox_network: config.sandbox.network,
            tools_enabled: config.tools_enabled,
            mcp_servers: config.mcp_servers.len(),
            extra_paths: config.extra_allowed_paths.len(),
            workspace: config.workspace.display().to_string(),
            config_path: Some(config.configured_path()),
            provider: config.provider.as_str().to_string(),
        }
    }
}

/// Report how the gateway and the tools are protected.
pub struct SecurityStatusTool {
    facts: SecurityFacts,
}

impl SecurityStatusTool {
    pub fn new(facts: SecurityFacts) -> Self {
        SecurityStatusTool { facts }
    }
}

#[async_trait]
impl Tool for SecurityStatusTool {
    fn name(&self) -> &str {
        "security_status"
    }

    fn description(&self) -> &str {
        "Report the security posture of this Lia installation: whether the \
         gateway has a token, whether it is exposed beyond localhost, the \
         approval policy, the shell sandbox, extra paths, MCP servers and the \
         configuration file's permissions. Use it for \"secure\" or \"audit \
         security\". It never prints secrets."
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn run(&self, _args: Value) -> Result<ToolOutput> {
        let loopback = matches!(
            self.facts.host.as_str(),
            "127.0.0.1" | "::1" | "localhost" | "[::1]"
        );
        let mut checks: Vec<(String, String, String)> = Vec::new();

        if self.facts.token_set {
            checks.push((
                "authentication".into(),
                "ok".into(),
                "the gateway has a token".into(),
            ));
        } else if loopback {
            checks.push((
                "authentication".into(),
                "warn".into(),
                "no gateway token; the gateway is reachable only from this machine".into(),
            ));
        } else {
            checks.push((
                "authentication".into(),
                "critical".into(),
                "no gateway token, and the gateway is bound beyond localhost".into(),
            ));
        }

        if loopback {
            checks.push((
                "bind".into(),
                "ok".into(),
                format!("listening on {}:{} (loopback only)", self.facts.host, self.facts.port),
            ));
        } else {
            checks.push((
                "bind".into(),
                if self.facts.token_set { "warn" } else { "critical" }.into(),
                format!(
                    "listening on {}:{} — anyone who can reach this port can try the gateway",
                    self.facts.host, self.facts.port
                ),
            ));
        }

        let approval = match self.facts.approval.as_str() {
            "allow" => (
                "warn",
                "risky tools run without asking; approval is set to allow",
            ),
            "deny" => ("ok", "risky tools are denied rather than run"),
            _ => ("ok", "risky tools ask before they run"),
        };
        checks.push(("approval".into(), approval.0.into(), approval.1.into()));

        if self.facts.sandbox_enabled {
            checks.push((
                "sandbox".into(),
                "ok".into(),
                if self.facts.sandbox_network {
                    "shell commands are confined, with network access".into()
                } else {
                    "shell commands are confined, without network access".into()
                },
            ));
        } else {
            checks.push((
                "sandbox".into(),
                "warn".into(),
                "the shell sandbox is off; approved commands run directly".into(),
            ));
        }

        if self.facts.extra_paths > 0 {
            checks.push((
                "workspace".into(),
                "warn".into(),
                format!(
                    "{} path(s) outside the workspace are allowed: {}",
                    self.facts.extra_paths, self.facts.workspace
                ),
            ));
        } else {
            checks.push((
                "workspace".into(),
                "ok".into(),
                format!("the assistant is confined to {}", self.facts.workspace),
            ));
        }

        if self.facts.tools_enabled {
            checks.push((
                "tools".into(),
                "ok".into(),
                format!(
                    "tools are enabled; {} MCP server(s) configured",
                    self.facts.mcp_servers
                ),
            ));
        } else {
            checks.push((
                "tools".into(),
                "warn".into(),
                "tools are disabled for this installation".into(),
            ));
        }

        #[cfg(unix)]
        if let Some(path) = &self.facts.config_path {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(path) {
                let mode = metadata.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    checks.push((
                        "config".into(),
                        "warn".into(),
                        format!(
                            "{} is mode {:o}; group and other users can read it",
                            path.display(),
                            mode
                        ),
                    ));
                } else {
                    checks.push((
                        "config".into(),
                        "ok".into(),
                        format!("{} is mode {:o}", path.display(), mode),
                    ));
                }
            }
        }

        let critical = checks.iter().filter(|(_, status, _)| status == "critical").count();
        let warnings = checks.iter().filter(|(_, status, _)| status == "warn").count();
        let verdict = if critical > 0 {
            "attention required"
        } else if warnings > 0 {
            "review suggested"
        } else {
            "hardened"
        };

        let mut out = format!(
            "# Security posture\n\n**{}** — {} warning(s), {} critical\n\n",
            verdict, warnings, critical
        );
        for (name, status, detail) in &checks {
            out.push_str(&format!(
                "- {} **{}**: {}\n",
                status_mark(status),
                name,
                detail
            ));
        }
        out.push_str(&format!(
            "\nProvider: {} · configuration: {}\n\n\
             This is a configuration review, not a penetration test.\n",
            self.facts.provider,
            self.facts
                .config_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "not written to a file".into())
        ));

        Ok(ToolOutput::ok(out).with_metadata(json!({
            "warnings": warnings,
            "critical": critical,
            "verdict": verdict,
            "checks": checks,
        })))
    }
}



/// Open an application or URL using the OS launcher.
pub struct OpenAppTool;

#[async_trait::async_trait]
impl Tool for OpenAppTool {
    fn name(&self) -> &str { "open_app" }
    fn description(&self) -> &str { "Open an application or URL with the system launcher." }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "target": { "type": "string", "description": "Application name or URL." }
            },
            "required": ["target"]
        })
    }
    fn requires_approval(&self) -> bool { true }
    async fn run(&self, args: serde_json::Value) -> crate::tool::Result<crate::tool::ToolOutput> {
        let target = args.get("target").and_then(serde_json::Value::as_str).ok_or_else(|| crate::tool::ToolError::InvalidArgs { tool: self.name().into(), reason: "missing target".into() })?;
        let launcher = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let status = tokio::process::Command::new(launcher).arg(target).status().await.map_err(|e| crate::tool::ToolError::Failed(self.name().into(), e.to_string()))?;
        if status.success() {
            Ok(crate::tool::ToolOutput::ok(format!("Opened {target}")))
        } else {
            Err(crate::tool::ToolError::Failed(self.name().into(), format!("launcher failed: {status}")))
        }
    }
}
