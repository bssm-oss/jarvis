use tauri::Manager;

#[tauri::command]
pub fn show_voice_activation(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;

    window.show().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())?;
    window
        .eval(
            "window.history.pushState(null, '', '/voice-activation');\
             window.dispatchEvent(new PopStateEvent('popstate'));\
             window.dispatchEvent(new CustomEvent('zeroclaw-voice-activation-show'));",
        )
        .map_err(|e| e.to_string())?;

    Ok(())
}
