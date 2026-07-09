//! Theme Manager Module
//! Detects the system light/dark appearance and drives the dynamic tray icon.
//!
//! On macOS the current appearance and its live changes both come from Tauri's
//! built-in theme support: `Window::theme()` reads `NSApp.effectiveAppearance`
//! and `WindowEvent::ThemeChanged` is emitted from the
//! `AppleInterfaceThemeChangedNotification` observer that tao registers for us.
//! There is no separate detection backend to run — we cache the last known
//! scheme in an atomic (seeded at startup, refreshed on each ThemeChanged) and
//! keep the tray-icon-swap plumbing below.

use crate::user_settings::UserSettings;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use tauri::image::Image;
use tauri::{AppHandle, Emitter, Manager, Theme};

/// Cached current scheme, updated on startup and on every ThemeChanged event.
/// Encoded to match `ColorScheme`: 0 = NoPreference, 1 = Dark, 2 = Light.
static CURRENT_SCHEME: AtomicU8 = AtomicU8::new(0);

/// Cached setting for dynamic tray icon (avoids disk I/O on the hot path)
static DYNAMIC_ICON_ENABLED: AtomicBool = AtomicBool::new(false);

/// System color scheme. Wire-compatible with the frontend `ColorScheme` type
/// (`'nopreference' | 'dark' | 'light'`). On macOS the backend only ever
/// reports Dark or Light; NoPreference is the pre-seed / no-window fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorScheme {
    /// No preference / unknown yet
    NoPreference,
    /// Dark appearance
    Dark,
    /// Light appearance
    Light,
}

impl ColorScheme {
    /// Map Tauri's system `Theme` to a `ColorScheme`.
    /// `Theme` is `#[non_exhaustive]`; anything that isn't Dark is treated Light.
    fn from_theme(theme: Theme) -> Self {
        match theme {
            Theme::Dark => ColorScheme::Dark,
            _ => ColorScheme::Light,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            ColorScheme::NoPreference => 0,
            ColorScheme::Dark => 1,
            ColorScheme::Light => 2,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => ColorScheme::Dark,
            2 => ColorScheme::Light,
            _ => ColorScheme::NoPreference,
        }
    }

    /// Whether this scheme represents dark mode
    pub fn is_dark(&self) -> bool {
        matches!(self, ColorScheme::Dark)
    }
}

/// Response from theme detection (unchanged wire shape for the frontend)
#[derive(Debug, Clone, serde::Serialize)]
pub struct ThemeInfo {
    /// The detected color scheme
    pub color_scheme: ColorScheme,
    /// Whether dark mode is preferred
    pub prefers_dark: bool,
    /// Source of the detection (for debugging)
    pub source: String,
}

impl ThemeInfo {
    fn from_scheme(scheme: ColorScheme, source: &str) -> Self {
        ThemeInfo {
            color_scheme: scheme,
            prefers_dark: scheme.is_dark(),
            source: source.to_string(),
        }
    }
}

/// Read the last known system color scheme from the cache.
/// Seeded by [`seed_from_window`] at startup and refreshed by
/// [`apply_theme_change`] on every ThemeChanged event, so no AppKit call
/// happens here — the getter is a cheap atomic load.
pub fn get_system_color_scheme() -> ThemeInfo {
    let scheme = ColorScheme::from_u8(CURRENT_SCHEME.load(Ordering::Relaxed));
    let source = if scheme == ColorScheme::NoPreference {
        "default"
    } else {
        "macos-appearance"
    };
    ThemeInfo::from_scheme(scheme, source)
}

/// Read the current appearance from a live window and prime the cache.
/// Call once from `setup()` on the main thread, before the tray needs it.
pub fn seed_from_window(app: &AppHandle) -> ThemeInfo {
    let scheme = read_window_scheme(app);
    CURRENT_SCHEME.store(scheme.to_u8(), Ordering::Relaxed);
    let info = get_system_color_scheme();
    eprintln!("[ThemeManager] Seeded system appearance: {:?}", info.color_scheme);
    info
}

