//! A reading of the machine.
//!
//! The interface shows a live system monitor, and the assistant can be asked
//! about the machine it is running on. Both read the same snapshot from here,
//! so the number in the panel and the number in an answer cannot disagree.
//!
//! The reading is taken from the kernel's own files: `/proc` on Linux, with
//! `statvfs` for the disks and the usual power-supply and thermal files for
//! the parts that have them. Nothing here changes the machine; every file is
//! read, and the only cost is the short sample the CPU and network rates are
//! measured over.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// How long a rate is measured over when the caller does not say.
pub const SAMPLE: Duration = Duration::from_millis(140);

/// A reading of the machine, at one moment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSnapshot {
    /// When the reading was taken, as an RFC 3339 timestamp.
    pub timestamp: String,
    pub host: HostInfo,
    /// Seconds since the machine booted.
    pub uptime_seconds: u64,
    pub load: LoadInfo,
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub disks: Vec<DiskInfo>,
    pub network: NetworkInfo,
    pub gpu: Option<GpuInfo>,
    pub battery: Option<BatteryInfo>,
    /// The busiest processes, highest CPU first.
    pub processes: Vec<ProcessInfo>,
    pub health: HealthInfo,
}

/// The machine's name and operating system.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostInfo {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
}

/// The one, five and fifteen minute load averages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadInfo {
    pub one: f32,
    pub five: f32,
    pub fifteen: f32,
}

/// The processor's state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CpuInfo {
    /// Busy time over the sample, as a percentage of all cores.
    pub usage_percent: f32,
    pub cores: usize,
    pub model: Option<String>,
    /// The warmest sensor that could be read, in degrees Celsius.
    pub temperature_c: Option<f32>,
}

/// What memory is in use.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryInfo {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub percent: f32,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_percent: f32,
}

/// One mounted filesystem.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskInfo {
    pub mount: String,
    pub filesystem: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub percent: f32,
}

/// The network's counters and the rate they are moving at.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkInfo {
    pub rx_bytes_per_second: f64,
    pub tx_bytes_per_second: f64,
    pub rx_bytes_total: u64,
    pub tx_bytes_total: u64,
    pub interfaces: Vec<InterfaceInfo>,
}

/// One network interface.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InterfaceInfo {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_bytes_per_second: f64,
    pub tx_bytes_per_second: f64,
}

/// A graphics processor, when one can be read.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpuInfo {
    pub name: Option<String>,
    pub usage_percent: Option<f32>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
}

/// The battery, when the machine has one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatteryInfo {
    pub percent: f32,
    pub status: String,
}

/// One process, as the monitor sees it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub state: String,
    /// CPU used over the sample, where one core is 100.
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub user: Option<String>,
}

/// The health summary the monitor draws its ring from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HealthInfo {
    /// A score from 0 to 100, where 100 is nothing wrong.
    pub score: u8,
    /// `optimal`, `stable`, `degraded` or `critical`.
    pub status: String,
    pub checks: Vec<HealthCheck>,
}

/// One thing the health check looked at.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HealthCheck {
    pub name: String,
    /// `ok`, `warn` or `critical`.
    pub status: String,
    pub detail: String,
}

