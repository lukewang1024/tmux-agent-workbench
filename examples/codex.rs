fn main() {
    let seconds = std::env::var("WORKBENCH_FIXTURE_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(30);
    std::thread::sleep(std::time::Duration::from_secs(seconds));
}
