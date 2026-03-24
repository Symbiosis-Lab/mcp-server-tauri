//! JavaScript execution in webview.

use super::script_executor::ScriptExecutor;
#[cfg(not(target_os = "macos"))]
use crate::logging::mcp_log_error;
use serde_json::Value;
#[cfg(not(target_os = "macos"))]
use tauri::Listener;
use tauri::{command, Runtime, State, WebviewWindow};
#[cfg(not(target_os = "macos"))]
use tokio::sync::oneshot;
#[cfg(not(target_os = "macos"))]
use uuid::Uuid;

/// Executes JavaScript code in the webview context.
///
/// On macOS, uses native `WKWebView.evaluateJavaScript:completionHandler:` for
/// reliable result delivery. On other platforms, falls back to `window.eval()` +
/// Tauri event roundtrip.
///
/// # Arguments
///
/// * `window` - The Tauri window handle
/// * `script` - JavaScript code to execute
///
/// # Returns
///
/// * `Ok(Value)` - JSON object containing:
///   - `success`: Whether execution succeeded
///   - `data`: The result of the script execution (if successful)
///   - `error`: Error message (if failed)
#[command]
pub async fn execute_js<R: Runtime>(
    window: WebviewWindow<R>,
    script: String,
    _state: State<'_, ScriptExecutor>,
) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        execute_js_native_macos(&window, &script).await
    }

    #[cfg(not(target_os = "macos"))]
    {
        execute_js_event_roundtrip(window, script, state).await
    }
}

/// macOS: Use native WKWebView.evaluateJavaScript for reliable JS execution.
///
/// The eval+event approach fails on macOS because scripts injected via
/// `window.eval()` run in a context where `window.__TAURI__.event.emit()`
/// doesn't reach the Rust listener. Using the native completion handler
/// sidesteps this entirely.
#[cfg(target_os = "macos")]
async fn execute_js_native_macos<R: Runtime>(
    window: &WebviewWindow<R>,
    script: &str,
) -> Result<Value, String> {
    use block2::RcBlock;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSError, NSString};
    use objc2_web_kit::WKWebView;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    let prepared = prepare_script_for_native(script);
    let (tx, rx) = mpsc::channel::<Result<Value, String>>();
    let tx = Arc::new(Mutex::new(Some(tx)));

    window
        .with_webview(move |webview| {
            unsafe {
                let wkwebview: &WKWebView = &*(webview.inner() as *const _ as *const WKWebView);
                let ns_script = NSString::from_str(&prepared);

                let tx_clone = tx.clone();
                let handler =
                    RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
                        if let Some(tx) = tx_clone.lock().unwrap().take() {
                            if !error.is_null() {
                                let err = &*error;
                                let desc = err.localizedDescription();
                                let _ = tx.send(Err(desc.to_string()));
                            } else if !result.is_null() {
                                // Result is an NSString containing JSON
                                // (our script wrapper ensures this via JSON.stringify)
                                let obj = &*result;
                                let desc_ns: *mut NSString =
                                    objc2::msg_send![obj, description];
                                if !desc_ns.is_null() {
                                    let json_str = (&*desc_ns).to_string();
                                    let val = parse_json_result(&json_str);
                                    let _ = tx.send(val);
                                } else {
                                    let _ = tx.send(Ok(Value::Null));
                                }
                            } else {
                                let _ = tx.send(Ok(Value::Null));
                            }
                        }
                    });

                wkwebview
                    .evaluateJavaScript_completionHandler(
                        &ns_script,
                        Some(&handler),
                    );
            }
        })
        .map_err(|e| format!("Failed to access webview: {e}"))?;

    // Wait for result with timeout
    match rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Ok(data)) => Ok(serde_json::json!({
            "success": true,
            "data": data
        })),
        Ok(Err(error)) => Ok(serde_json::json!({
            "success": false,
            "error": error
        })),
        Err(_) => Ok(serde_json::json!({
            "success": false,
            "error": "Script execution timeout (10s)"
        })),
    }
}

