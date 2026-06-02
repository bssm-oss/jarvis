use tauri::{Manager, Runtime};

#[tauri::command]
pub fn show_voice_activation(app: tauri::AppHandle) -> Result<(), String> {
    show_voice_activation_window(&app, None)
}

pub fn show_voice_activation_window<R: Runtime>(
    app: &tauri::AppHandle<R>,
    signal: Option<&serde_json::Value>,
) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;

    window.show().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())?;

    let signal_script = if let Some(signal) = signal {
        let serialized = serde_json::to_string(signal).map_err(|e| e.to_string())?;
        format!(
            "const signal = {serialized};\
             if (typeof signal.createdAt === 'string') {{\
               const parsed = Date.parse(signal.createdAt);\
               signal.createdAt = Number.isFinite(parsed) ? parsed : Date.now();\
             }} else if (typeof signal.createdAt !== 'number') {{\
               signal.createdAt = Date.now();\
             }}\
             sessionStorage.setItem('zeroclaw_voice_activation_signal', JSON.stringify(signal));\
             if (signal.ackText) sessionStorage.setItem('zeroclaw_voice_ack', signal.ackText);\
             window.dispatchEvent(new CustomEvent('zeroclaw-voice-activation', {{ detail: signal }}));"
        )
    } else {
        String::new()
    };

    let script = format!(
        "{signal_script}\
         window.history.pushState(null, '', '/voice-activation');\
         window.dispatchEvent(new PopStateEvent('popstate'));\
         window.dispatchEvent(new CustomEvent('zeroclaw-voice-activation-show'));"
    );
    window.eval(&script).map_err(|e| e.to_string())?;

    Ok(())
}
