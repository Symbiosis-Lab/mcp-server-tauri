//! Window and webview listing and discovery.
//!
//! Supports both `WebviewWindow` (combined window+webview) and standalone
//! `Webview` instances attached to bare `Window` containers. This is needed
//! for apps like moss that use Tauri's multi-webview architecture.

use serde::Serialize;
use serde_json::Value;
use tauri::{command, AppHandle, Manager, Runtime};

/// Information about a webview window.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    /// The unique label/identifier for this window
    pub label: String,
    /// The window title (if available)
    pub title: Option<String>,
    /// The current URL loaded in the webview (if available)
    pub url: Option<String>,
    /// Whether this window currently has focus
    pub focused: bool,
    /// Whether this window is visible
    pub visible: bool,
    /// Whether this is the main window (label == "main")
    pub is_main: bool,
}

/// Lists all open webview windows and standalone webviews in the application.
///
/// Enumerates both `WebviewWindow` instances and standalone `Webview`s attached
/// to bare `Window` containers. This ensures apps using Tauri's multi-webview
/// architecture are fully discoverable.
#[command]
pub async fn list_windows<R: Runtime>(app: AppHandle<R>) -> Result<Value, String> {
    let mut window_list: Vec<WindowInfo> = Vec::new();
    let mut seen_labels = std::collections::HashSet::new();

    // First: enumerate WebviewWindow instances (combined window+webview)
    for (label, window) in app.webview_windows().iter() {
        let title = window.title().ok();
        let url = window.url().ok().map(|u| u.to_string());
        let focused = window.is_focused().unwrap_or(false);
        let visible = window.is_visible().unwrap_or(false);
        let is_main = label == "main";

        seen_labels.insert(label.clone());
        window_list.push(WindowInfo {
            label: label.clone(),
            title,
            url,
            focused,
            visible,
            is_main,
        });
    }

    // Second: enumerate standalone Webview instances (not already seen)
    for (label, webview) in app.webviews().iter() {
        if seen_labels.contains(label) {
            continue;
        }
        let url = webview.url().ok().map(|u| u.to_string());
        // Standalone Webview doesn't have is_focused/is_visible — check parent window
        let parent = webview.window();
        let focused = parent.is_focused().unwrap_or(false);
        let visible = parent.is_visible().unwrap_or(false);
        let is_main = label == "main";

        let title = parent.title().ok();

        window_list.push(WindowInfo {
            label: label.clone(),
            title,
            url,
            focused,
            visible,
            is_main,
        });
    }

    // Sort by label for consistent ordering, with "main" first
    window_list.sort_by(|a, b| {
        if a.is_main {
            std::cmp::Ordering::Less
        } else if b.is_main {
            std::cmp::Ordering::Greater
        } else {
            a.label.cmp(&b.label)
        }
    });

    serde_json::to_value(&window_list).map_err(|e| format!("Failed to serialize windows: {e}"))
}

/// Context about which window was used for an operation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowContext {
    /// The label of the window that was used
    pub window_label: String,
    /// Total number of windows available
    pub total_windows: usize,
    /// Warning message if multiple windows exist but none was specified
    pub warning: Option<String>,
}

/// A resolved webview that can execute JavaScript.
///
/// Wraps either a `WebviewWindow` or a standalone `Webview`, providing a
/// uniform interface for JS execution and screenshots.
pub enum ResolvedWebview<R: Runtime> {
    WebviewWindow(tauri::WebviewWindow<R>),
    Webview(tauri::Webview<R>),
}

impl<R: Runtime> ResolvedWebview<R> {
    /// Execute JavaScript in this webview.
    pub fn eval(&self, js: &str) -> tauri::Result<()> {
        match self {
            Self::WebviewWindow(w) => w.eval(js),
            Self::Webview(w) => w.eval(js),
        }
    }

    /// Get the label of this webview.
    pub fn label(&self) -> &str {
        match self {
            Self::WebviewWindow(w) => w.label(),
            Self::Webview(w) => w.label(),
        }
    }