/// Execute JS in a `ResolvedWebview` (for WebSocket handler).
///
/// On macOS, uses native `evaluateJavaScript`. On other platforms, uses `eval`.
#[cfg(target_os = "macos")]
pub async fn execute_js_in_resolved<R: Runtime>(
    webview: &super::list_windows::ResolvedWebview<R>,
    script: &str,
) -> Result<Value, String> {
    use block2::RcBlock;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSError, NSString};
    use objc2_web_kit::WKWebView;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    let prepared = prepare_script_for_native(script);
    let (tx, rx) = mpsc::channel::<Result<Value, String>>();
    let tx = Arc::new(Mutex::new(Some(tx)));

    webview
        .with_webview(move |wv| {
            unsafe {
                let wkwebview: &WKWebView = &*(wv.inner() as *const _ as *const WKWebView);
                let ns_script = NSString::from_str(&prepared);

                let tx_clone = tx.clone();
                let handler =
                    RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
                        if let Some(tx) = tx_clone.lock().unwrap().take() {
                            if !error.is_null() {
                                let err = &*error;
                                let desc = err.localizedDescription();
                                let _ = tx.send(Err(desc.to_string()));
                            } else if !result.is_null() {
                                let obj = &*result;
                                let desc_ns: *mut NSString = objc2::msg_send![obj, description];
                                if !desc_ns.is_null() {
                                    let json_str = (&*desc_ns).to_string();
                                    let val = parse_json_result(&json_str);
                                    let _ = tx.send(val);
                                } else {
                                    let _ = tx.send(Ok(Value::Null));
                                }
                            } else {
                                let _ = tx.send(Ok(Value::Null));
                            }
                        }
                    });

                wkwebview.evaluateJavaScript_completionHandler(&ns_script, Some(&handler));
            }
        })
        .map_err(|e| format!("Failed to access webview: {e}"))?;

    match rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Ok(data)) => Ok(serde_json::json!({ "success": true, "data": data })),
        Ok(Err(error)) => Ok(serde_json::json!({ "success": false, "error": error })),
        Err(_) => Ok(serde_json::json!({ "success": false, "error": "Script execution timeout (10s)" })),
    }
}

/// Parse JSON result from the script wrapper.
/// The wrapper always returns JSON.stringify'd output.
/// Check for __error key to propagate script errors.
#[cfg(target_os = "macos")]
fn parse_json_result(json_str: &str) -> Result<Value, String> {
    match serde_json::from_str::<Value>(json_str) {
        Ok(Value::Object(ref obj)) if obj.contains_key("__error") => {
            let err = obj.get("__error").and_then(|v| v.as_str()).unwrap_or("Unknown error");
            Err(err.to_string())
        }
        Ok(val) => Ok(val),
        Err(_) => Ok(Value::String(json_str.to_string())),
    }
}

#[cfg(not(target_os = "macos"))]
pub async fn execute_js_in_resolved<R: Runtime>(
    webview: &super::list_windows::ResolvedWebview<R>,
    script: &str,
) -> Result<Value, String> {
    // On non-macOS, use eval (the event roundtrip won't work without a command context)
    // For basic expression evaluation, eval + polling is used
    webview.eval(script).map_err(|e| format!("eval failed: {e}"))?;
    // Note: without the event roundtrip, we can't get the return value on non-macOS
    // from a standalone Webview. Return success with null data.
    Ok(serde_json::json!({ "success": true, "data": null }))
}

