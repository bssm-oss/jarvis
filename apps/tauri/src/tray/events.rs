//! Tray menu event handling.

use tauri::{AppHandle, Manager, Runtime, menu::MenuEvent};

pub fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    match event.id().as_ref() {
        "show" => show_main_window(app, None),
        "chat" => show_main_window(app, Some("/agent")),
        "jarvis" => show_main_window(app, Some("/jarvis")),
        "hide" => hide_all_windows(app),
        "quit" => {
            #[cfg(target_os = "macos")]
            crate::macos::launch_agents::quit_completely();
            app.exit(0);
        }
        _ => {}
    }
}

fn show_main_window<R: Runtime>(app: &AppHandle<R>, navigate_to: Option<&str>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        if let Some(path) = navigate_to {
            let script = format!(
                "window.history.pushState(null, '', '{path}');\
                 window.dispatchEvent(new PopStateEvent('popstate'));"
            );
            let _ = window.eval(&script);
        }
    }
}

fn hide_all_windows<R: Runtime>(app: &AppHandle<R>) {
    for window in app.webview_windows().values() {
        let _ = window.hide();
    }
}
