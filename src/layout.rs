use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::process::Command;

use fs2::FileExt;

use crate::config::{Config, SidebarPosition};
use crate::paths::Paths;
use crate::server::ServerIdentity;

pub enum Action {
    Configure,
    Toggle,
    ToggleAll,
    EnsureAll,
    Maintain,
    Remember,
}

pub fn control(
    action: Action,
    target: Option<&str>,
    create_only: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let server = ServerIdentity::discover()?;
    let paths = Paths::discover()?;
    seed_layout_options(&server, &Config::load(&paths.config_file())?)?;
    let lock_path = paths
        .runtime_dir
        .join(format!("sidebar-create-{}.lock", server.key));
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    lock.lock_exclusive()?;
    let result = match action {
        Action::Configure => Ok(()),
        Action::Toggle => toggle(
            &server,
            target.ok_or("window target required")?,
            create_only,
        ),
        Action::ToggleAll => toggle_all(&server),
        Action::EnsureAll => ensure_all(&server),
        Action::Maintain => maintain(&server, target.ok_or("window target required")?),
        Action::Remember => remember(&server, target.ok_or("pane target required")?),
    };
    lock.unlock()?;
    result
}

fn seed_layout_options(
    server: &ServerIdentity,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let sidebar = &config.sidebar;
    for (name, value) in [
        ("@sidebar_width", sidebar.width.to_string()),
        ("@sidebar_min_width", sidebar.min_width.to_string()),
        ("@sidebar_max_width", sidebar.max_width.to_string()),
        (
            "@sidebar_main_min_width",
            sidebar.main_min_width.to_string(),
        ),
        (
            "@sidebar_position",
            match sidebar.position {
                SidebarPosition::Left => "left",
                SidebarPosition::Right => "right",
            }
            .to_owned(),
        ),
        (
            "@sidebar_auto_create",
            if sidebar.auto_create { "on" } else { "off" }.to_owned(),
        ),
    ] {
        let marker = format!("@workbench_seeded_{}", name.trim_start_matches('@'));
        let current = option(server, name).unwrap_or_default().trim().to_owned();
        let previous_seed = option(server, &marker)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if current.is_empty() || (!previous_seed.is_empty() && current == previous_seed) {
            tmux(server, &["set-option", "-g", name, &value])?;
            tmux(server, &["set-option", "-g", &marker, &value])?;
        }
    }
    Ok(())
}

fn toggle(
    server: &ServerIdentity,
    window: &str,
    create_only: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_target(window, '@')?;
    refuse_old_sidebar(server)?;
    let existing = sidebar_panes(server, window)?;
    if !existing.is_empty() {
        for duplicate in existing.iter().skip(1) {
            tmux(server, &["kill-pane", "-t", duplicate])?;
        }
        if !create_only {
            close_sidebar(server, window, &existing[0])?;
            tmux(
                server,
                &[
                    "set-option",
                    "-w",
                    "-t",
                    window,
                    "@workbench_sidebar_disabled",
                    "1",
                ],
            )?;
            let _ = tmux(
                server,
                &[
                    "set-option",
                    "-wu",
                    "-t",
                    window,
                    "@workbench_sidebar_auto_hidden",
                ],
            );
        }
        return Ok(());
    }
    if create_only
        && option(server, "@sidebar_auto_create")?.trim() == "off"
        && display(server, window, "#{@workbench_sidebar_auto_hidden}")?.trim() != "1"
    {
        return Ok(());
    }
    create(server, window, create_only)
}

fn toggle_all(server: &ServerIdentity) -> Result<(), Box<dyn std::error::Error>> {
    refuse_old_sidebar(server)?;
    let output = tmux(server, &["list-windows", "-a", "-F", "#{window_id}"])?;
    let windows: Vec<_> = output
        .lines()
        .filter(|line| valid_target(line, '@'))
        .collect();
    let create = windows
        .iter()
        .any(|window| sidebar_panes(server, window).is_ok_and(|panes| panes.is_empty()));
    for window in windows {
        if create {
            toggle(server, window, true)?;
        } else {
            for pane in sidebar_panes(server, window)? {
                close_sidebar(server, window, &pane)?;
            }
            tmux(
                server,
                &[
                    "set-option",
                    "-w",
                    "-t",
                    window,
                    "@workbench_sidebar_disabled",
                    "1",
                ],
            )?;
        }
    }
    Ok(())
}

