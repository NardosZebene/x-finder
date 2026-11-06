// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

mod app;
mod commands;
mod model;
mod thumbs;


fn main() -> Result<(), slint::PlatformError> {
    // Optional start folder: `finder-rs C:\some\folder` (also how Windows
    // opens folders when this is the default file explorer).
    let arg = std::env::args().nth(1).map(|a| a.trim().trim_matches('"').to_string());
    // Virtual folders (Control Panel, Recycle Bin, "shell:…") stay with Explorer.
    if let Some(a) = &arg
        && (a.starts_with("::") || a.to_lowercase().starts_with("shell:"))
    {
        let _ = std::process::Command::new("explorer.exe").arg(a).spawn();
        return Ok(());
    }
    // An app's "Show in folder": goes through Explorer, which selects the
    // file, and comes back here (see explorer_catch).
    let relay = arg.as_deref().and_then(app::instance::shown_by_app);
    // Already running and given a folder: it opens as a tab in that window.
    if let Some(a) = &arg
        && std::env::var_os("FINDER_SNAPSHOT").is_none()
        && app::instance::forward_to_running(&match &relay {
            Some(d) => format!("--relay,{}", d.display()),
            None => a.clone(),
        })
    {
        return Ok(());
    }
    if let Some(d) = relay {
        finder_rs::core::explorer_catch::relay(d, false);
    }
    // A folder, or a file (its folder opens with it selected).
    let (start, select) = app::instance::start_from_arg(arg.as_deref());

    // Software rendering keeps idle RAM in the tens of MB (plan §1).
    // SLINT_BACKEND still wins if set.
    if std::env::var_os("SLINT_BACKEND").is_none() {
        slint::BackendSelector::new().backend_name("winit".into()).renderer_name("software".into()).select()?;
    }

    // Native Windows menus ("More Options…") follow the system dark mode.
    finder_rs::core::shellx::enable_dark_menus();

    let ui = MainWindow::new()?;
    app::restore_window(&ui);
    app::install(&ui, start, select);

    // Files dragged in from Explorer or other apps.
    {
        use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
        ui.window().on_winit_window_event(|window, event| {
            match event {
                WindowEvent::DroppedFile(path) => app::external_drop(path.clone()),
                // Windows switched light/dark: "System" appearance follows it.
                WindowEvent::ThemeChanged(_) => app::apply_appearance(),
                // Stop Quick Look playback while the event loop still runs
                // (the Windows player can't shut down after it has ended).
                WindowEvent::CloseRequested => {
                    app::remember_window(window);
                    app::stop_playback();
                }
                WindowEvent::Destroyed => app::stop_playback(),
                // Back from Show Desktop / minimised / another app: draw
                // the whole window again, not just what changed.
                WindowEvent::Focused(true) | WindowEvent::Occluded(false) => app::repaint_all(),
                WindowEvent::Resized(_) => {
                    app::repaint_all();
                    app::window_changed(window);
                }
                WindowEvent::Moved(_) => app::window_changed(window),
                _ => {}
            }
            EventResult::Propagate
        });
    }
    // Once the window exists: light/dark for the app, title bar and menus.
    app::apply_appearance();
    slint::Timer::single_shot(std::time::Duration::from_millis(1), app::apply_appearance);

    // Taskbar / Alt+Tab icon from the exe's icon resource (all sizes).
    #[cfg(windows)]
    {
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1), move || {
            use slint::winit_030::WinitWindowAccessor;
            use slint::winit_030::winit::platform::windows::{IconExtWindows, WindowExtWindows};
            use slint::winit_030::winit::window::Icon;
            if let Some(ui) = weak.upgrade() {
                ui.window().with_winit_window(|w| {
                    if let Ok(icon) = Icon::from_resource(1, None) {
                        w.set_taskbar_icon(Some(icon));
                    }
                });
            }
        });
    }

    // Accept folders from later launches once the native window exists.
    {
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(1), move || {
            if let Some(ui) = weak.upgrade() {
                app::instance::listen(&ui);
            }
        });
    }

    app::selftest::start(&ui);

    let result = ui.run();
    app::shutdown();
    result
}