/// Prepare script for native `evaluateJavaScript` on macOS.
///
/// WKWebView's evaluateJavaScript returns ObjC-bridged types. Complex JS
/// objects (arrays, dicts) become NSDictionary/NSArray whose `description()`
/// is plist-like, not JSON. To guarantee correct serialization, we wrap
/// the user's script so it always returns a JSON string.
///
/// Important: the wrapper must be synchronous. WKWebView's evaluateJavaScript
/// does NOT await Promises — it returns the Promise object itself. Use a
/// synchronous IIFE with try/catch.
#[cfg(target_os = "macos")]
fn prepare_script_for_native(script: &str) -> String {
    let trimmed = script.trim();

    // Detect if the script is a single expression or multi-statement
    let has_real_semicolons = if let Some(without_trailing) = trimmed.strip_suffix(';') {
        without_trailing.contains(';')
    } else {
        trimmed.contains(';')
    };
    let is_multi_statement = has_real_semicolons
        || trimmed.starts_with("const ")
        || trimmed.starts_with("let ")
        || trimmed.starts_with("var ")
        || trimmed.starts_with("if ")
        || trimmed.starts_with("for ")
        || trimmed.starts_with("while ")
        || trimmed.starts_with("function ")
        || trimmed.starts_with("class ")
        || trimmed.starts_with("try ")
        || trimmed.starts_with("return ");

    // For single expressions, add "return" so the IIFE returns the value.
    // For multi-statement scripts, add "return" before the last statement.
    let body = if is_multi_statement {
        // Find the last semicolon-terminated statement and prepend "return" to the
        // final expression. This handles scripts like:
        //   var el = document.querySelector('body'); el.textContent
        // → var el = document.querySelector('body'); return el.textContent
        if let Some(last_semi_pos) = trimmed.rfind(';') {
            let (prefix, last_part) = trimmed.split_at(last_semi_pos + 1);
            let last_part = last_part.trim();
            if last_part.is_empty() {
                // Trailing semicolon, try the statement before it
                trimmed.to_string()
            } else if last_part.starts_with("return ") {
                trimmed.to_string()
            } else {
                format!("{prefix} return {last_part}")
            }
        } else {
            trimmed.to_string()
        }
    } else {
        format!("return {trimmed}")
    };

    // Synchronous IIFE that JSON.stringify's the result.
    format!(
        r#"(function() {{
            try {{
                var __result = (function() {{ {body} }})();
                if (__result !== undefined && __result !== null && typeof __result.then === 'function') {{
                    return JSON.stringify({{ __error: "Async scripts not supported in native eval. Wrap with await." }});
                }}
                return JSON.stringify(__result === undefined ? null : __result);
            }} catch (e) {{
                return JSON.stringify({{ __error: e.message || String(e) }});
            }}
        }})()"#
    )
}

