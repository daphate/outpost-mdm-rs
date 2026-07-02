//! UI-locale resolution for the admin panel.
//!
//! Russian is the default; a visitor with no `outpost_lang` cookie gets RU.
//! This module resolves the operator's locale from the `outpost_lang` cookie
//! (falling back to `Accept-Language`) and exposes it for the language switcher
//! on the settings page.
//!
//! Note: an earlier compile-time translation table (`Strings` + per-locale
//! literal maps + `WebUser::s()`) was never wired into the Askama templates —
//! they hard-code Russian — so ~1050 lines of dead translation data were
//! removed. The `outpost_lang` cookie is still honoured here. Re-introducing
//! real translations would mean threading `s: &'static Strings` through every
//! template; do that instead of resurrecting the unused map.

use axum::http::header;
use axum::http::request::Parts;

/// Locales we ship. Names are ISO 639-1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    Ru,
    En,
}

impl Locale {
    /// Default for a request with no `outpost_lang` cookie.
    pub const DEFAULT: Locale = Locale::Ru;

    pub fn code(self) -> &'static str {
        match self {
            Locale::Ru => "ru",
            Locale::En => "en",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Locale::Ru => "Русский",
            Locale::En => "English",
        }
    }

    pub fn all() -> &'static [Locale] {
        &[Locale::Ru, Locale::En]
    }
}

pub fn parse_locale(s: &str) -> Option<Locale> {
    match s.trim().to_ascii_lowercase().as_str() {
        "ru" | "ru-ru" | "ru_ru" => Some(Locale::Ru),
        "en" | "en-us" | "en-gb" | "en_us" => Some(Locale::En),
        _ => None,
    }
}

/// Read `outpost_lang` cookie; fall back to `Accept-Language` header;
/// fall back to `Locale::DEFAULT` (Russian).
pub fn from_request(parts: &Parts) -> Locale {
    // 1) explicit cookie
    if let Some(hdr) = parts
        .headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
    {
        for kv in hdr.split(';') {
            if let Some(v) = kv.trim().strip_prefix("outpost_lang=")
                && let Some(loc) = parse_locale(v)
            {
                return loc;
            }
        }
    }
    // 2) Accept-Language
    if let Some(hdr) = parts
        .headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
    {
        // Crude parse — take the first token before ';' or ',', try direct
        // match, then strip the q-factor noise.
        for tag in hdr
            .split(',')
            .map(|s| s.trim().split(';').next().unwrap_or(""))
        {
            if let Some(loc) = parse_locale(tag) {
                return loc;
            }
        }
    }
    Locale::DEFAULT
}
