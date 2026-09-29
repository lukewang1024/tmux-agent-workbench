fn main() {
    let seconds = std::env::var("WORKBENCH_FIXTURE_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(30);
    if std::env::var_os("WORKBENCH_FIXTURE_TRANSITION").is_some() {
        use std::io::Write;

        let mut stdout = std::io::stdout().lock();
        write!(
            stdout,
            "\n› Ask Codex to do anything\n  GPT-6-Sol high · ~\n"
        )
        .unwrap();
        stdout.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(4));
        // Redraw above the composer and restore the cursor. The pane title,
        // cursor, and history size stay unchanged, as they can in Codex.
        write!(stdout, "\x1b7\x1b[3A\rWorking (1s • esc to interrupt)\x1b8").unwrap();
        stdout.flush().unwrap();
    }
    std::thread::sleep(std::time::Duration::from_secs(seconds));
}
