//! Navigation shared by every native tmux menu, including shell providers.
use crate::ui_language::text as tr;
use std::process::Command;

pub const BACK_KEY: char = '\x7f';

pub fn back_label() -> &'static str {
    tr("← Back", "← 返回")
}

pub fn key_name(key: char) -> String {
    match key {
        '\r' => "Enter".into(),
        BACK_KEY => "BSpace".into(),
        '\x1b' => "Escape".into(),
        key => key.to_string(),
    }
}

pub fn item_label(label: &str, key: char) -> String {
    let hint = match key {
        '\r' => "enter",
        BACK_KEY => "back",
        '\x1b' => "esc",
        _ => return label.into(),
    };
    // Keep the native binding, but draw our own right-aligned keycap label.
    // tmux appends its canonical key name; place that suffix in the unused
    // trim marker of an empty list so it cannot appear beside the short hint.
    format!("{label} #[align=right]({hint})#[list=on]#[list=left-marker]")
}

pub fn close_item(back_closes: bool) -> [String; 3] {
    // Escape always closes natively. At the root, also bind Backspace to this
    // row without displaying another hint or adding a row. Submenus reserve
    // Backspace for their parent action. Clicking Close runs an empty command.
    [
        item_label(tr("× close", "× 关闭"), '\x1b'),
        key_name(if back_closes { BACK_KEY } else { '\x1b' }),
        String::new(),
    ]
}

/// Shell providers supply display-menu arguments and reuse the same footer.
pub fn display(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("tmux")
        .arg("display-menu")
        .args(args)
        .arg("")
        .args(close_item(true))
        .status()?;
    match status.code() {
        Some(0 | 2) => Ok(()),
        _ => Err("could not display menu".into()),
    }
}
