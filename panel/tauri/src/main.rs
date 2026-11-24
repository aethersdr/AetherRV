//! AetherRV kiosk panel, Tauri v2 (WebKitGTK 4.1): one fullscreen, undecorated window showing the React UI from the
//! local gateway. No IPC, no Tauri commands: the page talks to the gateway exactly as in a browser.
//!
//! URL: first argument, else $AETHERRV_PANEL_URL, else http://127.0.0.1:8080/ (the kiosk URL).

fn main() {
    let url = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("AETHERRV_PANEL_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8080/".into());
    tauri::Builder::default()
        .setup(move |app| {
            let u: tauri::Url = url.parse()?;
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::External(u))
                .title("AetherRV")
                .fullscreen(true)
                .decorations(false)
                // the panel's page colour (ui/src/styles/app.css --page) from the first frame: without it the GTK
                // window's default light background showed for ~0.5 s before the UI, a white flash at the splash
                // hand-off (docs/UI-RUNTIME-BENCHMARK.md)
                .background_color(tauri::window::Color(0x0b, 0x0c, 0x0f, 0xff))
                // not .resizable(false): GTK then fixes min = max size, sway floats such a window and it never
                // becomes fullscreen (measured: the page stayed at 800x600)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("tauri runtime");
}