fn ensure_all(server: &ServerIdentity) -> Result<(), Box<dyn std::error::Error>> {
    refuse_old_sidebar(server)?;
    if option(server, "@sidebar_auto_create")?.trim() == "off" {
        return Ok(());
    }
    let output = tmux(server, &["list-windows", "-a", "-F", "#{window_id}"])?;
    for window in output.lines().filter(|line| valid_target(line, '@')) {
        let disabled = display(server, window, "#{@workbench_sidebar_disabled}")?;
        if disabled.trim() != "1" {
            toggle(server, window, true)?;
            maintain(server, window)?;
        }
    }
    Ok(())
}

fn create(
    server: &ServerIdentity,
    window: &str,
    auto: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let window_width = display(server, window, "#{window_width}")?
        .trim()
        .parse::<u16>()
        .unwrap_or(0);
    let width = option_u16(server, "@sidebar_width", 26).clamp(
        option_u16(server, "@sidebar_min_width", 18),
        option_u16(server, "@sidebar_max_width", 36).min(64),
    );
    let main_min = option_u16(server, "@sidebar_main_min_width", 80);
    debug(&format!(
        "create window={window} window_width={window_width} width={width} main_min={main_min} auto={auto}"
    ));
    if window_width < width.saturating_add(main_min).saturating_add(1) {
        debug("responsive gate skipped sidebar");
        if auto {
            tmux(
                server,
                &[
                    "set-option",
                    "-w",
                    "-t",
                    window,
                    "@workbench_sidebar_auto_hidden",
                    "1",
                ],
            )?;
        }
        return Ok(());
    }
    let active = display(server, window, "#{pane_id}")?;
    let main_columns = main_columns(server, window)?;
    let main_layout = display(server, window, "#{window_layout}")?;
    tmux(
        server,
        &[
            "set-option",
            "-w",
            "-t",
            window,
            "@workbench_main_layout",
            main_layout.trim(),
        ],
    )?;
    let position = option(server, "@sidebar_position")?;
    let executable = std::env::current_exe()?.display().to_string();
    let width_string = width.to_string();
    let mut args = vec!["split-window", "-h", "-f"];
    if position.trim() != "right" {
        args.push("-b");
    }
    args.extend([
        "-l",
        &width_string,
        "-t",
        window,
        "-P",
        "-F",
        "#{pane_id}",
        &executable,
        "sidebar",
    ]);
    let pane = tmux(server, &args)?.trim().to_owned();
    debug(&format!("split returned pane={pane:?}"));
    if valid_target(&pane, '%') {
        restore_main_column_ratios(server, window_width, width, &main_columns)?;
        freeze_window_name(server, window)?;
        tmux(
            server,
            &["set-option", "-p", "-t", &pane, "@pane_role", "sidebar"],
        )?;
        let _ = tmux(
            server,
            &[
                "set-option",
                "-wu",
                "-t",
                window,
                "@workbench_sidebar_auto_hidden",
            ],
        );
        let _ = tmux(
            server,
            &[
                "set-option",
                "-wu",
                "-t",
                window,
                "@workbench_sidebar_disabled",
            ],
        );
    }
    if valid_target(active.trim(), '%') {
        tmux(server, &["select-pane", "-Z", "-t", active.trim()])?;
    }
    Ok(())
}

fn main_columns(
    server: &ServerIdentity,
    window: &str,
) -> Result<Vec<(String, u16)>, Box<dyn std::error::Error>> {
    let output = tmux(
        server,
        &[
            "list-panes",
            "-t",
            window,
            "-F",
            "#{pane_left}\u{1f}#{pane_id}\u{1f}#{pane_width}",
        ],
    )?;
    let mut columns = BTreeMap::<u16, (String, u16)>::new();
    for line in output.lines() {
        let fields: Vec<_> = line.split('\u{1f}').collect();
        if fields.len() != 3 || !valid_target(fields[1], '%') {
            continue;
        }
        let Ok(left) = fields[0].parse::<u16>() else {
            continue;
        };
        let Ok(width) = fields[2].parse::<u16>() else {
            continue;
        };
        columns
            .entry(left)
            .and_modify(|entry| entry.1 = entry.1.max(width))
            .or_insert_with(|| (fields[1].to_owned(), width));
    }
    Ok(columns.into_values().collect())
}