impl SystemSnapshot {
    /// The worst disk in the reading, if there is one.
    pub fn busiest_disk(&self) -> Option<&DiskInfo> {
        self.disks.iter().max_by(|a, b| {
            a.percent
                .partial_cmp(&b.percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }

    /// A short line saying how the machine is.
    pub fn headline(&self) -> String {
        format!(
            "{} · CPU {:.0}% · memory {:.0}% · load {:.2}",
            self.health.status, self.cpu.usage_percent, self.memory.percent, self.load.one
        )
    }
}

/// Read the machine, measuring the rates over `sample`.
pub fn collect(sample: Duration) -> SystemSnapshot {
    let started = Instant::now();

    let cpu_before = cpu_tick();
    let net_before = net_counters();
    let proc_before = process_ticks();

    std::thread::sleep(sample);

    let cpu_after = cpu_tick();
    let net_after = net_counters();
    let elapsed = started.elapsed().as_secs_f64().max(0.001);

    let uptime_seconds = uptime_seconds();
    let load = load_average();
    let cores = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);

    let (usage_percent, total_delta) = match (cpu_before, cpu_after) {
        (Some(before), Some(after)) => {
            let total = after.total.saturating_sub(before.total);
            let idle = after.idle.saturating_sub(before.idle);
            if total == 0 {
                (0.0, 0)
            } else {
                (
                    ((total.saturating_sub(idle)) as f64 / total as f64 * 100.0) as f32,
                    total,
                )
            }
        }
        _ => (0.0, 0),
    };

    let memory = memory_info();
    let disks = disk_info();
    let network = network_info(net_before, net_after, elapsed);
    let gpu = gpu_info();
    let battery = battery_info();

    let mut processes: Vec<ProcessInfo> = process_list(&proc_before, total_delta, cores);
    processes.sort_by(|a, b| {
        b.cpu_percent
            .partial_cmp(&a.cpu_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.memory_bytes.cmp(&a.memory_bytes))
    });
    processes.truncate(12);

    let cpu = CpuInfo {
        usage_percent,
        cores,
        model: cpu_model(),
        temperature_c: temperature_c(),
    };

    let mut snapshot = SystemSnapshot {
        timestamp: chrono::Local::now().to_rfc3339(),
        host: host_info(),
        uptime_seconds,
        load,
        cpu,
        memory,
        disks,
        network,
        gpu,
        battery,
        processes,
        health: HealthInfo::default(),
    };
    snapshot.health = health_info(&snapshot);
    snapshot
}

/// Read the machine with the default sample.
pub fn snapshot() -> SystemSnapshot {
    collect(SAMPLE)
}

#[derive(Debug, Clone, Copy)]
struct CpuTick {
    total: u64,
    idle: u64,
}

fn cpu_tick() -> Option<CpuTick> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    let line = text.lines().next()?;
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let values: Vec<u64> = fields.filter_map(|value| value.parse().ok()).collect();
    if values.len() < 4 {
        return None;
    }
    // user, nice, system, idle, iowait, irq, softirq, steal
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    let total = values.iter().sum();
    Some(CpuTick { total, idle })
}

fn cpu_model() -> Option<String> {
    let text = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("model name") {
            return Some(value.trim_start_matches(':').trim().to_string());
        }
    }
    None
}

fn uptime_seconds() -> u64 {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|text| {
            text.split_whitespace()
                .next()
                .and_then(|value| value.parse::<f64>().ok())
        })
        .map(|seconds| seconds.max(0.0) as u64)
        .unwrap_or(0)
}

fn load_average() -> LoadInfo {
    let Ok(text) = std::fs::read_to_string("/proc/loadavg") else {
        return LoadInfo::default();
    };
    let values: Vec<f32> = text
        .split_whitespace()
        .take(3)
        .filter_map(|value| value.parse().ok())
        .collect();
    LoadInfo {
        one: values.first().copied().unwrap_or(0.0),
        five: values.get(1).copied().unwrap_or(0.0),
        fifteen: values.get(2).copied().unwrap_or(0.0),
    }
}

fn memory_info() -> MemoryInfo {
    let mut values: HashMap<String, u64> = HashMap::new();
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        for line in text.lines() {
            if let Some((key, rest)) = line.split_once(':')
                && let Some(value) = rest.split_whitespace().next()
                    && let Ok(kib) = value.parse::<u64>() {
                        values.insert(key.trim().to_string(), kib);
                    }
        }
    }
    let kib = |key: &str| values.get(key).copied().unwrap_or(0) * 1024;
    let total = kib("MemTotal");
    let available = kib("MemAvailable");
    let used = total.saturating_sub(available);
    let swap_total = kib("SwapTotal");
    let swap_used = swap_total.saturating_sub(kib("SwapFree"));
    MemoryInfo {
        total_bytes: total,
        used_bytes: used,
        available_bytes: available,
        percent: percent(used, total),
        swap_total_bytes: swap_total,
        swap_used_bytes: swap_used,
        swap_percent: percent(swap_used, swap_total),
    }
}

fn percent(used: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64 * 100.0) as f32
    }
}

