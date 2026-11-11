//! Shared status values for the low-process-churn mode.
//!
//! Status formats can be evaluated many times for every redraw. Sampling here
//! lets every client consume the same values without starting a shell job.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regex::Regex;
#[cfg(not(target_os = "macos"))]
use sysinfo::Networks;
use sysinfo::{MemoryRefreshKind, System};

use crate::paths::Paths;
use crate::server::ServerIdentity;
use crate::tmux::{SurfaceInventory, Tmux};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(3);
const USAGE_TTL: Duration = Duration::from_secs(600);
const USAGE_RETRY: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Settings {
    enabled: bool,
    ready: bool,
    metrics: bool,
    usage: bool,
    mode: Mode,
    source: String,
    plugin_dir: PathBuf,
    quiet: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Compact,
    Standard,
    Full,
}

impl Settings {
    pub(crate) fn from_surface(
        surface: &SurfaceInventory,
        low_churn: bool,
        quiet: bool,
        attached: bool,
    ) -> Self {
        let source = match surface.usage_source.as_str() {
            "claude" | "trae" | "opencode" => surface.usage_source.clone(),
            _ => "codex".to_owned(),
        };
        let mode = match surface.metrics_mode.as_str() {
            "standard" => Mode::Standard,
            "full" => Mode::Full,
            _ => Mode::Compact,
        };
        Self {
            enabled: low_churn
                && attached
                && surface.shared_status == "1"
                && (surface.metrics_enabled != "off" || surface.usage_enabled != "off"),
            ready: surface.status_ready == "1",
            metrics: surface.metrics_enabled != "off",
            usage: surface.usage_enabled != "off",
            mode,
            source,
            plugin_dir: PathBuf::from(&surface.plugin_dir),
            quiet,
        }
    }
}

pub(crate) fn start(paths: &Paths, server: &ServerIdentity) -> SyncSender<Settings> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let paths = paths.clone();
    let server = server.clone();
    thread::spawn(move || run(receiver, &paths, &server));
    sender
}

