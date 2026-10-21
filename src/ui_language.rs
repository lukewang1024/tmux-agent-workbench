//! Shared workbench UI rule: system display language, zh variants or English.
use std::sync::OnceLock;

thread_local! { static OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) }; }

pub fn set_language(language: Option<&str>) {
    OVERRIDE.set(language.map(is_chinese));
}

fn chinese() -> bool {
    static CHINESE: OnceLock<bool> = OnceLock::new();
    OVERRIDE
        .get()
        .unwrap_or_else(|| *CHINESE.get_or_init(detect))
}

pub fn name() -> &'static str {
    if chinese() { "zh" } else { "en" }
}

pub fn text(english: &'static str, chinese_text: &'static str) -> &'static str {
    if chinese() { chinese_text } else { english }
}

fn is_chinese(value: &str) -> bool {
    value
        .split(['-', '_', '.', ':'])
        .next()
        .is_some_and(|lang| lang.eq_ignore_ascii_case("zh"))
}

fn detect() -> bool {
    // Explicit override is useful for remote terminals and deterministic tests.
    if let Ok(value) = std::env::var("TMUX_AGENT_WORKBENCH_LANGUAGE") {
        if !value.is_empty() {
            return is_chinese(&value);
        }
    }
    #[cfg(target_os = "macos")]
    {
        preferred_language()
    }
    #[cfg(not(target_os = "macos"))]
    {
        locale_language(|key| std::env::var(key).ok())
    }
}

#[cfg(any(not(target_os = "macos"), test))]
fn locale_language(env: impl Fn(&str) -> Option<String>) -> bool {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|key| env(key).filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "C".into());
    if matches!(locale.as_str(), "C" | "POSIX" | "C.UTF-8" | "C.utf8") {
        return false;
    }
    is_chinese(
        &env("LANGUAGE")
            .filter(|value| !value.is_empty())
            .unwrap_or(locale),
    )
}

#[cfg(target_os = "macos")]
fn preferred_language() -> bool {
    use std::ffi::{CStr, c_char, c_void};
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFLocaleCopyPreferredLanguages() -> *const c_void;
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(value: *const c_void);
    }
    // Copy owns the array; the borrowed string remains valid until release.
    unsafe {
        let languages = CFLocaleCopyPreferredLanguages();
        if languages.is_null() {
            return false;
        }
        let mut buffer = [0 as c_char; 128];
        let valid = CFArrayGetCount(languages) > 0
            && CFStringGetCString(
                CFArrayGetValueAtIndex(languages, 0),
                buffer.as_mut_ptr(),
                buffer.len() as isize,
                0x08000100,
            ) != 0;
        let chinese = valid && is_chinese(&CStr::from_ptr(buffer.as_ptr()).to_string_lossy());
        CFRelease(languages);
        chinese
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chinese_variants_and_english_fallback() {
        for value in ["zh", "ZH", "zh_CN.UTF-8", "zh-Hans-HK", "zh-TW"] {
            assert!(is_chinese(value));
        }
        for value in ["en-US", "ja-JP", "C", "", "zho"] {
            assert!(!is_chinese(value));
        }
    }
    #[test]
    fn standard_message_locale_precedence() {
        let check = |pairs: &[(&str, &str)]| {
            locale_language(|key| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, value)| value.to_string())
            })
        };
        assert!(check(&[
            ("LANG", "en_US.UTF-8"),
            ("LC_MESSAGES", "zh_CN.UTF-8")
        ]));
        assert!(!check(&[
            ("LANG", "zh_CN.UTF-8"),
            ("LC_ALL", "C"),
            ("LANGUAGE", "zh")
        ]));
        assert!(check(&[("LANG", "en_US.UTF-8"), ("LANGUAGE", "zh:en")]));
        assert!(!check(&[]));
    }
}