fn disk_info() -> Vec<DiskInfo> {
    let mut disks = Vec::new();
    let mut seen_devices: Vec<String> = Vec::new();

    if let Ok(text) = std::fs::read_to_string("/proc/mounts") {
        for line in text.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 3 {
                continue;
            }
            let device = fields[0];
            let mount = fields[1];
            let filesystem = fields[2];
            if !real_filesystem(filesystem) || !real_mount(mount) {
                continue;
            }
            // The same device mounted twice is one disk to a person.
            if seen_devices.iter().any(|seen| seen == device) {
                continue;
            }
            let Some(info) = disk_usage(mount, filesystem) else {
                continue;
            };
            seen_devices.push(device.to_string());
            disks.push(info);
            if disks.len() >= 8 {
                break;
            }
        }
    }

    disks.sort_by(|a, b| {
        b.percent
            .partial_cmp(&a.percent)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    disks
}

fn real_filesystem(filesystem: &str) -> bool {
    !matches!(
        filesystem,
        "proc"
            | "sysfs"
            | "devtmpfs"
            | "devpts"
            | "tmpfs"
            | "ramfs"
            | "cgroup"
            | "cgroup2"
            | "pstore"
            | "securityfs"
            | "debugfs"
            | "tracefs"
            | "configfs"
            | "fusectl"
            | "mqueue"
            | "hugetlbfs"
            | "autofs"
            | "binfmt_misc"
            | "rpc_pipefs"
            | "nsfs"
            | "fuse.portal"
            | "squashfs"
            | "efivarfs"
            | "bpf"
    )
}

fn real_mount(mount: &str) -> bool {
    !["/proc", "/sys", "/dev", "/run", "/snap", "/boot/efi"]
        .iter()
        .any(|prefix| mount == *prefix || mount.starts_with(&format!("{prefix}/")))
}

#[cfg(unix)]
fn disk_usage(mount: &str, filesystem: &str) -> Option<DiskInfo> {
    use std::ffi::CString;

    let path = CString::new(mount).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: the path is a valid C string, and the buffer is a full
    // statvfs that the call fills in.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = stats.f_frsize as u64;
    let total = (stats.f_blocks as u64).saturating_mul(block);
    let available = (stats.f_bavail as u64).saturating_mul(block);
    let free = (stats.f_bfree as u64).saturating_mul(block);
    let used = total.saturating_sub(free);
    Some(DiskInfo {
        mount: mount.to_string(),
        filesystem: filesystem.to_string(),
        total_bytes: total,
        used_bytes: used,
        available_bytes: available,
        percent: percent(used, total),
    })
}

#[cfg(not(unix))]
fn disk_usage(_mount: &str, _filesystem: &str) -> Option<DiskInfo> {
    None
}

fn net_counters() -> HashMap<String, (u64, u64)> {
    let mut counters = HashMap::new();
    let Ok(text) = std::fs::read_to_string("/proc/net/dev") else {
        return counters;
    };
    for line in text.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 9 {
            continue;
        }
        let rx = fields[0].parse::<u64>().unwrap_or(0);
        let tx = fields[8].parse::<u64>().unwrap_or(0);
        counters.insert(name.trim().to_string(), (rx, tx));
    }
    counters
}

fn network_info(
    before: HashMap<String, (u64, u64)>,
    after: HashMap<String, (u64, u64)>,
    elapsed: f64,
) -> NetworkInfo {
    let mut interfaces = Vec::new();
    let mut total_rx = 0u64;
    let mut total_tx = 0u64;
    let mut rate_rx = 0.0;
    let mut rate_tx = 0.0;

    for (name, (rx, tx)) in &after {
        let (old_rx, old_tx) = before.get(name).copied().unwrap_or((0, 0));
        let rx_delta = rx.saturating_sub(old_rx);
        let tx_delta = tx.saturating_sub(old_tx);
        let rx_rate = rx_delta as f64 / elapsed;
        let tx_rate = tx_delta as f64 / elapsed;

        total_rx = total_rx.saturating_add(*rx);
        total_tx = total_tx.saturating_add(*tx);
        // The loopback is busy whenever the gateway is used. It is not the
        // wire, so it does not count towards the machine's throughput.
        if name != "lo" {
            rate_rx += rx_rate;
            rate_tx += tx_rate;
        }
        interfaces.push(InterfaceInfo {
            name: name.clone(),
            rx_bytes: *rx,
            tx_bytes: *tx,
            rx_bytes_per_second: rx_rate,
            tx_bytes_per_second: tx_rate,
        });
    }

    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    NetworkInfo {
        rx_bytes_per_second: rate_rx,
        tx_bytes_per_second: rate_tx,
        rx_bytes_total: total_rx,
        tx_bytes_total: total_tx,
        interfaces,
    }
}