fn restore_main_column_ratios(
    server: &ServerIdentity,
    window_width: u16,
    sidebar_width: u16,
    columns: &[(String, u16)],
) -> Result<(), Box<dyn std::error::Error>> {
    if columns.len() < 2 {
        return Ok(());
    }
    let source_total: u32 = columns.iter().map(|(_, width)| u32::from(*width)).sum();
    let target_total = window_width
        .saturating_sub(sidebar_width)
        .saturating_sub(columns.len() as u16);
    if source_total == 0 || target_total == 0 {
        return Ok(());
    }
    let mut targets = Vec::with_capacity(columns.len());
    let mut assigned = 0_u16;
    for (index, (_, old_width)) in columns.iter().enumerate() {
        let width = if index + 1 == columns.len() {
            target_total.saturating_sub(assigned).max(1)
        } else {
            let remaining_columns = (columns.len() - index - 1) as u16;
            let scaled = ((u32::from(*old_width) * u32::from(target_total) + source_total / 2)
                / source_total) as u16;
            scaled
                .max(1)
                .min(target_total.saturating_sub(assigned + remaining_columns))
        };
        targets.push(width);
        assigned = assigned.saturating_add(width);
    }
    // Work from the right edge inward. Resizing the leftmost main pane moves
    // the sidebar boundary and makes its width drift on every toggle.
    for index in (1..columns.len()).rev() {
        tmux(
            server,
            &[
                "resize-pane",
                "-t",
                &columns[index].0,
                "-x",
                &targets[index].to_string(),
            ],
        )?;
    }
    Ok(())
}

fn maintain(server: &ServerIdentity, window: &str) -> Result<(), Box<dyn std::error::Error>> {
    validate_target(window, '@')?;
    if old_sidebar_loaded(server) {
        return Ok(());
    }
    let mut zoomed_pane = if display(server, window, "#{window_zoomed_flag}")?.trim() == "1" {
        let pane = display(server, window, "#{pane_id}")?;
        valid_target(pane.trim(), '%').then(|| pane.trim().to_owned())
    } else {
        None
    };
    let width = option_u16(server, "@sidebar_width", 26).min(64);
    let main_min = option_u16(server, "@sidebar_main_min_width", 80);
    let window_width = display(server, window, "#{window_width}")?
        .trim()
        .parse::<u16>()
        .unwrap_or(0);
    let rebuilding_sidebar = sidebar_panes(server, window)?.is_empty()
        && window_width >= width.saturating_add(main_min).saturating_add(1);
    if rebuilding_sidebar {
        if let Some(pane) = zoomed_pane.as_deref() {
            // list-panes reports the zoomed pane at the full viewport width.
            // Reveal the underlying layout before create() snapshots column
            // ratios, otherwise the active pane becomes permanently larger.
            tmux(server, &["resize-pane", "-Z", "-t", pane])?;
        }
        if display(server, window, "#{@responsive_auto_zoom}")?.trim() == "1" {
            // The following responsive hook owns this zoom and is about to
            // release it on the wide viewport; do not briefly restore it.
            zoomed_pane = None;
        }
    }
    let result = maintain_layout(server, window);
    let restore_result = restore_zoom(server, window, zoomed_pane.as_deref());
    result.and(restore_result)
}

fn maintain_layout(
    server: &ServerIdentity,
    window: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let panes = sidebar_panes(server, window)?;
    let pane_count = tmux(server, &["list-panes", "-t", window, "-F", "#{pane_id}"])?
        .lines()
        .count();
    if pane_count == 1 && panes.len() == 1 {
        tmux(server, &["kill-window", "-t", window])?;
        return Ok(());
    }
    let width = option_u16(server, "@sidebar_width", 26).min(64);
    let main_min = option_u16(server, "@sidebar_main_min_width", 80);
    let window_width = display(server, window, "#{window_width}")?
        .trim()
        .parse::<u16>()
        .unwrap_or(0);
    if window_width < width.saturating_add(main_min).saturating_add(1) {
        if let Some(pane) = panes.first() {
            tmux(
                server,
                &[
                    "set-option",
                    "-w",
                    "-t",
                    window,
                    "@workbench_sidebar_auto_hidden",
                    "1",
                ],
            )?;
            close_sidebar(server, window, pane)?;
        }
    } else if panes.is_empty() {
        // aggressive-resize lets an unobserved window grow back to the large
        // server size. Do not recreate its sidebar in the background only to
        // destroy it when a narrow client returns. The client/session-change
        // hooks call maintain synchronously when the window is actually shown.
        if !window_has_client(server, window)? {
            return Ok(());
        }
        let auto_hidden =
            display(server, window, "#{@workbench_sidebar_auto_hidden}")?.trim() == "1";
        let disabled = display(server, window, "#{@workbench_sidebar_disabled}")?.trim() == "1";
        let auto_create = option(server, "@sidebar_auto_create")?.trim() != "off";
        if !disabled && (auto_hidden || auto_create) {
            create(server, window, true)?;
        }
    } else if let Some(pane) = panes.first() {
        tmux(
            server,
            &["resize-pane", "-t", pane, "-x", &width.to_string()],
        )?;
    }
    Ok(())
}

