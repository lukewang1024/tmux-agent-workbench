//! Containers for Sidebar views and compact action menus.
use crate::server::ServerIdentity;
use std::process::Command;

#[derive(Clone, Copy)]
pub(crate) enum Sizing {
    Sidebar,
    Menu { width: u16, height: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dimensions {
    width: u16,
    height: u16,
    border: bool,
}
impl Dimensions {
    fn new(width: u16, height: u16, sizing: Sizing) -> Self {
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
    let shell_quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    let command = format!(
        "WORKBENCH_POPUP=1 WORKBENCH_TARGET_CLIENT={} WORKBENCH_SOURCE_PANE={} TMUX_AGENT_WORKBENCH_TMUX_SOCKET={} {} {}",
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
        &size.width.to_string(),
        "-h",
        &size.height.to_string(),
    ]);
    if !size.border {
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
    let status = popup.arg(command).status()?;
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