fn gpu_info() -> Option<GpuInfo> {
    let entries = std::fs::read_dir("/sys/class/drm").ok()?;
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let device = entry.path().join("device");
        let busy = std::fs::read_to_string(device.join("gpu_busy_percent"))
            .ok()
            .and_then(|text| text.trim().parse::<f32>().ok());
        let used = std::fs::read_to_string(device.join("mem_info_vram_used"))
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok());
        let total = std::fs::read_to_string(device.join("mem_info_vram_total"))
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok());
        let driver = std::fs::read_to_string(device.join("uevent"))
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("DRIVER=").map(str::to_string))
            });
        if busy.is_some() || used.is_some() || total.is_some() {
            return Some(GpuInfo {
                name: driver,
                usage_percent: busy,
                memory_used_bytes: used,
                memory_total_bytes: total,
            });
        }
    }
    None
}

fn battery_info() -> Option<BatteryInfo> {
    let entries = std::fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("BAT") {
            continue;
        }
        let percent = std::fs::read_to_string(entry.path().join("capacity"))
            .ok()
            .and_then(|text| text.trim().parse::<f32>().ok())?;
        let status = std::fs::read_to_string(entry.path().join("status"))
            .ok()
            .map(|text| text.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        return Some(BatteryInfo { percent, status });
    }
    None
}

fn process_ticks() -> HashMap<u32, u64> {
    let mut ticks = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return ticks;
    };
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if let Some(value) = process_tick(pid) {
            ticks.insert(pid, value);
        }
    }
    ticks
}

/// Read only the CPU ticks from /proc/PID/stat. The sampling pass needs
/// nothing else, so the name, state and memory are left unparsed.
fn process_tick(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = text.rfind(')')?;
    let rest = text.get(close + 2..)?;
    let mut fields = rest.split_whitespace();
    // The first field after the closing parenthesis is the state.
    let _state = fields.next()?;
    // Skip through to utime (field 14) and stime (field 15) of /proc/PID/stat.
    for _ in 0..10 {
        fields.next()?;
    }
    let utime = fields.next()?.parse::<u64>().ok()?;
    let stime = fields.next()?.parse::<u64>().ok()?;
    Some(utime.saturating_add(stime))
}

struct ProcessStat {
    name: String,
    state: String,
    ticks: u64,
    rss_pages: u64,
}

fn read_process_stat(pid: u32) -> Option<ProcessStat> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let name = text.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    let state = rest.first()?.to_string();
    let utime = rest.get(11)?.parse::<u64>().ok()?;
    let stime = rest.get(12)?.parse::<u64>().ok()?;
    let rss_pages = rest.get(21)?.parse::<u64>().unwrap_or(0);
    Some(ProcessStat {
        name,
        state,
        ticks: utime.saturating_add(stime),
        rss_pages,
    })
}

fn process_list(
    before: &HashMap<u32, u64>,
    total_delta: u64,
    cores: usize,
) -> Vec<ProcessInfo> {
    let users = user_names();
    let page_size = 4096u64;
    let mut processes = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return processes;
    };
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let Some(stat) = read_process_stat(pid) else {
            continue;
        };
        let old = before.get(&pid).copied().unwrap_or(stat.ticks);
        let delta = stat.ticks.saturating_sub(old);
        let cpu_percent = if total_delta == 0 {
            0.0
        } else {
            (delta as f64 / total_delta as f64 * cores as f64 * 100.0) as f32
        };
        // The UID is the owner of the /proc/PID directory, so it can be
        // taken from the entry's metadata instead of reading /proc/PID/status.
        let uid = entry.metadata().ok().map(|meta| {
            use std::os::unix::fs::MetadataExt;
            meta.uid()
        });
        processes.push(ProcessInfo {
            pid,
            name: stat.name,
            state: stat.state,
            cpu_percent,
            memory_bytes: stat.rss_pages.saturating_mul(page_size),
            user: uid.and_then(|uid| users.get(&uid).cloned()),
        });
    }
    processes
}

