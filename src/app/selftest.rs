//! Scripted screenshots for testing without a visible desktop.
//!
//! `FINDER_SNAPSHOT=out.bmp FINDER_STEPS="view:1;tag:0;settings"` runs the
//! steps after start-up, saves a picture of the window and exits. Unset, this
//! does nothing.

use std::time::Duration;

use slint::ComponentHandle;

use super::{App, ViewMode, with_app};
use crate::MainWindow;

impl App {
    fn self_test_step(&mut self, step: &str) {
        let (key, arg) = step.split_once(':').unwrap_or((step, ""));
        let n: i32 = arg.parse().unwrap_or(0);
        match key {
            "view" => self.set_view(ViewMode::from_index(n)),
            "select" => {
                let model = self.model();
                if let Some(idx) = model.find_name(arg)
                    && let Some(row) = model.row_of(idx)
                {
                    self.row_pressed(row, false, false);
                }
            }
            "tag" => self.toggle_tag_for_test(n as usize),
            "menu" => self.open_items_menu(330.0, 200.0),
            "place-menu" => self.open_place_menu(n as usize, 60.0, 300.0),
            "settings" => self.settings_set("open", 1),
            "folder-style" => {
                if let Some(ui) = self.ui() {
                    ui.global::<crate::Theme>().set_folder_style(n);
                    ui.global::<crate::Bridge>().set_set_folder_style(n);
                }
            }
            "appearance" => {
                if let Some(ui) = self.ui() {
                    ui.global::<crate::Theme>().set_appearance(n);
                    i_slint_renderer_software::x_finder_set_dark(ui.global::<crate::Theme>().get_dark());
                    ui.global::<crate::Bridge>().set_set_appearance(n);
                }
            }
            "place" => self.sidebar_clicked(n as usize),
            "goto" => match arg {
                "computer" => self.navigate(finder_rs::core::location::Location::Computer, None),
                "recent" => self.navigate(finder_rs::core::location::Location::Recent, None),
                "trash" => self.navigate(finder_rs::core::location::Location::Trash, None),
                path => self.navigate(finder_rs::core::location::Location::Dir(path.into()), None),
            },
            "mute" => self.media_mute(),
            "goto-tag" => self.navigate(finder_rs::core::location::Location::Tag(n as u8), None),
            "enclosing" => self.run(crate::commands::Command::ShowEnclosing),
            "help" => self.run(crate::commands::Command::Help),
            "palette" => self.palette_show(arg),
            "prun" => self.palette_run(n.max(0) as usize),
            "sort" => self.sort_by(finder_rs::core::sort::SortKey::from_index(n)),
            "line" => self.run_line(arg),
            "set" => {
                if let Some((k, v)) = arg.split_once('=') {
                    self.settings_set(k, v.parse().unwrap_or(0));
                }
            }
            "tip" => {
                if let Some(ui) = self.ui() {
                    let t = ui.global::<crate::Tip>();
                    t.set_text("Folder sizes".into());
                    t.set_x(725.0);
                    t.set_y(46.0);
                    t.set_shown(true);
                }
            }
            "picker" => {
                if let Some(ui) = self.ui() {
                    let b = ui.global::<crate::Bridge>();
                    b.set_layout_picker_x(655.0);
                    b.set_layout_picker_y(40.0);
                    b.set_layout_picker_open(true);
                }
            }
            "layout" => self.set_layout(n as usize),
            "slot" => self.slot_focus(n as usize),
            "sizes" => self.toggle_folder_sizes(),
            "tab-menu" => self.open_tab_menu(0, 300.0, 90.0),
            "hover" => self.menu_hover(n as usize),
            "new-tab" => self.open_tab(finder_rs::core::location::Location::Dir(arg.into()), true),
            "save-group" => self.save_tab_group(arg.to_string(), 3),
            "save-dialog" => self.begin_save_tab_group(),
            "open-group" => self.open_tab_group(n as usize),
            "left" => self.run(crate::commands::Command::CursorSide { forward: false, extend: false }),
            "pc" =>self.pc_open(1, n as usize, false),
            "search" =>self.search_submit(arg),
            // Typing in the search box (as the box does it).
            "type" => {
                if let Some(ui) = self.ui() {
                    ui.global::<crate::Bridge>().set_filter_text(arg.into());
                }
                self.set_filter(arg);
            }
            "skip" => self.media_skip(n),
            "banner" => self.show_explorer_banner(true),
            "tab" => self.activate(n.max(0) as usize),
            "shift-del" => self.run(crate::commands::Command::DeleteForever),
            "del" => self.run(crate::commands::Command::Recycle),
            "select-add" => {
                let model = self.model();
                if let Some(idx) = model.find_name(arg)
                    && let Some(row) = model.row_of(idx)
                {
                    self.row_pressed(row, true, false);
                }
            }
            // A letter typed on the list (type-to-select).
            "ta" => {
                for c in arg.chars() {
                    self.type_to_select(c);
                }
            }
            "size" => {
                if let (Some(ui), Some((w, h))) = (self.ui(), arg.split_once('x')) {
                    ui.window().set_size(slint::LogicalSize::new(w.parse().unwrap_or(1100.0), h.parse().unwrap_or(680.0)));
                }
            }
            "side" => {
                if let Some(ui) = self.ui() {
                    ui.global::<crate::Layout>().set_sidebar_open(true);
                }
            }
            "more" => {
                let w = self.ui().map_or(800.0, |ui| ui.window().size().to_logical(ui.window().scale_factor()).width);
                self.open_toolbar_more(w - 42.0, 40.0);
            }
            "toast-undo" => self.toast_with("Folders and Win+E now open in X Finder.", Some(("Undo", "undo-default"))),
            "icon-size" => {
                if let Some(ui) = self.ui() {
                    ui.global::<crate::Bridge>().set_icon_size(n as f32);
                }
                self.icon_size_changed(n as f32);
            }
            // n presses of ↓, 40 ms apart (like holding the key).
            "downs" => {
                for i in 0..n.max(1) {
                    slint::Timer::single_shot(Duration::from_millis(40 * i as u64), || {
                        with_app(|a| a.run(crate::commands::Command::CursorBy { delta: 1, extend: false }));
                    });
                }
            }
            "quicklook" => self.run(crate::commands::Command::QuickLook),
            "right" => self.run(crate::commands::Command::CursorSide { forward: true, extend: false }),
            _ => {}
        }
    }
}

