//! # Internationalization (i18n)
//!
//! Locale routing and translation primitives for Krab apps.
//!
//! ## Usage
//!
//! ```rust
//! use krab_core::i18n::{I18n, Locale, TranslationBundle};
//!
//! let mut bundle = TranslationBundle::new();
//! bundle.add_locale(Locale::new("en", "English"), vec![
//!     ("greeting", "Hello"),
//!     ("farewell", "Goodbye"),
//! ]);
//! bundle.add_locale(Locale::new("ne", "नेपाली"), vec![
//!     ("greeting", "नमस्ते"),
//!     ("farewell", "बिदा"),
//! ]);
//!
//! let i18n = I18n::new(bundle, "en");
//! assert_eq!(i18n.t("greeting"), "Hello");
//!
//! let i18n_ne = i18n.with_locale("ne");
//! assert_eq!(i18n_ne.t("greeting"), "नमस्ते");
//! ```

use std::collections::HashMap;

// ── Locale ──────────────────────────────────────────────────────────────────

/// Represents a supported locale.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Locale {
    /// BCP 47 language tag (e.g., "en", "ne", "ja").
    pub code: String,
    /// Human-readable name (e.g., "English", "नेपाली").
    pub display_name: String,
}

impl Locale {
    /// Create a new locale.
    pub fn new(code: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            display_name: display_name.into(),
        }
    }
}

// ── Translation Bundle ──────────────────────────────────────────────────────

/// Holds all translations for all supported locales.
#[derive(Debug, Clone, Default)]
pub struct TranslationBundle {
    /// locale_code -> { key -> translated_string }
    translations: HashMap<String, HashMap<String, String>>,
    /// All registered locales.
    locales: Vec<Locale>,
}

impl TranslationBundle {
    /// Create an empty bundle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a locale with its translations.
    pub fn add_locale(&mut self, locale: Locale, entries: Vec<(&str, &str)>) {
        let map: HashMap<String, String> = entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        self.translations.insert(locale.code.clone(), map);
        if !self.locales.iter().any(|l| l.code == locale.code) {
            self.locales.push(locale);
        }
    }

    /// Add translations from a JSON string: `{ "key": "value", ... }`.
    #[cfg(any(feature = "rest", feature = "db"))]
    pub fn add_locale_json(&mut self, locale: Locale, json: &str) -> Result<(), String> {
        let map: HashMap<String, String> =
            serde_json::from_str(json).map_err(|e| format!("Invalid i18n JSON: {}", e))?;
        self.translations.insert(locale.code.clone(), map);
        if !self.locales.iter().any(|l| l.code == locale.code) {
            self.locales.push(locale);
        }
        Ok(())
    }

    /// Get a translation for a key in a given locale.
    pub fn get(&self, locale: &str, key: &str) -> Option<&str> {
        self.translations
            .get(locale)
            .and_then(|m| m.get(key))
            .map(|s| s.as_str())
    }

    /// Get all supported locales.
    pub fn supported_locales(&self) -> &[Locale] {
        &self.locales
    }

    /// Get all translation keys for a locale.
    pub fn keys(&self, locale: &str) -> Vec<&str> {
        self.translations
            .get(locale)
            .map(|m| m.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default()
    }

    /// Check if a locale is supported.
    pub fn has_locale(&self, code: &str) -> bool {
        self.translations.contains_key(code)
    }
}

// ── I18n Context ────────────────────────────────────────────────────────────

/// Active i18n context bound to a specific locale.
#[derive(Debug, Clone)]
pub struct I18n {
    bundle: TranslationBundle,
    current_locale: String,
    fallback_locale: String,
}

impl I18n {
    /// Create a new i18n context with the given bundle and default locale.
    pub fn new(bundle: TranslationBundle, default_locale: impl Into<String>) -> Self {
        let locale = default_locale.into();
        Self {
            bundle,
            current_locale: locale.clone(),
            fallback_locale: locale,
        }
    }