fn user_names() -> HashMap<u32, String> {
    let mut users = HashMap::new();
    let Ok(text) = std::fs::read_to_string("/etc/passwd") else {
        return users;
    };
    for line in text.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 3 {
            continue;
        }
        if let Ok(uid) = fields[2].parse::<u32>() {
            users.insert(uid, fields[0].to_string());
        }
    }
    users
}

fn host_info() -> HostInfo {
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|text| text.trim().to_string())
        .unwrap_or_else(|_| "local".to_string());
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|text| text.trim().to_string())
        .unwrap_or_default();
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("PRETTY_NAME=")
                    .map(|value| value.trim_matches('"').to_string())
            })
        })
        .unwrap_or_else(|| std::env::consts::OS.to_string());
    HostInfo {
        hostname,
        os,
        kernel,
        arch: std::env::consts::ARCH.to_string(),
    }
}

fn temperature_c() -> Option<f32> {
    let mut warmest: Option<f32> = None;
    for root in ["/sys/class/thermal", "/sys/class/hwmon"] {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            if root.ends_with("thermal") {
                if let Some(value) = read_millidegrees(&path.join("temp")) {
                    warmest = Some(warmest.map_or(value, |old| old.max(value)));
                }
                continue;
            }
            let Ok(files) = std::fs::read_dir(&path) else {
                continue;
            };
            for file in files.filter_map(std::result::Result::ok) {
                let name = file.file_name().to_string_lossy().to_string();
                if !name.starts_with("temp") || !name.ends_with("_input") {
                    continue;
                }
                if let Some(value) = read_millidegrees(&file.path()) {
                    // Some hwmon sensors are not temperatures; a plausible
                    // range keeps a fan reading from being called a fever.
                    if (5.0..=150.0).contains(&value) {
                        warmest = Some(warmest.map_or(value, |old| old.max(value)));
                    }
                }
            }
        }
    }
    warmest.map(|value| (value * 10.0).round() / 10.0)
}

fn read_millidegrees(path: &std::path::Path) -> Option<f32> {
    let text = std::fs::read_to_string(path).ok()?;
    let value = text.trim().parse::<f32>().ok()?;
    Some(value / 1000.0)
}