fn restore_zoom(
    server: &ServerIdentity,
    window: &str,
    zoomed_pane: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(original_pane) = zoomed_pane else {
        return Ok(());
    };
    if display(server, window, "#{window_zoomed_flag}")
        .map(|value| value.trim() == "1")
        .unwrap_or(false)
    {
        return Ok(());
    }
    // Removing or adding the sidebar makes tmux leave zoom. Restore the pane
    // that was zoomed before maintenance; if that pane was the sidebar and was
    // removed, zoom the window's new active pane instead.
    let target = display(server, original_pane, "#{pane_id}")
        .ok()
        .filter(|pane| valid_target(pane.trim(), '%'))
        .map(|pane| pane.trim().to_owned())
        .or_else(|| {
            display(server, window, "#{pane_id}")
                .ok()
                .filter(|pane| valid_target(pane.trim(), '%'))
                .map(|pane| pane.trim().to_owned())
        });
    if let Some(target) = target {
        tmux(server, &["resize-pane", "-Z", "-t", &target])?;
    }
    Ok(())
}

fn window_has_client(
    server: &ServerIdentity,
    window: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let output = tmux(server, &["list-clients", "-F", "#{window_id}"])?;
    Ok(output.lines().any(|visible| visible == window))
}

fn freeze_window_name(
    server: &ServerIdentity,
    window: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let saved = display(server, window, "#{@workbench_saved_automatic_rename}")?;
    if saved.trim().is_empty() {
        let automatic = display(server, window, "#{automatic-rename}")?;
        tmux(
            server,
            &[
                "set-option",
                "-w",
                "-t",
                window,
                "@workbench_saved_automatic_rename",
                if matches!(automatic.trim(), "1" | "on") {
                    "on"
                } else {
                    "off"
                },
            ],
        )?;
    }
    tmux(
        server,
        &["set-option", "-w", "-t", window, "automatic-rename", "off"],
    )?;
    Ok(())
}

fn close_sidebar(
    server: &ServerIdentity,
    window: &str,
    pane: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    tmux(server, &["kill-pane", "-t", pane])?;
    let layout = display(server, window, "#{@workbench_main_layout}")?;
    if !layout.trim().is_empty() {
        // The main pane set may have changed while the sidebar was open. tmux
        // then rejects the saved layout because its pane count is stale. The
        // sidebar is already gone and the current layout is valid, so restoring
        // the old proportions is deliberately best-effort.
        let _ = tmux(server, &["select-layout", "-t", window, layout.trim()]);
        let _ = tmux(
            server,
            &["set-option", "-wu", "-t", window, "@workbench_main_layout"],
        );
    }
    let automatic = display(server, window, "#{@workbench_saved_automatic_rename}")?;
    if !automatic.trim().is_empty() {
        tmux(
            server,
            &[
                "set-option",
                "-w",
                "-t",
                window,
                "automatic-rename",
                automatic.trim(),
            ],
        )?;
        let _ = tmux(
            server,
            &[
                "set-option",
                "-wu",
                "-t",
                window,
                "@workbench_saved_automatic_rename",
            ],
        );
    }
    Ok(())
}

