//! GIF Manager
//! Handles downloading GIFs and preparing them for clipboard paste.
//!
//! IMPORTANT: The GIF is placed on the clipboard **as a file** (an NSPasteboard
//! file-URL write, the macOS equivalent of Linux's `text/uri-list`) rather than
//! as raw bytes or text. This is required for rich media pasting: Finder copies
//! the file, and Chromium/Electron apps (Discord, Slack) surface it as a file
//! attachment, preserving the animation.

use arboard::Clipboard;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

// --- Constants ---

const APP_CACHE_DIR: &str = "win11-clipboard-history/gifs";
const DOWNLOAD_TIMEOUT: u64 = 10;

// --- Cache Management ---

struct GifCache;

impl GifCache {
    /// Get (and create if missing) the cache directory.
    /// `dirs::cache_dir()` resolves to `~/Library/Caches` on macOS.
    fn get_dir() -> Result<PathBuf, String> {
        let cache_dir = dirs::cache_dir()
            .ok_or("Failed to resolve system cache directory")?
            .join(APP_CACHE_DIR);

        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir)
                .map_err(|e| format!("Failed to create cache dir: {}", e))?;
        }

        Ok(cache_dir)
    }

    /// Generate a file path based on the URL hash.
    fn get_path_for_url(url: &str) -> Result<PathBuf, String> {
        let mut hasher = DefaultHasher::new();
        url.hash(&mut hasher);
        let hash = hasher.finish();

        Ok(Self::get_dir()?.join(format!("{}.gif", hash)))
    }
}

// --- Downloader ---

struct Downloader;

impl Downloader {
    /// Downloads a URL to a local file.
    pub fn download(url: &str, destination: &Path) -> Result<(), String> {
        eprintln!("[GifManager] Downloading: {}", url);

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(DOWNLOAD_TIMEOUT))
            .build()
            .map_err(|e| format!("Client build error: {}", e))?;

        let response = client
            .get(url)
            .send()
            .map_err(|e| format!("Network request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("HTTP Error: {}", response.status()));
        }

        let bytes = response
            .bytes()
            .map_err(|e| format!("Failed to read bytes: {}", e))?;

        let mut file =
            fs::File::create(destination).map_err(|e| format!("File creation failed: {}", e))?;

        file.write_all(&bytes)
            .map_err(|e| format!("File write failed: {}", e))?;

        eprintln!(
            "[GifManager] Saved {} bytes to {:?}",
            bytes.len(),
            destination
        );
        Ok(())
    }
}

// --- Clipboard Logic (The Critical Part) ---

struct ClipboardHandler;

impl ClipboardHandler {
    /// Puts the file on the general pasteboard as a file URL (`public.file-url`),
    /// so paste targets receive the actual .gif file — the macOS equivalent of
    /// the upstream `text/uri-list` write on Linux.
    ///
    /// Deliberately writes ONLY the file-URL representation (no raw image data):
    /// apps that prefer image data would otherwise paste a static first frame
    /// instead of attaching the animated file.
    fn copy_file_url(path: &Path) -> Result<(), String> {
        use objc2::rc::autoreleasepool;
        use objc2::runtime::ProtocolObject;
        use objc2_app_kit::NSPasteboard;
        use objc2_foundation::{NSArray, NSString, NSURL};

        let path_str = path
            .to_str()
            .ok_or_else(|| "GIF cache path is not valid UTF-8".to_string())?;

        eprintln!("[GifManager] Writing file URL to NSPasteboard");

        autoreleasepool(|_| {
            let url = unsafe { NSURL::fileURLWithPath(&NSString::from_str(path_str)) };
            let objects = NSArray::from_retained_slice(&[ProtocolObject::from_retained(url)]);

            let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
            unsafe { pasteboard.clearContents() };
            let written = unsafe { pasteboard.writeObjects(&objects) };

            if written {
                Ok(())
            } else {
                Err("NSPasteboard writeObjects returned false".to_string())
            }
        })
    }

    /// Fallback: Just put the text URL on the clipboard.
    fn copy_url_fallback(url: &str) -> Result<(), String> {
        eprintln!("[GifManager] Fallback: Setting clipboard to URL text");
        Clipboard::new()
            .map_err(|e| e.to_string())?
            .set_text(url)
            .map_err(|e| e.to_string())
    }
}

// --- Public API ---

/// Downloads a GIF from the URL and returns the local file path.
pub fn download_gif_to_file(url: &str) -> Result<PathBuf, String> {
    let target_path = GifCache::get_path_for_url(url)?;

    // Check if we already have it to avoid redownload (optional optimization,
    // but the original code overwrote every time. I'll maintain overwrite
    // to ensure validity, but using `Downloader` keeps it clean).
    Downloader::download(url, &target_path)?;

    Ok(target_path)
}

/// Downloads GIF and sets clipboard.
/// Returns Ok(Some(uri)) if successful (for history marking),
/// Ok(Some(url)) if fallback used,
/// Err if everything failed.
pub fn paste_gif_to_clipboard_with_uri(url: &str) -> Result<Option<String>, String> {
    // 1. Attempt Download
    let gif_path = match download_gif_to_file(url) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("[GifManager] Download failed ({}), using URL fallback.", e);
            ClipboardHandler::copy_url_fallback(url)?;
            return Ok(Some(url.to_string()));
        }
    };

    // 2. Attempt Copy
    match ClipboardHandler::copy_file_url(&gif_path) {
        Ok(()) => {
            let uri = format!("file://{}", gif_path.to_string_lossy());
            Ok(Some(uri))
        }
        Err(e) => {
            eprintln!("[GifManager] File copy failed ({}), using URL fallback.", e);
            ClipboardHandler::copy_url_fallback(url)?;
            Ok(Some(url.to_string()))
        }
    }
}

/// Convenience wrapper for cases where the URI return isn't needed.
pub fn paste_gif_to_clipboard(url: &str) -> Result<(), String> {
    paste_gif_to_clipboard_with_uri(url).map(|_| ())
}

/// Helper for external use if needed (legacy support)
pub fn copy_url_to_clipboard(url: &str) -> Result<(), String> {
    ClipboardHandler::copy_url_fallback(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_resolution() {
        let dir = GifCache::get_dir();
        assert!(dir.is_ok());
        assert!(dir.unwrap().ends_with("win11-clipboard-history/gifs"));
    }

    #[test]
    fn test_path_generation() {
        let path = GifCache::get_path_for_url("http://example.com/cat.gif");
        assert!(path.is_ok());
        assert!(path.unwrap().extension().unwrap() == "gif");
    }
}