    /// Set a fallback locale for missing translations.
    pub fn with_fallback(mut self, fallback: impl Into<String>) -> Self {
        self.fallback_locale = fallback.into();
        self
    }

    /// Create a new context with a different locale.
    pub fn with_locale(&self, locale: impl Into<String>) -> Self {
        Self {
            bundle: self.bundle.clone(),
            current_locale: locale.into(),
            fallback_locale: self.fallback_locale.clone(),
        }
    }

    /// Translate a key using the current locale.
    ///
    /// Falls back to the fallback locale, then returns the key itself.
    pub fn t<'a>(&'a self, key: &'a str) -> &'a str {
        self.bundle
            .get(&self.current_locale, key)
            .or_else(|| self.bundle.get(&self.fallback_locale, key))
            .unwrap_or(key)
    }

    /// Translate with interpolation: `{name}` placeholders are replaced.
    pub fn t_with(&self, key: &str, params: &[(&str, &str)]) -> String {
        let template = self.t(key);
        let mut result = template.to_string();
        for (param, value) in params {
            result = result.replace(&format!("{{{}}}", param), value);
        }
        result
    }

    /// Get the current locale code.
    pub fn current_locale(&self) -> &str {
        &self.current_locale
    }

    /// Get all supported locales.
    pub fn supported_locales(&self) -> &[Locale] {
        self.bundle.supported_locales()
    }

