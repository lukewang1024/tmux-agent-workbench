//! Shared workbench UI rule: system display language, zh variants or English.
use std::sync::OnceLock;

thread_local! {
    static OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    static CONFIGURED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

pub fn set_language(language: Option<&str>) {
    OVERRIDE.set(language.map(is_chinese));
}

pub fn set_configured_language(language: &str) {
    CONFIGURED.set(language_choice(language));
}

fn chinese() -> bool {
    OVERRIDE.get().unwrap_or_else(|| {
        let environment = std::env::var("TMUX_AGENT_WORKBENCH_LANGUAGE").ok();
        resolve_language(environment.as_deref(), CONFIGURED.get(), system_language)
    })
}

fn language_choice(value: &str) -> Option<bool> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("auto") {
        None
    } else {
        Some(is_chinese(value))
    }
}

fn resolve_language(
    environment: Option<&str>,
    configured: Option<bool>,
    system: impl FnOnce() -> bool,
) -> bool {
    environment
        .and_then(language_choice)
        .or(configured)
        .unwrap_or_else(system)
}

fn system_language() -> bool {
    static CHINESE: OnceLock<bool> = OnceLock::new();
    *CHINESE.get_or_init(detect_system)
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

fn detect_system() -> bool {
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
    fn configured_language_and_fallbacks() {
        let system = || false;
        assert!(resolve_language(None, language_choice("zh-CN"), system));
        assert!(!resolve_language(None, language_choice("en-US"), || true));
        assert!(!resolve_language(None, language_choice("ja-JP"), || true));
        assert!(!resolve_language(None, language_choice("auto"), || false));
        assert!(resolve_language(None, language_choice("auto"), || true));
    }

    #[test]
    fn environment_override_precedes_configured_language() {
        assert!(!resolve_language(
            Some("en"),
            language_choice("zh-CN"),
            || true
        ));
        assert!(resolve_language(
            Some("zh"),
            language_choice("en-US"),
            || false
        ));
        assert!(resolve_language(
            Some("auto"),
            language_choice("zh-CN"),
            || false
        ));
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