/// Read the app-wide appearance from any available webview window.
fn read_window_scheme(app: &AppHandle) -> ColorScheme {
    let window = app
        .get_webview_window("main")
        .or_else(|| app.webview_windows().into_values().next());
    match window {
        Some(w) => match w.theme() {
            Ok(theme) => ColorScheme::from_theme(theme),
            Err(e) => {
                eprintln!("[ThemeManager] window.theme() failed: {e}");
                ColorScheme::NoPreference
            }
        },
        None => ColorScheme::NoPreference,
    }
}

/// Handle a `WindowEvent::ThemeChanged`: update the cache, notify the frontend,
/// and swap the tray icon. Wire this from the main window's event handler.
pub fn apply_theme_change(app: &AppHandle, theme: Theme) {
    let scheme = ColorScheme::from_theme(theme);
    let previous = CURRENT_SCHEME.swap(scheme.to_u8(), Ordering::Relaxed);
    if previous == scheme.to_u8() {
        return;
    }

    eprintln!("[ThemeManager] System appearance changed: {:?}", scheme);

    let info = ThemeInfo::from_scheme(scheme, "macos-appearance");
    if let Err(e) = app.emit("system-theme-changed", &info) {
        eprintln!("[ThemeManager] Failed to emit theme change event: {e}");
    }

    update_tray_icon(app, scheme.is_dark());
}

/// Refresh the tray icon manually (e.g. after a settings change).
/// Accepts settings to avoid reloading them.
pub fn refresh_tray_icon(app_handle: &AppHandle, settings: &UserSettings) {
    let is_dark = get_system_color_scheme().prefers_dark;
    update_tray_icon_with_settings(app_handle, is_dark, settings);
}

/// Update the cached dynamic tray icon setting
pub fn update_dynamic_tray_flag(enabled: bool) {
    DYNAMIC_ICON_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Helper to get the initial tray icon.
/// Uses a default icon initially to avoid blocking startup, then updates asynchronously.
pub fn initial_tray_icon(_settings: &UserSettings) -> (Image<'static>, bool) {
    eprintln!("[Tray] Initializing with default icon (non-blocking).");

    let icon =
        Image::from_bytes(include_bytes!("../icons/icon.png")).expect("Failed to load tray icon");
    (icon, false)
}

fn get_icon_bytes(enable_dynamic: bool, is_dark: bool) -> &'static [u8] {
    if enable_dynamic {
        if is_dark {
            include_bytes!("../icons/icon-light.png")
        } else {
            include_bytes!("../icons/icon-dark.png")
        }
    } else {
        include_bytes!("../icons/icon.png")
    }
}

fn apply_icon_to_tray(app: &AppHandle, icon_bytes: &[u8]) {
    if let Some(tray) = app.tray_by_id("main-tray") {
        if let Ok(icon) = Image::from_bytes(icon_bytes) {
            let _ = tray.set_icon(Some(icon));
            let _ = tray.set_icon_as_template(false);
        }
    }
}

fn update_tray_icon(app: &AppHandle, is_dark: bool) {
    // Determine target based on cached atomic setting (avoids disk I/O)
    let enable_dynamic = DYNAMIC_ICON_ENABLED.load(Ordering::Relaxed);
    let icon_bytes = get_icon_bytes(enable_dynamic, is_dark);
    apply_icon_to_tray(app, icon_bytes);
}

/// Optimized update that takes the settings directly
pub fn update_tray_icon_with_settings(app: &AppHandle, is_dark: bool, settings: &UserSettings) {
    let icon_bytes = get_icon_bytes(settings.enable_dynamic_tray_icon, is_dark);
    apply_icon_to_tray(app, icon_bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_scheme_from_theme() {
        assert_eq!(ColorScheme::from_theme(Theme::Dark), ColorScheme::Dark);
        assert_eq!(ColorScheme::from_theme(Theme::Light), ColorScheme::Light);
    }

    #[test]
    fn test_color_scheme_u8_roundtrip() {
        for scheme in [
            ColorScheme::NoPreference,
            ColorScheme::Dark,
            ColorScheme::Light,
        ] {
            assert_eq!(ColorScheme::from_u8(scheme.to_u8()), scheme);
        }
    }

    #[test]
    fn test_is_dark() {
        assert!(ColorScheme::Dark.is_dark());
        assert!(!ColorScheme::Light.is_dark());
        assert!(!ColorScheme::NoPreference.is_dark());
    }
}