    /// Check if a locale is supported.
    pub fn has_locale(&self, code: &str) -> bool {
        self.bundle.has_locale(code)
    }
}

// ── Locale Detection ────────────────────────────────────────────────────────

/// Extract locale from a URL path prefix (e.g., "/en/about" -> Some("en")).
pub fn detect_locale_from_path(path: &str, supported: &[Locale]) -> Option<String> {
    let segments: Vec<&str> = path.trim_start_matches('/').splitn(2, '/').collect();
    if let Some(first) = segments.first() {
        if supported.iter().any(|l| l.code == *first) {
            return Some(first.to_string());
        }
    }
    None
}

/// Extract locale from Accept-Language header.
pub fn detect_locale_from_header(header: &str, supported: &[Locale]) -> Option<String> {
    // Parse "en-US,en;q=0.9,ne;q=0.8" format
    let mut candidates: Vec<(f32, String)> = header
        .split(',')
        .map(|part| {
            let parts: Vec<&str> = part.trim().splitn(2, ";q=").collect();
            let lang = parts[0].trim();
            let quality: f32 = parts.get(1).and_then(|q| q.parse().ok()).unwrap_or(1.0);
            // Normalize: "en-US" -> "en"
            let base_lang = lang.split('-').next().unwrap_or(lang);
            (quality, base_lang.to_string())
        })
        .collect();

    candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    candidates
        .into_iter()
        .map(|(_, lang)| lang)
        .find(|lang| supported.iter().any(|l| l.code == *lang))
}

/// Strip locale prefix from a path (e.g., "/en/about" -> "/about").
pub fn strip_locale_prefix(path: &str, locale: &str) -> String {
    let prefix = format!("/{}", locale);
    if path.starts_with(&prefix) {
        let rest = &path[prefix.len()..];
        if rest.is_empty() {
            "/".to_string()
        } else {
            rest.to_string()
        }
    } else {
        path.to_string()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_bundle() -> TranslationBundle {
        let mut bundle = TranslationBundle::new();
        bundle.add_locale(
            Locale::new("en", "English"),
            vec![
                ("greeting", "Hello"),
                ("farewell", "Goodbye"),
                ("welcome", "Welcome, {name}!"),
            ],
        );
        bundle.add_locale(
            Locale::new("ne", "नेपाली"),
            vec![
                ("greeting", "नमस्ते"),
                ("farewell", "बिदा"),
                ("welcome", "स्वागतम्, {name}!"),
            ],
        );
        bundle
    }

    #[test]
    fn basic_translation() {
        let i18n = I18n::new(test_bundle(), "en");
        assert_eq!(i18n.t("greeting"), "Hello");
        assert_eq!(i18n.t("farewell"), "Goodbye");
    }

    #[test]
    fn locale_switching() {
        let i18n = I18n::new(test_bundle(), "en");
        let ne = i18n.with_locale("ne");
        assert_eq!(ne.t("greeting"), "नमस्ते");
        assert_eq!(ne.current_locale(), "ne");
    }

    #[test]
    fn fallback_locale() {
        let mut bundle = TranslationBundle::new();
        bundle.add_locale(
            Locale::new("en", "English"),
            vec![("greeting", "Hello"), ("only_en", "Only English")],
        );
        bundle.add_locale(Locale::new("ne", "नेपाली"), vec![("greeting", "नमस्ते")]);

        let i18n = I18n::new(bundle, "ne").with_fallback("en");
        assert_eq!(i18n.t("greeting"), "नमस्ते");
        assert_eq!(i18n.t("only_en"), "Only English"); // Falls back
    }

    #[test]
    fn missing_key_returns_key() {
        let i18n = I18n::new(test_bundle(), "en");
        assert_eq!(i18n.t("nonexistent_key"), "nonexistent_key");
    }

    #[test]
    fn interpolation() {
        let i18n = I18n::new(test_bundle(), "en");
        let result = i18n.t_with("welcome", &[("name", "Krab")]);
        assert_eq!(result, "Welcome, Krab!");
    }

    #[test]
    fn interpolation_nepali() {
        let i18n = I18n::new(test_bundle(), "ne");
        let result = i18n.t_with("welcome", &[("name", "Krab")]);
        assert_eq!(result, "स्वागतम्, Krab!");
    }

    #[test]
    fn locale_detection_from_path() {
        let locales = vec![Locale::new("en", "English"), Locale::new("ne", "नेपाली")];

        assert_eq!(
            detect_locale_from_path("/en/about", &locales),
            Some("en".to_string())
        );
        assert_eq!(
            detect_locale_from_path("/ne/blog", &locales),
            Some("ne".to_string())
        );
        assert_eq!(detect_locale_from_path("/fr/about", &locales), None);
        assert_eq!(detect_locale_from_path("/about", &locales), None);
    }

    #[test]
    fn locale_detection_from_header() {
        let locales = vec![Locale::new("en", "English"), Locale::new("ne", "नेपाली")];

        assert_eq!(
            detect_locale_from_header("ne,en;q=0.9", &locales),
            Some("ne".to_string())
        );
        assert_eq!(
            detect_locale_from_header("en-US,en;q=0.9,ne;q=0.8", &locales),
            Some("en".to_string())
        );
        assert_eq!(
            detect_locale_from_header("fr;q=0.9,ne;q=0.8", &locales),
            Some("ne".to_string())
        );
    }

    #[test]
    fn strip_locale_prefix_works() {
        assert_eq!(strip_locale_prefix("/en/about", "en"), "/about");
        assert_eq!(strip_locale_prefix("/en", "en"), "/");
        assert_eq!(strip_locale_prefix("/about", "en"), "/about");
    }

    #[cfg(any(feature = "rest", feature = "db"))]
    #[test]
    fn bundle_json_loading() {
        let mut bundle = TranslationBundle::new();
        bundle
            .add_locale_json(
                Locale::new("en", "English"),
                r#"{"hello": "Hello", "bye": "Bye"}"#,
            )
            .unwrap();

        assert_eq!(bundle.get("en", "hello"), Some("Hello"));
        assert_eq!(bundle.get("en", "bye"), Some("Bye"));
    }

    #[test]
    fn bundle_supported_locales() {
        let bundle = test_bundle();
        assert_eq!(bundle.supported_locales().len(), 2);
        assert!(bundle.has_locale("en"));
        assert!(bundle.has_locale("ne"));
        assert!(!bundle.has_locale("fr"));
    }
}