fn health_info(snapshot: &SystemSnapshot) -> HealthInfo {
    let mut checks = Vec::new();
    let mut penalty = 0.0f32;

    let memory = snapshot.memory.percent;
    if memory >= 90.0 {
        checks.push(HealthCheck {
            name: "memory".into(),
            status: "critical".into(),
            detail: format!(
                "{} in use of {}",
                humans(snapshot.memory.used_bytes),
                humans(snapshot.memory.total_bytes)
            ),
        });
        penalty += 20.0;
    } else if memory >= 75.0 {
        checks.push(HealthCheck {
            name: "memory".into(),
            status: "warn".into(),
            detail: format!("{memory:.0}% in use"),
        });
        penalty += (memory - 75.0) * 0.5;
    } else {
        checks.push(HealthCheck {
            name: "memory".into(),
            status: "ok".into(),
            detail: format!("{memory:.0}% in use"),
        });
    }

    if snapshot.memory.swap_total_bytes > 0 && snapshot.memory.swap_percent >= 80.0 {
        checks.push(HealthCheck {
            name: "swap".into(),
            status: "warn".into(),
            detail: format!("{:.0}% of swap in use", snapshot.memory.swap_percent),
        });
        penalty += 6.0;
    }

    let cores = snapshot.cpu.cores.max(1) as f32;
    let load_ratio = snapshot.load.one / cores;
    if load_ratio >= 2.0 {
        checks.push(HealthCheck {
            name: "load".into(),
            status: "critical".into(),
            detail: format!("{:.2} on {cores:.0} cores", snapshot.load.one),
        });
        penalty += 20.0;
    } else if load_ratio >= 1.0 {
        checks.push(HealthCheck {
            name: "load".into(),
            status: "warn".into(),
            detail: format!("{:.2} on {cores:.0} cores", snapshot.load.one),
        });
        penalty += (load_ratio - 1.0) * 14.0;
    } else {
        checks.push(HealthCheck {
            name: "load".into(),
            status: "ok".into(),
            detail: format!("{:.2} on {cores:.0} cores", snapshot.load.one),
        });
    }

    if let Some(worst) = snapshot.busiest_disk() {
        if worst.percent >= 90.0 {
            checks.push(HealthCheck {
                name: "disk".into(),
                status: "critical".into(),
                detail: format!("{} is {:.0}% full", worst.mount, worst.percent),
            });
            penalty += 20.0;
        } else if worst.percent >= 80.0 {
            checks.push(HealthCheck {
                name: "disk".into(),
                status: "warn".into(),
                detail: format!("{} is {:.0}% full", worst.mount, worst.percent),
            });
            penalty += (worst.percent - 80.0) * 0.6;
        } else {
            checks.push(HealthCheck {
                name: "disk".into(),
                status: "ok".into(),
                detail: format!("{} is {:.0}% full", worst.mount, worst.percent),
            });
        }
    }

    if let Some(temp) = snapshot.cpu.temperature_c {
        if temp >= 85.0 {
            checks.push(HealthCheck {
                name: "temperature".into(),
                status: "critical".into(),
                detail: format!("{temp:.0}°C"),
            });
            penalty += 18.0;
        } else if temp >= 75.0 {
            checks.push(HealthCheck {
                name: "temperature".into(),
                status: "warn".into(),
                detail: format!("{temp:.0}°C"),
            });
            penalty += (temp - 75.0) * 0.8;
        } else {
            checks.push(HealthCheck {
                name: "temperature".into(),
                status: "ok".into(),
                detail: format!("{temp:.0}°C"),
            });
        }
    }

    if let Some(battery) = &snapshot.battery {
        let charging = battery.status.eq_ignore_ascii_case("charging");
        if !charging && battery.percent <= 15.0 {
            checks.push(HealthCheck {
                name: "battery".into(),
                status: "warn".into(),
                detail: format!("{:.0}% and not charging", battery.percent),
            });
            penalty += 8.0;
        } else {
            checks.push(HealthCheck {
                name: "battery".into(),
                status: "ok".into(),
                detail: format!("{:.0}% {}", battery.percent, battery.status),
            });
        }
    }

    let score = (100.0 - penalty).clamp(0.0, 100.0).round() as u8;
    let status = match score {
        90..=100 => "optimal",
        75..=89 => "stable",
        55..=74 => "degraded",
        _ => "critical",
    };
    HealthInfo {
        score,
        status: status.to_string(),
        checks,
    }
}

/// A size, written the way a person reads it.
pub fn humans(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A duration in seconds, written the way a person reads it.
pub fn human_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    let minutes = seconds % 3_600 / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_has_a_shape() {
        let snapshot = collect(Duration::from_millis(20));
        assert!(!snapshot.timestamp.is_empty());
        assert!(snapshot.cpu.cores > 0);
        assert!(snapshot.memory.total_bytes > 0);
        assert!(snapshot.memory.percent <= 100.0);
        assert!(snapshot.health.score <= 100);
        assert!(!snapshot.health.checks.is_empty());
    }

    #[test]
    fn sizes_and_durations_read_well() {
        assert_eq!(humans(512), "512 B");
        assert_eq!(humans(1024), "1.0 KB");
        assert_eq!(humans(1024 * 1024 * 2), "2.0 MB");
        assert_eq!(human_duration(59), "59s");
        assert_eq!(human_duration(3_600 + 120), "1h 2m");
        assert_eq!(human_duration(86_400 * 2 + 3_600 * 5), "2d 5h");
    }
}