fn remember(server: &ServerIdentity, pane: &str) -> Result<(), Box<dyn std::error::Error>> {
    validate_target(pane, '%')?;
    let pane = if display(server, pane, "#{@pane_role}")?.trim() == "sidebar" {
        pane.to_owned()
    } else {
        let window = display(server, pane, "#{window_id}")?;
        let Some(sidebar) = sidebar_panes(server, window.trim())?.into_iter().next() else {
            return Ok(());
        };
        sidebar
    };
    let requested = display(server, &pane, "#{pane_width}")?
        .trim()
        .parse::<u16>()
        .unwrap_or(26);
    let min = option_u16(server, "@sidebar_min_width", 18);
    let max = option_u16(server, "@sidebar_max_width", 36).min(64);
    let main_min = option_u16(server, "@sidebar_main_min_width", 80);
    let saved = option_u16(server, "@sidebar_width", 26);
    let mut effective_max = max;
    let mut all_at_saved = true;
    let output = tmux(
        server,
        &[
            "list-panes",
            "-a",
            "-f",
            "#{==:#{@pane_role},sidebar}",
            "-F",
            "#{pane_id}\u{1f}#{pane_width}\u{1f}#{window_width}",
        ],
    )?;
    let mut sidebars = Vec::new();
    for line in output.lines() {
        let mut fields = line.split('\u{1f}');
        let Some(sidebar) = fields.next() else {
            continue;
        };
        let Some(pane_width) = fields.next().and_then(|value| value.parse::<u16>().ok()) else {
            continue;
        };
        let Some(window_width) = fields.next().and_then(|value| value.parse::<u16>().ok()) else {
            continue;
        };
        if !valid_target(sidebar, '%') {
            continue;
        }
        all_at_saved &= pane_width == saved;
        // One column belongs to tmux's separator between the sidebar and the
        // main area. Existing sidebars should converge on the widest value
        // that still leaves every main area at its configured minimum.
        effective_max = effective_max.min(window_width.saturating_sub(main_min.saturating_add(1)));
        sidebars.push(sidebar.to_owned());
    }
    effective_max = effective_max.max(min);
    let width = requested.clamp(min, effective_max);
    if width == saved && requested == saved && all_at_saved {
        return Ok(());
    }
    tmux(
        server,
        &["set-option", "-g", "@sidebar_width", &width.to_string()],
    )?;
    for sidebar in sidebars {
        tmux(
            server,
            &["resize-pane", "-t", &sidebar, "-x", &width.to_string()],
        )?;
    }
    Ok(())
}

fn sidebar_panes(
    server: &ServerIdentity,
    window: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let output = tmux(
        server,
        &[
            "list-panes",
            "-t",
            window,
            "-F",
            "#{pane_id}\u{1f}#{@pane_role}",
        ],
    )?;
    Ok(output
        .lines()
        .filter_map(|line| {
            let (pane, role) = line.split_once('\u{1f}')?;
            (role == "sidebar" && valid_target(pane, '%')).then(|| pane.to_owned())
        })
        .collect())
}

fn refuse_old_sidebar(server: &ServerIdentity) -> Result<(), Box<dyn std::error::Error>> {
    if old_sidebar_loaded(server) {
        Err("legacy tmux-agent-sidebar is loaded; remove it before enabling the Workbench v2 sidebar".into())
    } else {
        Ok(())
    }
}

fn old_sidebar_loaded(server: &ServerIdentity) -> bool {
    option(server, "@agent_sidebar_bin").is_ok_and(|value| !value.trim().is_empty())
}

fn option(server: &ServerIdentity, name: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(tmux(server, &["show-option", "-gqv", name])?)
}

fn option_u16(server: &ServerIdentity, name: &str, fallback: u16) -> u16 {
    option(server, name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(fallback)
}

fn display(
    server: &ServerIdentity,
    target: &str,
    format: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let value = tmux(server, &["display-message", "-p", "-t", target, format])?;
    debug(&format!(
        "display socket={} target={target} format={format} value={value:?}",
        server.socket_path.display()
    ));
    Ok(value)
}

fn tmux(server: &ServerIdentity, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8(output.stdout)?)
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_owned()
            .into())
    }
}

fn validate_target(value: &str, prefix: char) -> Result<(), Box<dyn std::error::Error>> {
    if valid_target(value, prefix) {
        Ok(())
    } else {
        Err(format!("invalid tmux target: {value}").into())
    }
}

fn valid_target(value: &str, prefix: char) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
}

fn debug(message: &str) {
    if std::env::var_os("TMUX_AGENT_WORKBENCH_DEBUG").is_some() {
        eprintln!("tmux-agent-workbench layout: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn targets_reject_command_injection() {
        assert!(valid_target("@12", '@'));
        assert!(!valid_target("@12;run-shell", '@'));
    }
}