/// Fallback: eval + event roundtrip (works on non-macOS platforms).
#[cfg(not(target_os = "macos"))]
async fn execute_js_event_roundtrip<R: Runtime>(
    window: WebviewWindow<R>,
    script: String,
    state: State<'_, ScriptExecutor>,
) -> Result<Value, String> {
    // Generate unique execution ID
    let exec_id = Uuid::new_v4().to_string();

    // Create oneshot channel for the result
    let (tx, rx) = oneshot::channel();

    // Store the sender for when result comes back
    {
        let mut pending = state.pending_results.lock().await;
        pending.insert(exec_id.clone(), tx);
    }

    // Set up event listener for the result
    let exec_id_clone = exec_id.clone();
    let pending_clone = state.pending_results.clone();

    let unlisten = window.listen("__script_result", move |event| {
        let raw_payload = event.payload();

        match serde_json::from_str::<serde_json::Map<String, Value>>(raw_payload) {
            Ok(payload) => {
                if let Some(Value::String(event_exec_id)) = payload.get("exec_id") {
                    if event_exec_id == &exec_id_clone {
                        // Forward to our result handler
                        let pending = pending_clone.clone();
                        let payload = payload.clone();
                        let exec_id_for_task = exec_id_clone.clone();

                        tokio::spawn(async move {
                            let mut pending_guard = pending.lock().await;
                            if let Some(sender) = pending_guard.remove(&exec_id_for_task) {
                                let result = if payload
                                    .get("success")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false)
                                {
                                    serde_json::json!({
                                        "success": true,
                                        "data": payload.get("data").cloned().unwrap_or(Value::Null)
                                    })
                                } else {
                                    serde_json::json!({
                                        "success": false,
                                        "error": payload.get("error")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("Unknown error")
                                    })
                                };

                                let _ = sender.send(result);
                            }
                        });
                    }
                }
            }
            Err(e) => {
                mcp_log_error(
                    "EXECUTE_JS",
                    &format!("Failed to parse __script_result payload: {e}. Raw: {raw_payload}"),
                );
            }
        }
    });

    // Prepare the script with appropriate return handling
    let prepared_script = prepare_script(&script);

    // Create wrapped script that uses event emission for result communication
    let wrapped_script = format!(
        r#"
        (function() {{
            function __sendResult(success, data, error) {{
                try {{
                    if (window.__TAURI__ && window.__TAURI__.event) {{
                        window.__TAURI__.event.emit('__script_result', {{
                            exec_id: '{exec_id}',
                            success: success,
                            data: data,
                            error: error
                        }});
                    }} else {{
                        console.error('[MCP] __TAURI__ not available, cannot send result');
                    }}
                }} catch (e) {{
                    console.error('[MCP] Failed to emit result:', e);
                }}
            }}

            (async () => {{
                try {{
                    const __executeScript = async () => {{
                        {prepared_script}
                    }};
                    const __result = await __executeScript();
                    __sendResult(true, __result !== undefined ? __result : null, null);
                }} catch (error) {{
                    __sendResult(false, null, error.message || String(error));
                }}
            }})().catch(function(error) {{
                __sendResult(false, null, error.message || String(error));
            }});
        }})();
        "#
    );

    // Execute the wrapped script
    if let Err(e) = window.eval(&wrapped_script) {
        let mut pending = state.pending_results.lock().await;
        pending.remove(&exec_id);

        return Ok(serde_json::json!({
            "success": false,
            "error": format!("Failed to execute script: {}", e)
        }));
    }

    // Wait for result with timeout
    let result = match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(_)) => Ok(serde_json::json!({
            "success": false,
            "error": "Script execution failed: channel closed"
        })),
        Err(_) => {
            let mut pending = state.pending_results.lock().await;
            pending.remove(&exec_id);
            Ok(serde_json::json!({
                "success": false,
                "error": "Script execution timeout"
            }))
        }
    };

    window.unlisten(unlisten);
    result
}

/// Prepare script by adding return statement if needed (for event roundtrip path).
#[cfg(not(target_os = "macos"))]
fn prepare_script(script: &str) -> String {
    let trimmed = script.trim();
    let needs_return = !trimmed.starts_with("return ");

    let has_real_semicolons = if let Some(without_trailing) = trimmed.strip_suffix(';') {
        without_trailing.contains(';')
    } else {
        trimmed.contains(';')
    };

    let is_multi_statement = has_real_semicolons
        || trimmed.starts_with("const ")
        || trimmed.starts_with("let ")
        || trimmed.starts_with("var ")
        || trimmed.starts_with("if ")
        || trimmed.starts_with("for ")
        || trimmed.starts_with("while ")
        || trimmed.starts_with("function ")
        || trimmed.starts_with("class ")
        || trimmed.starts_with("try ");

    let is_single_expression = trimmed.starts_with("await ")
        || trimmed.starts_with("(")
        || trimmed.starts_with("JSON.")
        || trimmed.starts_with("{")
        || trimmed.starts_with("[")
        || trimmed.ends_with(")()");

    let is_wrapped_expression = (trimmed.starts_with("(") && trimmed.ends_with(")"))
        || (trimmed.starts_with("(") && trimmed.ends_with(")()"))
        || (trimmed.starts_with("JSON.") && trimmed.ends_with(")"))
        || (trimmed.starts_with("await "));

    if needs_return && (is_single_expression || is_wrapped_expression || !is_multi_statement) {
        format!("return {trimmed}")
    } else {
        script.to_string()
    }
}
