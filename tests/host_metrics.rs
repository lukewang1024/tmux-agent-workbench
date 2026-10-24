#[cfg(target_os = "macos")]
#[test]
fn cpu_counter_command_does_not_initialize_workbench_files() {
    let root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tmux-agent-workbench"))
        .arg("host-cpu-counters")
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ticks: Vec<u64> = String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .map(|value| value.parse().unwrap())
        .collect();
    assert_eq!(ticks.len(), 2);
    assert!(ticks[0] > 0 && ticks[1] <= ticks[0]);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