pub fn start(ui: &MainWindow) {
    let Some(out) = std::env::var_os("FINDER_SNAPSHOT") else { return };
    let steps = std::env::var("FINDER_STEPS").unwrap_or_default();
    let weak = ui.as_weak();
    // Let the folder load, run each step with a pause, then capture.
    let steps: Vec<String> = steps.split(';').filter(|s| !s.is_empty()).map(String::from).collect();
    for (i, step) in steps.into_iter().enumerate() {
        slint::Timer::single_shot(Duration::from_millis(1200 + 500 * i as u64), move || {
            with_app(|a| a.self_test_step(&step));
        });
    }
    // "live": run the steps but leave the window up (for real screen grabs).
    if out == "live" {
        return;
    }
    let n = std::env::var("FINDER_STEPS").unwrap_or_default().split(';').count() as u64;
    slint::Timer::single_shot(Duration::from_millis(2200 + 500 * n), move || {
        super::media::trace("snapshot");
        if let Some(ui) = weak.upgrade()
            && let Ok(buf) = ui.window().take_snapshot()
        {
            let _ = write_bmp(std::path::Path::new(&out), buf.width(), buf.height(), buf.as_bytes());
        }
        super::media::trace("quit");
        // Stop playback while the event loop still runs.
        with_app(|a| a.stop_media());
        let _ = slint::quit_event_loop();
    });
}

/// Minimal 32-bit top-down BMP writer (RGBA in, BGRA out).
fn write_bmp(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    let mut out = Vec::with_capacity(54 + rgba.len());
    let size = 54 + rgba.len() as u32;
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    for p in rgba.as_chunks::<4>().0 {
        out.extend_from_slice(&[p[2], p[1], p[0], 255]);
    }
    std::fs::write(path, out)
}
