//! Containers for Sidebar views and compact action menus.
use crate::server::ServerIdentity;
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub(crate) enum Sizing {
    Sidebar,
    Menu { width: u16, height: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Dimensions {
    pub width: u16,
    pub height: u16,
    pub border: bool,
}
impl Dimensions {
    pub fn new(width: u16, height: u16, sizing: Sizing) -> Self {
        let border = width >= 36 && height >= 12;
        let (requested_width, requested_height) = match sizing {
            Sizing::Sidebar if border => (
                ((u32::from(width) * 9 / 10) as u16).clamp(32, 96),
                ((u32::from(height) * 9 / 10) as u16).clamp(12, 40),
            ),
            Sizing::Sidebar => (width, height),
            Sizing::Menu {
                width: preferred_width,
                height: preferred_height,
            } => {
                let margin = if border { 2 } else { 0 };
                (
                    preferred_width
                        .saturating_add(margin)
                        .clamp(28, 60)
                        .min(width.saturating_sub(if border { 4 } else { 0 })),
                    preferred_height
                        .saturating_add(margin)
                        .max(6)
                        .min(height.saturating_sub(if border { 2 } else { 0 }))
                        .min(32),
                )
            }
        };
        Self {
            width: requested_width.min(width),
            height: requested_height.min(height),
            border,
        }
    }
}

pub(crate) fn open(
    server: &ServerIdentity,
    pane: &str,
    client: &str,
    args: &[&str],
    sizing: Sizing,
) -> Result<(), Box<dyn std::error::Error>> {
    if !pane
        .strip_prefix('%')
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err("invalid source pane".into());
    }
    let dimensions = crate::tmux::Tmux::new(server.clone())
        .client_format(client, "#{client_width} #{client_height}")?
        .ok_or("navigation client disappeared")?;
    let (width, height) = dimensions.split_once(' ').ok_or("invalid client size")?;
    let size = Dimensions::new(width.parse()?, height.parse()?, sizing);
    // tmux discards mouse events outside a display-popup. Action menus use a
    // client-local input surface with the same bounded card drawn inside it.
    // The surface consumes the closing click instead of forwarding it to the
    // underlying terminal. Sidebar panes/popups keep their existing container.
    let surface = match sizing {
        Sizing::Menu { width, height } => format!("WORKBENCH_MENU_SURFACE={width},{height} "),
        Sizing::Sidebar => String::new(),
    };
    let shell_quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    let command = format!(
        "{surface}WORKBENCH_POPUP=1 WORKBENCH_TARGET_CLIENT={} WORKBENCH_SOURCE_PANE={} TMUX_AGENT_WORKBENCH_TMUX_SOCKET={} {} {}",
        shell_quote(client),
        shell_quote(pane),
        shell_quote(&server.socket_path.to_string_lossy()),
        shell_quote(&std::env::current_exe()?.to_string_lossy()),
        args.iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let mut popup = Command::new("tmux");
    popup.args(server.tmux_args()).args([
        "display-popup",
        "-E",
        "-c",
        client,
        "-t",
        pane,
        "-x",
        "C",
        "-y",
        "C",
        "-w",
        &if surface.is_empty() {
            size.width.to_string()
        } else {
            width.into()
        },
        "-h",
        &if surface.is_empty() {
            size.height.to_string()
        } else {
            height.into()
        },
    ]);
    if !size.border || !surface.is_empty() {
        popup.arg("-B");
    }
    for variable in [
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
    ] {
        if let Some(value) = std::env::var_os(variable) {
            let mut entry = std::ffi::OsString::from(format!("{variable}="));
            entry.push(value);
            popup.args([std::ffi::OsString::from("-e"), entry]);
        }
    }
    let status = if surface.is_empty() {
        popup.arg(command).status()?
    } else {
        let mut child = popup.arg(command).spawn()?;
        let mut next_size_check = Instant::now() + Duration::from_millis(200);
        loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= next_size_check {
                next_size_check = Instant::now() + Duration::from_millis(200);
                let current = crate::tmux::Tmux::new(server.clone())
                    .client_format(client, "#{client_width} #{client_height}")?;
                if current.as_deref() != Some(dimensions.as_str()) {
                    // Context menus dismiss on viewport changes. In particular,
                    // tmux cannot grow a popup beyond its initial size, so a
                    // stale input surface must not leave uncaptured margins.
                    let _ = Command::new("tmux")
                        .args(server.tmux_args())
                        .args(["display-popup", "-C", "-c", client])
                        .output();
                    break child.wait()?;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    if !status.success() && !matches!(status.code(), Some(129 | 130)) {
        return Err("could not open navigation panel".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_popup_preserves_context_until_space_is_too_small() {
        for (width, height) in [(40, 20), (63, 31), (160, 50)] {
            let size = Dimensions::new(width, height, Sizing::Sidebar);
            assert!(size.border);
            assert!(size.width < width && size.height < height);
            assert!(size.width <= 96 && size.height <= 40);
        }
        for (width, height) in [(32, 12), (24, 8), (80, 10)] {
            assert_eq!(
                Dimensions::new(width, height, Sizing::Sidebar),
                Dimensions {
                    width,
                    height,
                    border: false,
                }
            );
        }
    }

    #[test]
    fn action_menus_fit_their_content_and_keep_a_margin() {
        let preferred = Sizing::Menu {
            width: 30,
            height: 13,
        };
        let menu = Dimensions::new(160, 50, preferred);
        let sidebar = Dimensions::new(160, 50, Sizing::Sidebar);
        assert!(menu.width < sidebar.width && menu.height < sidebar.height);
        for (width, height) in [(40, 20), (36, 12), (32, 12), (24, 8), (1, 1)] {
            let menu = Dimensions::new(width, height, preferred);
            assert!(menu.width > 0 && menu.width <= width);
            assert!(menu.height > 0 && menu.height <= height);
            if menu.border {
                assert!(menu.width + 4 <= width && menu.height + 2 <= height);
            }
        }
    }
}
