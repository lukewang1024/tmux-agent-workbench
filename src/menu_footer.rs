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
        tr("× close", "× 关闭").into(),
        key_name(if back_closes { BACK_KEY } else { '\x1b' }),
        String::new(),
    ]
}

/// Shell providers supply display-menu arguments and reuse the same footer.
pub fn display(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    crate::menu::display_legacy(args)
}