    /// Access the platform webview handle.
    pub fn with_webview<F: FnOnce(tauri::webview::PlatformWebview) + Send + 'static>(
        &self,
        f: F,
    ) -> tauri::Result<()> {
        match self {
            Self::WebviewWindow(w) => w.with_webview(f),
            Self::Webview(w) => w.with_webview(f),
        }
    }

    /// Get the underlying WebviewWindow if this is one (needed for screenshot API).
    pub fn as_webview_window(&self) -> Option<&tauri::WebviewWindow<R>> {
        match self {
            Self::WebviewWindow(w) => Some(w),
            Self::Webview(_) => None,
        }
    }

    /// Clone into a WebviewWindow if possible.
    pub fn into_webview_window(self) -> Option<tauri::WebviewWindow<R>> {
        match self {
            Self::WebviewWindow(w) => Some(w),
            Self::Webview(_) => None,
        }
    }
}

/// Result of resolving a window, including context information.
pub struct ResolvedWindow<R: Runtime> {
    pub window: ResolvedWebview<R>,
    pub context: WindowContext,
}

/// Resolves a webview by label, with smart fallback.
///
/// Resolution order:
/// 1. Try as WebviewWindow (combined window+webview)
/// 2. Try as standalone Webview
/// 3. If no explicit label, fall back to first available webview
pub fn resolve_window_with_context<R: Runtime>(
    app: &AppHandle<R>,
    label: Option<String>,
) -> Result<ResolvedWindow<R>, String> {
    let total_ww = app.webview_windows().len();
    let total_wv = app.webviews().len();
    let total_windows = total_ww + total_wv;

    let explicit_label = label.is_some();
    let target_label = label.clone().unwrap_or_else(|| "main".to_string());

    // Try as WebviewWindow first
    if let Some(w) = app.get_webview_window(&target_label) {
        let warning = if !explicit_label && total_windows > 1 {
            Some(multi_window_warning(&target_label, total_windows, app))
        } else {
            None
        };
        return Ok(ResolvedWindow {
            window: ResolvedWebview::WebviewWindow(w),
            context: WindowContext {
                window_label: target_label,
                total_windows,
                warning,
            },
        });
    }

    // Try as standalone Webview
    if let Some(w) = app.get_webview(&target_label) {
        let warning = if !explicit_label && total_windows > 1 {
            Some(multi_window_warning(&target_label, total_windows, app))
        } else {
            None
        };
        return Ok(ResolvedWindow {
            window: ResolvedWebview::Webview(w),
            context: WindowContext {
                window_label: target_label,
                total_windows,
                warning,
            },
        });
    }

    // Fall back to first available webview (only when no explicit label)
    if !explicit_label {
        // Prefer standalone webviews (more likely to be the app's primary content)
        if let Some((lbl, w)) = app.webviews().iter().next() {
            let warning = Some(multi_window_warning(lbl, total_windows, app));
            return Ok(ResolvedWindow {
                window: ResolvedWebview::Webview(w.clone()),
                context: WindowContext {
                    window_label: lbl.clone(),
                    total_windows,
                    warning,
                },
            });
        }
        // Then WebviewWindows
        if let Some((lbl, w)) = app.webview_windows().iter().next() {
            let warning = Some(multi_window_warning(lbl, total_windows, app));
            return Ok(ResolvedWindow {
                window: ResolvedWebview::WebviewWindow(w.clone()),
                context: WindowContext {
                    window_label: lbl.clone(),
                    total_windows,
                    warning,
                },
            });
        }
    }

    Err(format!("Window '{target_label}' not found"))
}

/// Simple resolve — returns a WebviewWindow for backward compatibility.
pub fn resolve_window<R: Runtime>(
    app: &AppHandle<R>,
    label: Option<String>,
) -> Result<tauri::WebviewWindow<R>, String> {
    let explicit = label.is_some();
    let target = label.unwrap_or_else(|| "main".to_string());

    if let Some(w) = app.get_webview_window(&target) {
        return Ok(w);
    }

    if !explicit {
        if let Some((_, w)) = app.webview_windows().iter().next() {
            return Ok(w.clone());
        }
    }

    Err(format!("Window '{target}' not found"))
}

fn multi_window_warning<R: Runtime>(
    actual_label: &str,
    total_windows: usize,
    app: &AppHandle<R>,
) -> String {
    let all_labels: Vec<String> = app
        .webviews()
        .keys()
        .chain(app.webview_windows().keys())
        .cloned()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    format!(
        "Multiple webviews detected ({total_windows} total). Using '{actual_label}'. \
         Use windowId parameter to target a specific webview. \
         Available: {}",
        all_labels.join(", ")
    )
}