fn run(receiver: Receiver<Settings>, paths: &Paths, server: &ServerIdentity) {
    let tmux = Tmux::new(server.clone());
    let mut sampler = Sampler::new();
    let mut settings = None;
    let mut previous = None;
    let mut next_sample = Instant::now();
    let mut last_usage_refresh: Option<(String, Instant)> = None;
    loop {
        let timeout = next_sample.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(timeout) {
            Ok(next) => {
                if !next.ready {
                    previous = None;
                }
                settings = Some(next);
                next_sample = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if Instant::now() < next_sample {
            continue;
        }
        next_sample = Instant::now() + SAMPLE_INTERVAL;
        let Some(settings) = &settings else { continue };
        if !settings.enabled {
            if previous.take().is_some() || settings.ready {
                let _ = tmux.output_with_timeout(
                    &["set-option", "-gq", "@workbench_status_ready", "0"],
                    Duration::from_secs(2),
                );
            }
            continue;
        }
        if settings.usage {
            maybe_refresh_usage(paths, server, settings, &mut last_usage_refresh);
        }
        let values = sampler.sample(paths, settings);
        if previous.as_ref() != Some(&values) && publish(&tmux, &values).is_ok() {
            previous = Some(values);
        }
    }
    if previous.is_some() {
        let _ = tmux.output_with_timeout(
            &["set-option", "-gq", "@workbench_status_ready", "0"],
            Duration::from_secs(2),
        );
    }
}

fn publish(tmux: &Tmux, values: &[(String, String)]) -> Result<(), crate::tmux::TmuxError> {
    let mut args = Vec::<String>::new();
    for (name, value) in values {
        if !args.is_empty() {
            args.push(";".into());
        }
        args.extend([
            "set-option".into(),
            "-gq".into(),
            name.clone(),
            value.clone(),
        ]);
    }
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    tmux.output_with_timeout(&refs, Duration::from_secs(2))?;
    Ok(())
}

fn maybe_refresh_usage(
    paths: &Paths,
    server: &ServerIdentity,
    settings: &Settings,
    last: &mut Option<(String, Instant)>,
) {
    let cache = paths.state_dir.join("usage").join(&settings.source);
    let stale = fs::metadata(cache)
        .and_then(|metadata| metadata.modified())
        .map(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .is_ok_and(|age| age >= USAGE_TTL)
        })
        .unwrap_or(true);
    if !stale
        || last
            .as_ref()
            .is_some_and(|(source, at)| source == &settings.source && at.elapsed() < USAGE_RETRY)
    {
        return;
    }
    let script = settings.plugin_dir.join("bin/workbench-agent-usage");
    if !settings.plugin_dir.is_absolute() || !script.is_file() {
        return;
    }
    *last = Some((settings.source.clone(), Instant::now()));
    let mut command = Command::new(script);
    command
        .args(["refresh", &settings.source, "automatic"])
        .env("TMUX", format!("{},0,0", server.socket_path.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(mut child) = command.spawn() {
        thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

struct Sampler {
    system: System,
    cpu_sampled: bool,
    #[cfg(not(target_os = "macos"))]
    networks: Networks,
    #[cfg(not(target_os = "macos"))]
    network_baseline: Option<(String, u64, u64)>,
    network_sampled_at: Option<Instant>,
    network_values: (String, String),
}

impl Sampler {
    fn new() -> Self {
        Self {
            system: System::new(),
            cpu_sampled: false,
            #[cfg(not(target_os = "macos"))]
            networks: Networks::new(),
            #[cfg(not(target_os = "macos"))]
            network_baseline: None,
            network_sampled_at: None,
            network_values: ("--".into(), "--".into()),
        }
    }

    fn sample(&mut self, paths: &Paths, settings: &Settings) -> Vec<(String, String)> {
        let mut cpu = "--".to_owned();
        let mut memory = "--".to_owned();
        let mut disk = "--".to_owned();
        if settings.metrics {
            self.system.refresh_cpu_usage();
            if self.cpu_sampled {
                cpu = format!("{:.0}%", self.system.global_cpu_usage().clamp(0.0, 100.0));
            }
            self.cpu_sampled = true;
            self.system
                .refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
            let total = self.system.total_memory();
            if total > 0 {
                #[cfg(target_os = "macos")]
                let used = crate::host_metrics::used_memory_bytes()
                    .unwrap_or_else(|_| self.system.used_memory());
                #[cfg(not(target_os = "macos"))]
                let used = self.system.used_memory();
                memory = format!("{:.0}%", 100.0 * used.min(total) as f64 / total as f64);
            }
            if matches!(settings.mode, Mode::Standard | Mode::Full) {
                let path = std::env::var_os("TMUX_METRICS_DISK_PATH")
                    .map(PathBuf::from)
                    .or_else(dirs::home_dir);
                if let Some(value) = path.as_deref().and_then(disk_usage) {
                    disk = value;
                }
            }
            if settings.mode == Mode::Full {
                self.refresh_network(settings.quiet);
            }
        }
        let (up, down) = &self.network_values;
        let mut values = vec![
            ("@workbench_cached_cpu".into(), format!("󰻠 {cpu}")),
            (
                "@workbench_cached_metrics_compact".into(),
                format!("󰻠 {cpu} 󰍛 {memory}"),
            ),
            (
                "@workbench_cached_metrics_standard".into(),
                format!("󰻠 {cpu} 󰍛 {memory} 󰋊 {disk}"),
            ),
            (
                "@workbench_cached_metrics_full".into(),
                format!("󰻠 {cpu} 󰍛 {memory} 󰋊 {disk} 󰕒 {up} 󰇚 {down}"),
            ),
        ];
        for source in ["codex", "claude", "trae", "opencode"] {
            let value = if settings.usage {
                let cache = paths.state_dir.join("usage").join(source);
                fs::metadata(&cache)
                    .ok()
                    .filter(|metadata| metadata.len() <= 65_536)
                    .and_then(|_| fs::read_to_string(cache).ok())
                    .and_then(|data| usage_badge(&data))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            values.push((format!("@workbench_cached_usage_{source}"), value));
        }
        let updated = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|time| time.as_secs())
            .unwrap_or(0);
        values.push(("@workbench_status_updated_at".into(), updated.to_string()));
        values.push(("@workbench_status_ready".into(), "1".into()));
        values
    }

    fn refresh_network(&mut self, quiet: bool) {
        let interval = if cfg!(target_os = "macos") && quiet {
            Duration::from_secs(15)
        } else {
            SAMPLE_INTERVAL
        };
        if self
            .network_sampled_at
            .is_some_and(|at| at.elapsed() < interval)
        {
            return;
        }
        let elapsed = self.network_sampled_at.map(|at| at.elapsed());
        self.network_sampled_at = Some(Instant::now());
        #[cfg(target_os = "macos")]
        {
            if let Some((received, transmitted)) = mac_network_rates() {
                self.network_values = (rate(transmitted as f64), rate(received as f64));
            } else {
                self.network_values = ("--".into(), "--".into());
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.networks.refresh(true);
            if let Some(interface) = default_interface() {
                if let Some(data) = self.networks.get(&interface) {
                    let rx = data.total_received();
                    let tx = data.total_transmitted();
                    if let (Some(elapsed), Some((previous, previous_rx, previous_tx))) =
                        (elapsed, &self.network_baseline)
                        && previous == &interface
                        && rx >= *previous_rx
                        && tx >= *previous_tx
                        && elapsed.as_secs_f64() > 0.0
                        && elapsed.as_secs_f64() <= 120.0
                    {
                        let seconds = elapsed.as_secs_f64();
                        self.network_values = (
                            rate((tx - *previous_tx) as f64 / seconds),
                            rate((rx - *previous_rx) as f64 / seconds),
                        );
                    } else {
                        self.network_values = ("--".into(), "--".into());
                    }
                    self.network_baseline = Some((interface, rx, tx));
                } else {
                    self.network_values = ("--".into(), "--".into());
                    self.network_baseline = None;
                }
            } else {
                self.network_values = ("--".into(), "--".into());
                self.network_baseline = None;
            }
        }
        #[cfg(target_os = "macos")]
        let _ = elapsed;
    }
}

fn usage_badge(value: &str) -> Option<String> {
    let daily = value
        .split_whitespace()
        .nth(1)
        .and_then(|field| field.strip_prefix('D'))
        .filter(|field| {
            !field.is_empty()
                && field.len() <= 16
                && field
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || b".KMB".contains(&byte))
        })
        .unwrap_or("0");
    static MATCHER: OnceLock<Regex> = OnceLock::new();
    let matcher =
        MATCHER.get_or_init(|| Regex::new(r"(?:^|\s)([0-9]+)%L").expect("constant regex"));
    // The original shell renderer's greedy match displays the last limit.
    let remaining: u32 = matcher
        .captures_iter(value)
        .last()?
        .get(1)?
        .as_str()
        .parse()
        .ok()?;
    let icon = if remaining >= 88 {
        ''
    } else if remaining >= 63 {
        ''
    } else if remaining >= 35 {
        ''
    } else {
        ''
    };
    Some(format!("  {icon} {remaining}% · T {daily}"))
}

fn rate(bytes_per_second: f64) -> String {
    let value = bytes_per_second / 1_000_000.0;
    if value >= 999.995 {
        ">999MB/s".into()
    } else {
        format!("{value:.2}MB/s")
    }
}

#[cfg(unix)]
fn disk_usage(path: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs receives a valid NUL-terminated path and writable struct.
    let mut stats = unsafe { std::mem::zeroed::<libc::statvfs>() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let used = stats.f_blocks.saturating_sub(stats.f_bfree) as u64;
    let total = used.saturating_add(stats.f_bavail as u64);
    (total > 0).then(|| {
        format!(
            "{}%",
            (used.saturating_mul(100).saturating_add(total - 1)) / total
        )
    })
}

#[cfg(not(unix))]
fn disk_usage(_: &Path) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
fn default_interface() -> Option<String> {
    fs::read_to_string("/proc/net/route")
        .ok()?
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 8 || fields[1] != "00000000" || fields[7] != "00000000" {
                return None;
            }
            Some((fields[6].parse::<u64>().ok()?, fields[0].to_owned()))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, interface)| interface)
}

#[cfg(target_os = "macos")]
fn mac_network_rates() -> Option<(u64, u64)> {
    use std::io::Read;
    let mut child = Command::new("/usr/bin/nettop")
        .args([
            "-n",
            "-P",
            "-L",
            "2",
            "-s",
            "1",
            "-d",
            "-x",
            "-t",
            "external",
            "-J",
            "bytes_in,bytes_out",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let output = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut data = Vec::new();
        let _ = output.take(16 * 1024 * 1024).read_to_end(&mut data);
        data
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => {
                let _ = reader.join();
                return None;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
        }
    }
    let data = reader.join().ok()?;
    parse_nettop_rates(&String::from_utf8_lossy(&data))
}

#[cfg(any(test, target_os = "macos"))]
fn parse_nettop_rates(data: &str) -> Option<(u64, u64)> {
    let mut sample = 0;
    let mut received = 0u64;
    let mut transmitted = 0u64;
    let mut found = false;
    for line in data.lines() {
        let fields = line.split(',').collect::<Vec<_>>();
        if fields.first().is_some_and(|value| value.is_empty()) {
            sample += 1;
            continue;
        }
        if sample == 2 && fields.len() >= 3 {
            if let (Ok(rx), Ok(tx)) = (fields[1].parse::<u64>(), fields[2].parse::<u64>()) {
                received = received.saturating_add(rx);
                transmitted = transmitted.saturating_add(tx);
                found = true;
            }
        }
    }
    found.then_some((received, transmitted))
}

#[cfg(test)]
mod tests {
    use super::{parse_nettop_rates, rate, usage_badge};

    #[test]
    fn badge_matches_the_existing_usage_widget() {
        assert_eq!(
            usage_badge("Cdx D1.2M W3 M4 | Lplus:5h 50%L · reset soon"),
            Some("   50% · T 1.2M".into())
        );
        assert_eq!(usage_badge("OC D1 W2 M3 | L—"), None);
        assert_eq!(
            usage_badge("Cdx D0 W0 M0 | L 95%L"),
            Some("   95% · T 0".into())
        );
        assert_eq!(
            usage_badge("Cdx D1 W2 M3 | L5h 90%L/7d 20%L"),
            Some("   20% · T 1".into())
        );
        assert_eq!(
            usage_badge("Cdx D#{danger} W2 M3 | L 50%L"),
            Some("   50% · T 0".into())
        );
    }

    #[test]
    fn network_rates_keep_existing_units_and_cap() {
        assert_eq!(rate(1_234_567.0), "1.23MB/s");
        assert_eq!(rate(1_000_000_000.0), ">999MB/s");
        assert_eq!(
            parse_nettop_rates(",first\na,1,2\n,second\na,10,20\nb,30,40\n"),
            Some((40, 60))
        );
        assert_eq!(parse_nettop_rates(",first\na,1,2\n"), None);
    }
}
