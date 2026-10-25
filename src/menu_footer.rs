//! Navigation labels shared by action menus and shell providers.
use crate::ui_language::text as tr;

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

pub fn close_item(back_closes: bool) -> [String; 3] {
    [
        item_label(tr("× close", "× 关闭"), "Escape"),
        key_name(if back_closes { BACK_KEY } else { '\x1b' }),
        String::new(),
    ]
}

pub(crate) fn item_label(label: &str, key: &str) -> String {
    let hint = match key {
        "Enter" => "enter",
        "BSpace" => "back",
        "Escape" => "esc",
        _ => return label.into(),
    };
    format!("{label} #[align=right]({hint})#[list=on]#[list=left-marker]")
}

/// Shell providers supply display-menu arguments and reuse the same footer.
pub fn display(args: &[String], page: usize) -> Result<(), Box<dyn std::error::Error>> {
    crate::menu::display_legacy(args, page)
}
