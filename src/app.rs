//! Application state and the glue between UI events and the core.
//!
//! All state lives on the UI thread in a thread-local. Background work
//! (listing, watching, opening files) reports back through
//! `slint::invoke_from_event_loop`, which re-enters here via [`with_app`].

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use finder_rs::core::format::{self, thousands};
use finder_rs::core::preview::{self, BlockKind, Content};
use finder_rs::core::highlight;
use finder_rs::core::shell;
use finder_rs::core::fs_list::{self, Cancel, Entry, ListEvent};
use finder_rs::core::location::{self, Location};
use finder_rs::core::sort::SortKey;
use finder_rs::core::sys;
use finder_rs::core::watcher::{self, DirWatcher, WatchEvent};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::commands::{self, Command};

mod actions;
mod groups;
mod sizes;
mod split;
mod media;
mod palette;
pub mod instance;
mod menu;
mod places;
use places::build_places;
pub mod selftest;
pub use actions::external_drop;
use actions::{DropKind, RenameTarget};
pub use menu::{apply_appearance, shutdown};

/// Ends Quick Look audio/video (window closing).
pub fn stop_playback() {
    with_app(|a| a.stop_media());
}
use menu::MenuState;
use finder_rs::core::settings::{IconStyle, Settings};
use crate::model::{FileModel, ThumbCtx, ViewOpts};
use crate::thumbs::{self, Thumbs};
use crate::{
    Bridge, Crumb, DocBlock, MainWindow, PaneData, PreviewInfo, SidebarItem, StripItem, TabItem, TableRow, Theme,
};

const TYPE_AHEAD_TIMEOUT: Duration = Duration::from_millis(900);
/// Column view waits this long after the cursor stops before loading a preview,
/// so holding an arrow key doesn't list every folder it passes.
const PREVIEW_DELAY: Duration = Duration::from_millis(90);
/// Picture size in the gallery strip (gallery.slint: 76px tile, 6px margin).
const GALLERY_THUMB: f32 = 64.0;
/// Only show "Loading preview…" if a preview takes longer than this, so
/// arrowing through files doesn't flicker.
const LOADING_HINT_DELAY: Duration = Duration::from_millis(160);
/// Items on each side of the cursor in the gallery strip.
const STRIP_HALF: usize = 14;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    List,
    Icons,
    Columns,
    Gallery,
}

impl ViewMode {
    fn index(self) -> i32 {
        match self {
            ViewMode::List => 0,
            ViewMode::Icons => 1,
            ViewMode::Columns => 2,
            ViewMode::Gallery => 3,
        }
    }
    fn shows_thumbnails(self) -> bool {
        matches!(self, ViewMode::Icons | ViewMode::Gallery)
    }
    fn from_index(i: i32) -> Self {
        match i {
            1 => ViewMode::Icons,
            2 => ViewMode::Columns,
            3 => ViewMode::Gallery,
            _ => ViewMode::List,
        }
    }
}

/// A parent-folder column in column view.
struct Pane {
    loc: Location,
    model: Rc<FileModel>,
    cancel: Cancel,
    loading: bool,
    /// Name of the child that leads towards the current folder.
    select: String,
    selected_row: i32,
}

/// The last column in column view: contents of the selected folder.
struct Preview {
    entry: u32,
    path: PathBuf,
    model: Rc<FileModel>,
    cancel: Cancel,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` with the app state. Returns `None` if the app isn't set up or the
/// state is already borrowed (re-entrant call), instead of panicking.
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

struct Tab {
    id: u64,
    loc: Location,
    history: Vec<Location>,
    hist_pos: usize,
    model: Rc<FileModel>,
    /// View options the model was last sorted/filtered with.
    applied: ViewOpts,
    cancel: Cancel,
    loading: bool,
    loaded: usize,
    started: Instant,
    /// When the folder was last listed: switching back to the tab reloads
    /// it if the folder changed since (files dropped onto the tab, saved by
    /// another app…), since only the tab on screen is watched.
    listed_at: std::time::SystemTime,
    elapsed: Option<Duration>,
    error: Option<String>,
    /// Name to select once loading completes (e.g. the folder we came up from).
    pending_select: Option<String>,
    /// Select the first item once loading completes (keyboard navigation).
    pending_first: bool,
    /// Scroll offset to restore when switching back to this tab.
    scroll: f32,
    /// Column view: parent folders, outermost first.
    panes: Vec<Pane>,
    /// Column view starts here (the folder you opened), unless the
    /// "full path from drive" setting is on.
    col_root: Location,
    preview: Option<Preview>,
}

struct Place {
    label: String,
    icon: &'static str,
    /// `None` for section headers.
    loc: Option<Location>,
    /// A sidebar favourite (can be removed), as opposed to a drive.
    favorite: bool,
    /// Colour tag shown by this entry (Tags section), or -1.
    tag: i32,
}

pub struct App {
    ui: slint::Weak<MainWindow>,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    opts: ViewOpts,
    places: Vec<Place>,
    type_ahead: String,
    type_ahead_at: Instant,
    /// Icon view: last plain click (row, time), for double-click detection.
    last_press: Option<(usize, Instant)>,
    watcher: Option<DirWatcher>,
    watch_serial: u64,
    /// One-off message (e.g. "couldn't open file") shown in the status bar.
    note: Option<String>,
    view: ViewMode,
    preview_serial: u64,
    panes_model: Rc<VecModel<PaneData>>,
    thumbs: Rc<Thumbs>,
    /// Column view: start at "This PC" instead of the folder you opened.
    columns_absolute: bool,
    /// Set while moving around inside column view, so the start column stays put.
    keep_col_root: bool,
    quicklook: bool,
    preview_pane: bool,
    /// Serial of the newest file-preview request; older results are dropped.
    pv_serial: u64,
    pv_pending: bool,
    /// (tab id, entry) currently shown in the preview.
    pv_target: Option<(u64, u32)>,
    pv_path: Option<PathBuf>,
    pv_page: u32,
    pv_pages: u32,
    /// Names to select once the watcher reports them (after paste, drop, rename).
    select_after: Vec<String>,
    /// While a paste/duplicate/drop into this folder runs: select whatever new
    /// items appear (their final names, e.g. "x - Copy.png", aren't known up front).
    select_new: bool,
    new_items: Vec<String>,
    rename_target: Option<RenameTarget>,
    /// Files dropped from other apps, collected for a moment before copying.
    pending_drops: Vec<PathBuf>,
    settings: Settings,
    menu: Option<MenuState>,
    /// Full-resolution preview image; shown scaled to `pv_box` exactly.
    pv_source: Option<shell::Pixels>,
    /// Size of the preview area on screen (logical px).
    pv_box: (f32, f32),
    /// Pixel size the current preview was requested at.
    pv_req_px: u32,
    pv_fit_serial: u64,
    /// Drive names and sizes (filled in the background).
    volumes: std::collections::HashMap<String, sys::VolumeInfo>,
    drive_mask: u32,
    drive_timer: Option<slint::Timer>,
    /// Typing in the search box searches subfolders once typing pauses.
    search_timer: slint::Timer,
    /// The search box text as typed (case kept, for the title).
    live_text: String,
    /// The next navigation keeps the search box text (live search).
    keep_filter: bool,
    /// Recycle Bin items by their `$R…` path (for Put Back).
    trash_items: std::collections::HashMap<PathBuf, finder_rs::core::trash::TrashItem>,
    /// Free space on the current folder's drive.
    free_space: Option<u64>,
    /// Folder sizes worked out this session: path -> (bytes, files).
    size_cache: std::collections::HashMap<PathBuf, (u64, u64)>,
    /// Folder whose size is being worked out right now.
    size_job: Option<(PathBuf, Cancel)>,
    /// "Folder sizes" toolbar mode: size every folder in the list.
    folder_sizes: bool,
    /// The running folder-sizes scan (tab id, cancel token).
    sizes_scan: Option<(u64, Cancel)>,
    /// Split layout (0 single … 4 grid of four) and the tab shown in each pane.
    layout: usize,
    pane_ids: Vec<u64>,
    slots_model: Rc<VecModel<crate::SlotData>>,
    /// Watchers for the visible panes other than the focused one.
    pane_watches: Vec<split::PaneWatch>,
    /// Audio/video playing in Quick Look.
    media: Option<media::Media>,
    media_muted: bool,
    /// Quick Look's video area (logical pixels, window coordinates).
    media_rect: (f32, f32, f32, f32),
    /// What each command palette row does.
    palette_actions: Vec<palette::PAction>,
    /// A `!command` whose output the palette is showing.
    cmd_run: Option<palette::CmdRun>,
    /// What the toast's button does ("undo-default"), if it has one.
    toast_action: Option<&'static str>,
    /// Takes over Explorer windows other apps open while X Finder is the
    /// default file explorer.
    catcher: Option<finder_rs::core::explorer_catch::Catcher>,
    /// A press on an already-selected item: (row, ctrl). Applied on release
    /// unless the press turned into a drag.
    pending_click: Option<(usize, bool)>,
    cmd_serial: u64,
}

/// Creates the app state, wires every UI callback and opens `start`.
/// Puts the window back at its last size/position (not in test runs).
pub fn restore_window(ui: &MainWindow) {
    if std::env::var_os("FINDER_SNAPSHOT").is_some() {
        return;
    }
    let s = Settings::load();
    if s.window_pw >= 400 && s.window_ph >= 300 {
        ui.window().set_size(slint::PhysicalSize::new(s.window_pw, s.window_ph));
    } else if s.window_w >= 400.0 && s.window_h >= 300.0 {
        ui.window().set_size(slint::LogicalSize::new(s.window_w, s.window_h));
    }
    if s.window_x != i32::MIN && sys::point_on_screen(s.window_x + 40, s.window_y + 20) {
        ui.window().set_position(slint::PhysicalPosition::new(s.window_x, s.window_y));
    }
    if s.window_maximized {
        // The native window exists once the event loop runs.
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(1), move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().set_maximized(true);
            }
        });
    }
}

/// Records the window's size/position as it closes (saved by `shutdown`).
pub fn remember_window(window: &slint::Window) {
    with_app(|a| {
        let max = window.is_maximized();
        a.settings.window_maximized = max;
        // A maximized size isn't the size to come back to.
        if !max && !window.is_minimized() {
            let px = window.size();
            if px.width < 200 || px.height < 150 {
                return;
            }
            let size = px.to_logical(window.scale_factor());
            a.settings.window_w = size.width;
            a.settings.window_h = size.height;
            a.settings.window_pw = px.width;
            a.settings.window_ph = px.height;
            let pos = window.position();
            a.settings.window_x = pos.x;
            a.settings.window_y = pos.y;
        }
    });
}

/// The window was moved or resized: remember it now (saved a moment later),
/// so it comes back even if Windows shuts down with X Finder open.
pub fn window_changed(window: &slint::Window) {
    if std::env::var_os("FINDER_SNAPSHOT").is_some() {
        return;
    }
    remember_window(window);
    thread_local! { static SAVE: slint::Timer = slint::Timer::default(); }
    SAVE.with(|t| {
        t.start(slint::TimerMode::SingleShot, Duration::from_millis(900), || {
            with_app(|a| a.save_settings());
        })
    });
}

pub fn install(ui: &MainWindow, start: Location, select: Option<String>) {
    let settings = Settings::load();
    let app = App {
        ui: ui.as_weak(),
        tabs: Vec::new(),
        active: 0,
        next_id: 1,
        opts: ViewOpts::default(),
        places: build_places(&settings, &Default::default()),
        type_ahead: String::new(),
        type_ahead_at: Instant::now(),
        last_press: None,
        watcher: None,
        watch_serial: 0,
        note: None,
        view: ViewMode::List,
        preview_serial: 0,
        panes_model: Rc::new(VecModel::default()),
        thumbs: Thumbs::new(|job, px| {
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| a.on_thumb(job, px));
            });
        }),
        columns_absolute: false,
        keep_col_root: false,
        quicklook: false,
        preview_pane: false,
        pv_serial: 0,
        pv_pending: false,
        pv_target: None,
        pv_path: None,
        pv_page: 0,
        pv_pages: 0,
        select_after: Vec::new(),
        select_new: false,
        new_items: Vec::new(),
        rename_target: None,
        pending_drops: Vec::new(),
        settings: settings.clone(),
        menu: None,
        pv_source: None,
        pv_box: (400.0, 400.0),
        pv_req_px: 0,
        pv_fit_serial: 0,
        volumes: Default::default(),
        drive_mask: 0,
        drive_timer: None,
        search_timer: slint::Timer::default(),
        live_text: String::new(),
        keep_filter: false,
        trash_items: Default::default(),
        free_space: None,
        size_cache: Default::default(),
        size_job: None,
        folder_sizes: settings.show_folder_sizes,
        sizes_scan: None,
        layout: 0,
        pane_ids: Vec::new(),
        slots_model: Rc::new(VecModel::default()),
        pane_watches: Vec::new(),
        media: None,
        media_muted: false,
        media_rect: (0.0, 0.0, 0.0, 0.0),
        palette_actions: Vec::new(),
        cmd_run: None,
        toast_action: None,
        catcher: None,
        pending_click: None,
        cmd_serial: 0,
    };
    // Restore saved settings.
    {
        let b = ui.global::<Bridge>();
        b.set_icon_size(settings.icon_size.clamp(32.0, 256.0));
        b.set_preview_pane_width(settings.preview_width.clamp(200.0, 1200.0));
        b.set_pane_width(settings.column_width.clamp(140.0, 640.0));
        b.set_columns_absolute(settings.columns_full_path);
        b.set_preview_pane(settings.preview_pane);
        b.set_show_sizes(settings.show_folder_sizes);
        finder_rs::core::sort::set_size_mixes_folders(settings.show_folder_sizes);
        finder_rs::core::sort::set_folders_first(settings.folders_first);
    }
    let mut app = app;
    app.opts.show_hidden = settings.show_hidden;
    app.columns_absolute = settings.columns_full_path;
    app.preview_pane = settings.preview_pane;
    app.thumbs.windows_icons.set(settings.icon_style == IconStyle::Windows);
    app.thumbs.folder_previews.set(folder_preview_mode(&settings));
    app.thumbs.set_icon_size(ui.global::<Bridge>().get_icon_size(), 16.0, ui.window().scale_factor());
    ui.global::<Bridge>().set_panes(ModelRc::from(app.panes_model.clone()));
    ui.global::<Bridge>().set_slots(ModelRc::from(app.slots_model.clone()));
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    wire(ui);
    with_app(|app| {
        app.sync_sidebar();
        app.sync_settings_ui();
        app.start_drive_watch();
        if let Ok(exe) = std::env::current_exe() {
            finder_rs::core::default_explorer::repair(&exe);
            finder_rs::core::default_explorer::refresh_installed(&exe);
        }
        app.update_explorer_catch();
        let mut tab = app.make_tab(start);
        tab.pending_select = select;
        app.tabs.push(tab);
        app.active = 0;
        app.enter_location();
        // "Remember tabs": reopen last session's tabs, unless a folder was
        // passed on the command line (or this is a test run).
        if app.settings.restore_tabs
            && std::env::args().nth(1).is_none()
            && std::env::var_os("FINDER_SNAPSHOT").is_none()
            && let Some(last) = app.settings.last_tabs.clone()
        {
            app.open_tabs(&last);
        }
        let view = ViewMode::from_index(app.settings.start_view);
        if view != ViewMode::List {
            app.set_view(view);
        }
        if app.settings.layout > 0 {
            app.set_layout(app.settings.layout as usize);
        }
        app.settings.launches = app.settings.launches.saturating_add(1);
        app.save_settings();
    });
    // Asked a moment after start-up, never in the middle of opening.
    slint::Timer::single_shot(Duration::from_secs(3), || {
        with_app(|a| a.maybe_ask_default_explorer());
    });
}

fn wire(ui: &MainWindow) {
    let b = ui.global::<Bridge>();
    b.on_key(|text, ctrl, alt, shift| with_app(|a| a.on_key(&text, ctrl, alt, shift)).unwrap_or(false));
    b.on_row_pressed(|row, ctrl, shift| {
        instance::arm_drag_out();
        with_app(|a| a.row_pressed(row as usize, ctrl, shift));
    });
    b.on_explorer_banner_pick(|choice| {
        with_app(|a| a.explorer_banner_pick(choice));
    });
    b.on_toast_action_clicked(|| {
        with_app(|a| a.toast_action_clicked());
    });
    b.on_row_activated(|row| {
        with_app(|a| a.row_activated(row as usize));
    });
    b.on_row_middle(|row| {
        with_app(|a| a.row_middle(row as usize));
    });
    b.on_columns_setting_changed(|| {
        with_app(|a| {
            if let Some(ui) = a.ui() {
                a.columns_absolute = ui.global::<Bridge>().get_columns_absolute();
            }
            a.sync_columns();
        });
    });
    b.on_set_view(|mode| {
        with_app(|a| {
            a.set_view(ViewMode::from_index(mode));
            a.remember_folder_view();
        });
    });
    b.on_pane_clicked(|pane, row| {
        instance::arm_drag_out();
        with_app(|a| a.pane_clicked(pane as usize, row as usize, false));
    });
    b.on_pane_activated(|pane, row| {
        with_app(|a| a.pane_clicked(pane as usize, row as usize, true));
    });
    b.on_preview_clicked(|row| {
        with_app(|a| a.preview_clicked(row as usize, false));
    });
    b.on_preview_activated(|row| {
        with_app(|a| a.preview_clicked(row as usize, true));
    });
    b.on_search_submit(|text| {
        with_app(|a| a.search_submit(&text));
    });
    b.on_pc_open(|section, index| {
        with_app(|a| a.pc_open(section, index.max(0) as usize, false));
    });
    b.on_pc_context(|section, index, x, y| {
        with_app(|a| a.open_pc_menu(section, index.max(0) as usize, x, y));
    });
    b.on_fav_drag(|i| with_app(|a| a.fav_drag(i.max(0) as usize)).unwrap_or_default());
    b.on_menu_hover(|i| {
        with_app(|a| a.menu_hover(i.max(0) as usize));
    });
    b.on_menu_pick(|id| {
        with_app(|a| a.menu_pick(&id));
    });
    b.on_menu_close(|| {
        with_app(|a| a.menu_close());
    });
    b.on_background_context(|x, y| {
        with_app(|a| {
            a.model().select_where(|_| false);
            a.sync_status();
            a.open_background_menu(x, y);
        });
    });
    b.on_background_click(|| {
        with_app(|a| {
            a.model().select_where(|_| false);
            a.sync_status();
        });
    });
    b.on_place_context(|i, x, y| {
        with_app(|a| a.open_place_menu(i as usize, x, y));
    });
    b.on_header_context(|x, y| {
        with_app(|a| a.open_header_menu(x, y));
    });
    b.on_settings_set(|key, value| {
        with_app(|a| a.settings_set(&key, value));
    });
    b.on_preview_box(|w, h| {
        with_app(|a| a.preview_box(w, h));
    });
    b.on_row_context(|row, x, y| {
        with_app(|a| a.row_context(row as usize, x, y));
    });
    b.on_pane_context(|pane, row, x, y| {
        with_app(|a| a.pane_context(pane, row as usize, x, y));
    });
    b.on_menu_action(|id| {
        with_app(|a| a.menu_action(&id));
    });
    b.on_rename_commit(|text| {
        with_app(|a| a.rename_commit(&text));
    });
    b.on_rename_cancel(|| {
        with_app(|a| a.rename_cancel());
    });
    b.on_can_drop_row(|row, data| {
        with_app(|a| a.can_drop_to(a.drop_target(DropKind::Row(row as usize)), &data)).unwrap_or(false)
    });
    b.on_drop_row(|row, data| {
        with_app(|a| a.drop_to(a.drop_target(DropKind::Row(row as usize)), &data));
    });
    b.on_can_drop_pane(|pane, row, data| {
        with_app(|a| a.can_drop_to(a.drop_target(DropKind::Pane(pane, row as usize)), &data)).unwrap_or(false)
    });
    b.on_drop_pane(|pane, row, data| {
        with_app(|a| a.drop_to(a.drop_target(DropKind::Pane(pane, row as usize)), &data));
    });
    b.on_can_drop_place(|i, data| {
        with_app(|a| {
            let kind = DropKind::Place(i as usize);
            if a.drop_tag(kind).is_some() {
                return data.file_paths().map(|mut p| p.next().is_some()).unwrap_or(false);
            }
            // Reordering favourites.
            if let Some(from) = App::fav_drag_source(&data) {
                let target = a.places.get(i as usize);
                let ok = target.is_some_and(|p| p.favorite || p.loc == Some(Location::Recent));
                return ok && from != i as usize;
            }
            // Dropping on the Recycle Bin moves items there.
            if a.places.get(i as usize).is_some_and(|p| p.loc == Some(Location::Trash)) {
                return data.file_paths().map(|mut p| p.next().is_some()).unwrap_or(false);
            }
            a.can_drop_to(a.drop_target(kind), &data)
        })
        .unwrap_or(false)
    });
    b.on_drop_place(|i, data| {
        with_app(|a| {
            let kind = DropKind::Place(i as usize);
            if let Some(from) = App::fav_drag_source(&data) {
                // Onto Recents = to the top of the favourites.
                let to = if a.places.get(i as usize).is_some_and(|p| p.loc == Some(Location::Recent)) {
                    a.places.iter().position(|p| p.favorite).unwrap_or(i as usize)
                } else {
                    i as usize
                };
                a.move_favorite(from, to);
                return;
            }
            if a.places.get(i as usize).is_some_and(|p| p.loc == Some(Location::Trash)) {
                let sources: Vec<PathBuf> =
                    data.file_paths().map(|it| it.map(std::path::Path::to_path_buf).collect()).unwrap_or_default();
                if !sources.is_empty() {
                    a.run_op(finder_rs::core::fileops::Op::Recycle { sources });
                }
                return;
            }
            match a.drop_tag(kind) {
                Some(tag) => a.tag_dropped(tag, &data),
                None => a.drop_to(a.drop_target(kind), &data),
            }
        });
    });
    b.on_can_drop_tab(|i, data| {
        with_app(|a| a.can_drop_to(a.drop_target(DropKind::Tab(i as usize)), &data)).unwrap_or(false)
    });
    b.on_drop_tab(|i, data| {
        with_app(|a| a.drop_to(a.drop_target(DropKind::Tab(i as usize)), &data));
    });
    b.on_can_drop_here(|data| {
        with_app(|a| a.can_drop_to(a.drop_target(DropKind::Here), &data)).unwrap_or(false)
    });
    b.on_drop_here(|data| {
        with_app(|a| a.drop_to(a.drop_target(DropKind::Here), &data));
    });
    b.on_icon_size_changed(|size| {
        with_app(|a| a.icon_size_changed(size));
    });
    b.on_media_toggle(|| {
        with_app(|a| a.media_toggle());
    });
    b.on_media_seek(|f| {
        with_app(|a| a.media_seek(f));
    });
    b.on_media_skip(|secs| {
        with_app(|a| a.media_skip(secs));
    });
    b.on_media_mute(|| {
        with_app(|a| a.media_mute());
    });
    b.on_media_rect(|x, y, w, h| {
        with_app(|a| a.place_media(x, y, w, h));
    });
    b.on_toggle_quicklook(|| {
        with_app(|a| a.run(Command::QuickLook));
    });
    b.on_toggle_preview_pane(|| {
        with_app(|a| a.run(Command::TogglePreviewPane));
    });
    b.on_open_current(|| {
        with_app(|a| a.open_current());
    });
    b.on_pv_page_step(|delta| {
        with_app(|a| a.pv_page_step(delta));
    });
    b.on_sort_by(|col| {
        with_app(|a| a.sort_by(SortKey::from_index(col)));
    });
    b.on_sidebar_clicked(|i| {
        with_app(|a| a.sidebar_clicked(i as usize));
    });
    b.on_crumb_clicked(|i| {
        with_app(|a| a.crumb_clicked(i as usize));
    });
    b.on_tab_clicked(|i| {
        with_app(|a| a.activate(i as usize));
    });
    b.on_tab_context(|i, x, y| {
        with_app(|a| a.open_tab_menu(i as usize, x, y));
    });
    b.on_tab_closed(|i| {
        with_app(|a| a.close_tab(i as usize));
    });
    b.on_new_tab(|| {
        with_app(|a| a.run(Command::NewTab));
    });
    b.on_back(|| {
        with_app(|a| a.run(Command::Back));
    });
    b.on_forward(|| {
        with_app(|a| a.run(Command::Forward));
    });
    b.on_up(|| {
        with_app(|a| a.run(Command::Up));
    });
    b.on_refresh(|| {
        with_app(|a| a.run(Command::Refresh));
    });
    b.on_palette_show(|text| {
        with_app(|a| a.palette_show(&text));
    });
    b.on_palette_changed(|text| {
        with_app(|a| a.palette_changed(&text));
    });
    b.on_palette_run(|i| {
        with_app(|a| a.palette_run(i.max(0) as usize));
    });
    b.on_palette_copy_output(|| {
        with_app(|a| a.palette_copy_output());
    });
    b.on_palette_close(|| {
        with_app(|a| a.palette_close());
    });
    b.on_set_layout(|l| {
        with_app(|a| a.set_layout(l.max(0) as usize));
    });
    b.on_slot_focus(|k| {
        with_app(|a| a.slot_focus(k as usize));
    });
    b.on_slot_pressed(|k, row, ctrl, shift| {
        instance::arm_drag_out();
        with_app(|a| a.slot_pressed(k as usize, row as usize, ctrl, shift));
    });
    b.on_slot_activated(|k, row| {
        with_app(|a| a.slot_activated(k as usize, row as usize));
    });
    b.on_slot_context(|k, row, x, y| {
        with_app(|a| a.slot_context(k as usize, row as usize, x, y));
    });
    b.on_toolbar_more(|x, y| {
        with_app(|a| a.open_toolbar_more(x, y));
    });
    b.on_toggle_sizes(|| {
        with_app(|a| a.toggle_folder_sizes());
    });
    b.on_toggle_hidden(|| {
        with_app(|a| a.run(Command::ToggleHidden));
    });
    b.on_path_entered(|text| {
        with_app(|a| a.path_entered(&text));
    });
    b.on_filter_changed(|text| {
        with_app(|a| a.set_filter(&text));
    });
}

/// Shows the preview (and plays the media) for the newest cursor move.
/// Switching tracks can pump Windows messages while the app is busy (the old
/// player shutting down); a timer firing then finds the app in use, so it
/// tries again instead of being lost (the title would stay on an old file).
fn run_preview(serial: u64) {
    let done = with_app(|a| {
        // An older move: the newer one's timer does the work.
        if serial != a.preview_serial {
            return;
        }
        a.update_preview(serial);
        a.update_media();
    });
    if done.is_none() {
        slint::Timer::single_shot(Duration::from_millis(30), move || run_preview(serial));
    }
}

impl App {
    /// The "make X Finder the default?" banner: from the third start on,
    /// while it isn't the default, at most once a week after "Not now", and
    /// never after "Don't ask again" or three "Not now"s.
    fn maybe_ask_default_explorer(&mut self) {
        let s = &self.settings;
        let now = chrono::Utc::now().timestamp();
        let due = s.launches >= 3 && !s.explorer_ask_never && s.explorer_not_now < 3 && now >= s.explorer_ask_after;
        let test = std::env::var_os("FINDER_SNAPSHOT").is_some();
        if !due || test || std::env::current_exe().is_ok_and(|e| finder_rs::core::default_explorer::is_default(&e)) {
            return;
        }
        self.show_explorer_banner(true);
    }

    pub(super) fn show_explorer_banner(&self, on: bool) {
        if let Some(ui) = self.ui() {
            ui.global::<Bridge>().set_explorer_banner(on);
        }
    }

    fn explorer_banner_pick(&mut self, choice: i32) {
        self.show_explorer_banner(false);
        match choice {
            0 => {
                let Ok(exe) = std::env::current_exe() else { return };
                match finder_rs::core::default_explorer::set_default(&exe, true) {
                    Ok(()) => {
                        self.sync_settings_ui();
                        self.update_explorer_catch();
                        self.toast_with("Folders and Win+E now open in X Finder.", Some(("Undo", "undo-default")));
                    }
                    Err(e) => self.toast(e),
                }
            }
            1 => {
                self.settings.explorer_not_now += 1;
                self.settings.explorer_ask_after = chrono::Utc::now().timestamp() + 7 * 24 * 3600;
                self.save_settings();
            }
            _ => {
                self.settings.explorer_ask_never = true;
                self.save_settings();
                self.toast("Got it. You can still turn this on in Settings.");
            }
        }
    }

    /// Mouse button released: a press on a selected item that didn't move
    /// is a plain click after all (select just it, or Ctrl: unselect it).
    pub(super) fn press_released(&mut self, moved: bool) {
        let Some((row, ctrl)) = self.pending_click.take() else { return };
        if moved {
            return;
        }
        let model = self.model();
        if model.entry_at(row).is_none() {
            return;
        }
        if ctrl {
            model.toggle(row);
        } else {
            model.select_only(row);
        }
        self.size_selected_folder();
        self.sync_status();
        self.sync_strip();
        self.schedule_preview();
    }

    /// Catch Explorer windows only while X Finder is the default explorer.
    pub(super) fn update_explorer_catch(&mut self) {
        // Any X Finder registered as the default counts (an older copy too).
        let on = std::env::var_os("FINDER_CATCH").is_some()
            || (std::env::var_os("FINDER_SNAPSHOT").is_none() && finder_rs::core::default_explorer::registered_x_finder().is_some());
        if !on {
            self.catcher = None;
        } else if self.catcher.is_none() {
            self.catcher = finder_rs::core::explorer_catch::start(instance::open_caught);
        }
    }

    fn toast_action_clicked(&mut self) {
        match self.toast_action.take() {
            Some("undo-default") => {
                if let Ok(exe) = std::env::current_exe() {
                    let _ = finder_rs::core::default_explorer::set_default(&exe, false);
                    self.sync_settings_ui();
                    self.update_explorer_catch();
                }
                self.toast("Windows Explorer is the default again.");
            }
            _ => {
                if let Some(ui) = self.ui() {
                    ui.global::<Bridge>().set_toast("".into());
                }
            }
        }
    }
}

/// Folder previews: off, a picture on our folder icon, or (Windows folder
/// style) the folder exactly as Windows draws it.
pub(crate) fn folder_preview_mode(s: &finder_rs::core::settings::Settings) -> u8 {
    // Every style, Windows included, draws its own (Windows 11-style) folder
    // with the preview tucked in: Windows' own folder pictures are the
    // Windows 10 ones on Windows 10 and wouldn't match the rest of the app.
    if s.folder_previews { 1 } else { 0 }
}

/// Key for per-folder settings: lower-case, no trailing backslash.
fn folder_key(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().to_lowercase();
    match s.trim_end_matches('\\') {
        "" => s,
        t if t.ends_with(':') => format!("{t}\\"),
        t => t.to_string(),
    }
}

fn card_is_video(path: &std::path::Path) -> bool {
    preview::classify_path(path) == preview::Kind::Video
}

/// Redraws the whole window on the next frame, and once more a moment
/// later (Windows may still be restoring the window the first time).
pub fn repaint_all() {
    fn bump() {
        with_app(|a| {
            if let Some(ui) = a.ui() {
                let b = ui.global::<Bridge>();
                b.set_repaint_serial(b.get_repaint_serial().wrapping_add(1));
                ui.window().request_redraw();
            }
        });
    }
    bump();
    slint::Timer::single_shot(Duration::from_millis(150), bump);
}

/// Network folders: skip per-file extras (tags) that would be slow there.
fn is_network(dir: &std::path::Path) -> bool {
    let text = dir.to_string_lossy();
    text.starts_with(r"\\")
        || sys::drives().iter().any(|d| d.kind == 4 && text.to_ascii_uppercase().starts_with(&d.name.to_ascii_uppercase()))
}


impl App {
    fn ui(&self) -> Option<MainWindow> {
        self.ui.upgrade()
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn model(&self) -> Rc<FileModel> {
        self.tab().model.clone()
    }

    fn make_tab(&mut self, loc: Location) -> Tab {
        let id = self.next_id;
        self.next_id += 1;
        let model = Rc::new(FileModel::default());
        model.set_want_thumbs(self.view.shows_thumbnails());
        Tab {
            id,
            loc: loc.clone(),
            history: vec![loc.clone()],
            hist_pos: 0,
            model,
            applied: self.opts.clone(),
            cancel: Cancel::new(),
            loading: false,
            loaded: 0,
            started: Instant::now(),
            listed_at: std::time::SystemTime::now(),
            elapsed: None,
            error: None,
            pending_select: None,
            pending_first: false,
            scroll: 0.0,
            col_root: loc.clone(),
            panes: Vec::new(),
            preview: None,
        }
    }

    // ---- loading -----------------------------------------------------------

    /// (Re)lists a tab's folder. Cancels any listing still running for it.
    fn start_load(&mut self, idx: usize) {
        let id = self.tabs[idx].id;
        self.cancel_folder_sizes_for(id);
        let opts = self.opts.clone();
        let tab = &mut self.tabs[idx];
        tab.cancel.cancel();
        tab.cancel = Cancel::new();
        tab.model.clear();
        tab.applied = opts.clone();
        tab.loading = true;
        tab.loaded = 0;
        tab.started = Instant::now();
        tab.listed_at = std::time::SystemTime::now();
        tab.elapsed = None;
        tab.error = None;
        if let Some(p) = tab.preview.take() {
            p.cancel.cancel();
        }
        tab.model.set_thumbs(match &tab.loc {
            Location::Dir(dir) => Some(ThumbCtx {
                service: self.thumbs.clone(),
                model_id: tab.id,
                dir: dir.clone(),
                read_tags: !is_network(dir),
            }),
            Location::Tag(_) | Location::Recent | Location::Trash | Location::Search { .. } => Some(ThumbCtx {
                service: self.thumbs.clone(),
                model_id: tab.id,
                dir: PathBuf::new(),
                read_tags: true,
            }),
            Location::Computer => None,
        });
        if idx == self.active {
            // Entry numbers are reused by the new listing.
            self.pv_target = None;
        }
        let tab = &mut self.tabs[idx];

        match tab.loc.clone() {
            Location::Computer => {
                let drives: Vec<Entry> = sys::drives().iter().map(|d| Entry::drive(&d.name, d.kind)).collect();
                tab.loaded = drives.len();
                tab.model.append(drives, &opts);
                self.finish_load(idx, None);
            }
            Location::Dir(path) => {
                let tab_id = tab.id;
                spawn_list(path, tab.cancel.clone(), move |a, ev| a.on_list_event(tab_id, ev));
            }
            loc @ (Location::Recent | Location::Trash | Location::Search { .. }) => {
                self.start_special_load(idx, loc);
            }
            Location::Tag(bit) => {
                // Everything tagged with this colour, wherever it lives.
                let tab_id = tab.id;
                let cancel = tab.cancel.clone();
                std::thread::spawn(move || {
                    let entries: Vec<Entry> = finder_rs::core::tags::tagged(bit)
                        .into_iter()
                        .filter_map(|p| {
                            let mut e = Entry::from_path(&p)?;
                            e.path = Some(p);
                            Some(e)
                        })
                        .collect();
                    let total = entries.len();
                    let _ = slint::invoke_from_event_loop(move || {
                        if cancel.is_cancelled() {
                            return;
                        }
                        with_app(|a| {
                            a.on_list_event(tab_id, ListEvent::Batch(entries));
                            a.on_list_event(tab_id, ListEvent::Done { total, elapsed: Duration::ZERO, error: None });
                        });
                    });
                });
            }
        }
    }

    fn on_list_event(&mut self, tab_id: u64, ev: ListEvent) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else { return };
        match ev {
            ListEvent::Batch(batch) => {
                let tab = &mut self.tabs[idx];
                tab.loaded += batch.len();
                tab.model.append(batch, &tab.applied);
                if idx == self.active {
                    self.sync_status();
                    if self.view == ViewMode::Gallery && self.tab().model.cursor().is_none() {
                        self.move_cursor_to(0, false);
                    }
                }
            }
            ListEvent::Done { error, .. } => self.finish_load(idx, error),
        }
    }

    fn finish_load(&mut self, idx: usize, error: Option<String>) {
        let tab = &mut self.tabs[idx];
        if matches!(tab.loc, Location::Recent | Location::Trash) {
            // Most recently opened / deleted first.
            let newest = ViewOpts { sort: SortKey::Modified, ascending: false, ..tab.applied.clone() };
            tab.model.sort(&newest);
        } else {
            tab.model.sort(&tab.applied);
        }
        tab.loading = false;
        tab.error = error;
        tab.elapsed = Some(tab.started.elapsed());
        let mut scroll_row = None;
        if std::mem::take(&mut tab.pending_first)
            && tab.pending_select.is_none()
            && let Some(e) = tab.model.entry_at(0)
        {
            tab.model.select_only(0);
            tab.model.set_cursor(Some(e));
            tab.model.set_anchor(Some(e));
            scroll_row = Some(0);
        } else if let Some(name) = tab.pending_select.take() {
            if let Some(e) = tab.model.find_name(&name)
                && let Some(row) = tab.model.row_of(e)
            {
                tab.model.select_only(row);
                tab.model.set_cursor(Some(e));
                tab.model.set_anchor(Some(e));
                scroll_row = Some(row);
            }
        } else {
            scroll_row = tab.model.cursor_row();
        }
        if idx == self.active {
            self.sync_status();
            if let (Some(ui), Some(row)) = (self.ui(), scroll_row) {
                ui.invoke_scroll_to_row(row as i32);
            }
            if self.view == ViewMode::Gallery && self.tab().model.cursor().is_none() {
                self.move_cursor_to(0, false);
            }
            self.sync_strip();
            self.schedule_preview();
            self.start_folder_sizes();
        } else if self.layout > 0 {
            self.sync_slots();
        }
    }

    // ---- watching ----------------------------------------------------------

    fn rewatch(&mut self) {
        self.watcher = None;
        self.watch_serial += 1;
        let serial = self.watch_serial;
        if let Location::Dir(path) = &self.tab().loc {
            self.watcher = watcher::watch(path, move |ev| {
                let _ = slint::invoke_from_event_loop(move || {
                    with_app(|a| a.on_watch(serial, ev));
                });
            });
        }
        self.rewatch_panes();
    }

    fn on_watch(&mut self, serial: u64, ev: WatchEvent) {
        if serial != self.watch_serial || self.tab().loading {
            return;
        }
        match ev {
            WatchEvent::Rescan => self.reload(),
            WatchEvent::Changed(changes) => {
                let tab = &self.tabs[self.active];
                for (name, entry) in changes {
                    if self.select_new && entry.is_some() && tab.model.find_name(&name).is_none() {
                        self.new_items.push(name.clone());
                    }
                    tab.model.apply_change(&name, entry, &tab.applied);
                }
                if self.select_new && !self.new_items.is_empty() {
                    self.select_after = self.new_items.clone();
                }
                self.sync_status();
                self.apply_select_after();
            }
        }
    }

    // ---- navigation --------------------------------------------------------

    fn navigate(&mut self, loc: Location, select: Option<String>) {
        let keep_root = std::mem::take(&mut self.keep_col_root);
        let tab = &mut self.tabs[self.active];
        if !keep_root {
            tab.col_root = loc.clone();
        }
        if tab.loc != loc {
            tab.history.truncate(tab.hist_pos + 1);
            tab.history.push(loc.clone());
            tab.hist_pos = tab.history.len() - 1;
            tab.loc = loc;
        }
        tab.pending_select = select;
        tab.pending_first = false;
        self.enter_location();
    }

    fn go_history(&mut self, delta: isize) {
        let tab = &mut self.tabs[self.active];
        let Some(pos) = tab.hist_pos.checked_add_signed(delta).filter(|&p| p < tab.history.len()) else {
            return;
        };
        let from = std::mem::replace(&mut tab.loc, tab.history[pos].clone());
        tab.hist_pos = pos;
        // Going back to a parent re-selects the folder we left.
        tab.pending_select = (from.parent().as_ref() == Some(&tab.loc)).then(|| from.name_in_parent()).flatten();
        tab.pending_first = false;
        self.enter_location();
    }

    fn go_up(&mut self) {
        let loc = self.tab().loc.clone();
        if let Some(parent) = loc.parent() {
            self.navigate(parent, loc.name_in_parent());
        }
    }

    /// Free space on the focused tab's drive (status bar).
    fn update_free_space(&mut self) {
        self.free_space = match &self.tab().loc {
            Location::Dir(p) if !is_network(p) => sys::free_space(p),
            _ => None,
        };
    }

    /// Common tail of every navigation: clear filter, list, refresh chrome.
    fn enter_location(&mut self) {
        self.update_free_space();
        if let Location::Dir(p) = &self.tab().loc {
            let p = p.clone();
            self.remember_folder(&p);
        }
        self.note = None;
        self.select_after.clear();
        self.select_new = false;
        self.new_items.clear();
        self.type_ahead.clear();
        if !std::mem::take(&mut self.keep_filter) {
            self.clear_filter_text();
        }
        self.apply_folder_view();
        let idx = self.active;
        self.start_load(idx);
        self.tabs[idx].scroll = 0.0;
        self.sync_all();
        if let Some(ui) = self.ui() {
            ui.invoke_scroll_top();
        }
        self.sync_columns();
        self.rewatch();
    }

    fn reload(&mut self) {
        let model = self.model();
        let keep = model.cursor().and_then(|c| model.with_entry(c, |e| e.name.to_string()));
        self.tabs[self.active].pending_select = keep;
        let idx = self.active;
        self.start_load(idx);
        self.sync_status();
    }

    fn open_target(&mut self, idx: u32, new_tab: bool) {
        let model = self.model();
        let Some((path, is_dir)) = model.with_entry(idx, |e| match &e.link_dir {
            // A shortcut to a folder opens that folder here.
            Some(target) => (target.clone(), true),
            None => (self.tab().loc.path_of(e), e.is_dir),
        }) else {
            return;
        };
        if is_dir {
            if new_tab {
                self.open_tab(Location::Dir(path), false);
            } else {
                self.keep_col_root = self.view == ViewMode::Columns;
                self.navigate(Location::Dir(path), None);
            }
        } else {
            open_file(path);
        }
    }

    fn open_selection(&mut self, new_tab: bool) {
        let model = self.model();
        let mut targets = model.selected();
        if targets.is_empty() {
            targets.extend(model.cursor());
        }
        // The first folder opens here (unless asked for tabs); further
        // folders open in background tabs; files open in their apps.
        let mut navigated = new_tab;
        let mut deferred = None;
        for idx in targets {
            let is_dir = model.with_entry(idx, |e| e.is_dir || e.link_dir.is_some()).unwrap_or(false);
            if is_dir && !navigated {
                deferred = Some(idx);
                navigated = true;
            } else {
                self.open_target(idx, true);
            }
        }
        if let Some(idx) = deferred {
            self.open_target(idx, false);
        }
    }

    fn path_entered(&mut self, text: &str) {
        match location::parse_user_input(text) {
            Some(Location::Dir(p)) if p.is_file() => {
                let select = p.file_name().map(|n| n.to_string_lossy().into_owned());
                let parent = Location::Dir(p.parent().map(PathBuf::from).unwrap_or(p));
                self.navigate(parent, select);
            }
            Some(Location::Dir(p)) if !p.is_dir() => {
                self.note = Some(format!("Can't find “{}”.", p.display()));
                self.sync_status();
            }
            Some(loc) => self.navigate(loc, None),
            None => {
                self.note = Some(format!("“{}” isn't a full path.", text.trim()));
                self.sync_status();
            }
        }
    }

    fn sidebar_clicked(&mut self, i: usize) {
        if let Some(loc) = self.places.get(i).and_then(|p| p.loc.clone()) {
            self.navigate(loc, None);
        }
    }

    fn crumb_clicked(&mut self, shown: usize) {
        let crumbs = self.tab().loc.crumbs();
        // "…" stands for the deepest hidden folder.
        let Some(&slot) = visible_crumbs(crumbs.len()).get(shown) else { return };
        let i = slot.unwrap_or(crumbs.len().saturating_sub(4));
        if i + 1 >= crumbs.len() {
            return;
        }
        let select = crumbs.get(i + 1).map(|c| c.0.clone());
        self.navigate(crumbs[i].1.clone(), select);
    }

    // ---- tabs --------------------------------------------------------------

    fn open_tab(&mut self, loc: Location, activate: bool) {
        let tab = self.make_tab(loc);
        let at = self.active + 1;
        self.tabs.insert(at, tab);
        self.start_load(at);
        if activate {
            self.activate(at);
        } else {
            self.sync_tabs();
        }
    }

    fn close_tab(&mut self, i: usize) {
        if self.tabs.len() <= 1 || i >= self.tabs.len() {
            return;
        }
        self.save_scroll();
        let tab = self.tabs.remove(i);
        tab.cancel.cancel();
        let next = if i < self.active {
            self.active - 1
        } else {
            self.active.min(self.tabs.len() - 1)
        };
        self.active = usize::MAX; // force a full switch
        self.activate(next);
    }

    fn save_scroll(&mut self) {
        // Scroll offset isn't readable from Rust without extra plumbing, so
        // remember the cursor row instead (restored by `activate`).
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.scroll = tab.model.cursor_row().map_or(0.0, |r| r as f32);
        }
    }

    fn activate(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        let prev = self.tabs.get(self.active).map(|t| t.id);
        if i != self.active {
            self.save_scroll();
        }
        self.active = i;
        // Split layout: a tab picked from the tab bar takes the pane of the
        // tab that was focused.
        self.fix_slots(prev);
        self.update_free_space();
        self.type_ahead.clear();
        self.clear_filter_text();
        self.apply_folder_view();
        // The tab may have been sorted/filtered with older options.
        let opts = self.opts.clone();
        let tab = &mut self.tabs[i];
        if !tab.loading && tab.applied != opts {
            if tab.applied.sort != opts.sort || tab.applied.ascending != opts.ascending {
                tab.model.sort(&opts);
            } else {
                tab.model.refilter(&opts);
            }
        }
        tab.applied = opts;
        self.sync_all();
        if let Some(ui) = self.ui() {
            ui.invoke_scroll_top();
            if let Some(row) = self.tab().model.cursor_row() {
                ui.invoke_scroll_to_row(row as i32);
            }
        }
        self.sync_columns();
        self.rewatch();
        self.start_folder_sizes();
        // Changed while the tab wasn't on screen: list it again.
        let tab = &self.tabs[i];
        if let Location::Dir(p) = &tab.loc
            && !tab.loading
            && std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|m| m > tab.listed_at)
        {
            self.reload();
        }
    }

    fn move_tab(&mut self, delta: isize) {
        let Some(to) = self.active.checked_add_signed(delta).filter(|&t| t < self.tabs.len()) else { return };
        self.tabs.swap(self.active, to);
        self.active = to;
        self.sync_tabs();
    }

    // ---- view options --------------------------------------------------------

    fn sort_by(&mut self, key: SortKey) {
        if self.opts.sort == key {
            self.opts.ascending = !self.opts.ascending;
        } else {
            self.opts.sort = key;
            self.opts.ascending = key.default_ascending();
        }
        self.apply_opts(true);
        self.remember_folder_view();
    }

    /// Sort and view for the folder being shown: what was chosen there last
    /// time, else the defaults (name A→Z, the "Open folders as" view).
    fn apply_folder_view(&mut self) {
        let Location::Dir(p) = &self.tab().loc else { return };
        let views = &self.settings.folder_views;
        let own = views.get(&folder_key(p));
        // Else a rule set on a parent folder for all its subfolders.
        let rule = |pick: fn(&finder_rs::core::settings::FolderView) -> bool| {
            p.ancestors().skip(1).find_map(|a| views.get(&folder_key(a)).filter(|v| pick(v)))
        };
        let sort_from = match own {
            Some(v) => Some((v.sort, v.ascending)),
            None => rule(|v| v.sub_sort.is_some()).map(|v| (v.sub_sort.unwrap_or(0), v.sub_ascending)),
        };
        let view_from = match own {
            Some(v) => Some(v.view),
            None => rule(|v| v.sub_view.is_some()).and_then(|v| v.sub_view),
        };
        let view = ViewMode::from_index(view_from.unwrap_or(self.settings.start_view));
        // Folder-sizes mode: biggest first, in the list (unless the folder
        // was sorted some other way while sizes were on).
        let (sort, ascending) = match sort_from {
            Some((s, a)) => (SortKey::from_index(s), a),
            None if self.folder_sizes => (SortKey::Size, false),
            None => (SortKey::Name, true),
        };
        let view = if self.folder_sizes { ViewMode::List } else { view };
        self.opts.sort = sort;
        self.opts.ascending = ascending;
        // Column view keeps going while you click through folders in it.
        if view != self.view && self.view != ViewMode::Columns && self.layout == 0 {
            self.set_view(view);
        }
    }

    /// Remembers this folder's sort and view (they come back next visit).
    pub(super) fn remember_folder_view(&mut self) {
        let Location::Dir(p) = &self.tab().loc else { return };
        let key = folder_key(p);
        let views = &mut self.settings.folder_views;
        let v = views.entry(key).or_default();
        v.sort = self.opts.sort.index();
        v.ascending = self.opts.ascending;
        v.view = self.view.index();
        v.used = chrono::Utc::now().timestamp();
        // Keep the settings file small: forget the folders used longest ago
        // (never a rule for subfolders).
        while views.len() > 2000 {
            let Some(oldest) = views
                .iter()
                .filter(|(_, v)| v.sub_sort.is_none() && v.sub_view.is_none())
                .min_by_key(|(_, v)| v.used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            views.remove(&oldest);
        }
        self.save_settings();
    }

    /// "sort kind", "view icons all": sorts or shows this folder that way;
    /// with `all`, every subfolder too (also ones made later), replacing
    /// what was chosen in them before.
    pub(super) fn set_folder_rule(&mut self, sort: Option<(SortKey, bool)>, view: Option<ViewMode>, all: bool) {
        let Location::Dir(p) = self.tab().loc.clone() else {
            return self.toast("Open a folder first.");
        };
        if let Some((key, ascending)) = sort {
            self.opts.sort = key;
            self.opts.ascending = ascending;
            self.apply_opts(true);
        }
        if let Some(v) = view {
            self.set_view(v);
        }
        self.remember_folder_view();
        if !all {
            return;
        }
        let key = folder_key(&p);
        let prefix = if key.ends_with('\\') { key.clone() } else { format!("{key}\\") };
        for (k, v) in self.settings.folder_views.iter_mut() {
            if *k == key {
                if let Some((s, a)) = sort {
                    v.sub_sort = Some(s.index());
                    v.sub_ascending = a;
                }
                if let Some(m) = view {
                    v.sub_view = Some(m.index());
                }
            } else if k.starts_with(&prefix) {
                // Subfolders chosen before follow the new rule too.
                if let Some((s, a)) = sort {
                    (v.sort, v.ascending, v.sub_sort) = (s.index(), a, None);
                }
                if let Some(m) = view {
                    (v.view, v.sub_view) = (m.index(), None);
                }
            }
        }
        self.save_settings();
    }

    /// "sort reset" / "sort reset all": back to the defaults here (and in
    /// every subfolder).
    pub(super) fn reset_folder_rule(&mut self, all: bool) {
        let Location::Dir(p) = self.tab().loc.clone() else {
            return self.toast("Open a folder first.");
        };
        let key = folder_key(&p);
        let prefix = if key.ends_with('\\') { key.clone() } else { format!("{key}\\") };
        self.settings.folder_views.retain(|k, _| *k != key && !(all && k.starts_with(&prefix)));
        self.save_settings();
        self.apply_folder_view();
        self.apply_opts(true);
    }

    fn set_filter(&mut self, text: &str) {
        let f = text.trim().to_lowercase();
        if f != self.opts.filter {
            self.opts.query = finder_rs::core::query::Query::parse(&f);
            // Content is only searched on Enter (it reads the files).
            if let Some(c) = self.opts.query.content.take() {
                let hint = if self.opts.query.exts.is_empty() && self.opts.query.kind.is_none() {
                    "Add a type (pdf:, docx:, kind:document…) then press Enter to search inside files".to_string()
                } else {
                    format!("Press Enter to search inside files for “{c}” (this folder and subfolders)")
                };
                self.note(hint);
            } else {
                self.note = None;
            }
            self.opts.filter = f;
            self.apply_opts(false);
        }
        self.schedule_live_search(text);
    }

    /// This folder filters instantly as you type; once typing pauses, the
    /// search widens to every subfolder (the box keeps what you typed).
    fn schedule_live_search(&mut self, text: &str) {
        let text = text.trim().to_string();
        if text == self.live_text && !text.is_empty() {
            return;
        }
        self.live_text = text;
        if !matches!(self.tab().loc, Location::Dir(_) | Location::Search { .. }) {
            self.search_timer.stop();
            return;
        }
        let q = finder_rs::core::query::Query::parse(&self.live_text);
        // Reading inside files is slower: wait a little longer.
        let delay = if q.content.is_some() { 700 } else if self.live_text.is_empty() { 1 } else { 350 };
        self.search_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(delay), || {
            with_app(|a| a.live_search());
        });
    }

    /// Runs (or updates, or ends) the subfolder search for the box's text.
    pub(super) fn live_search(&mut self) {
        self.search_timer.stop();
        let text = self.live_text.clone();
        let (root, in_search) = match &self.tab().loc {
            Location::Dir(p) => (p.clone(), false),
            Location::Search { root, query } if *query != text => (root.clone(), true),
            _ => return,
        };
        if text.is_empty() {
            // Box cleared: back to the folder the search started in.
            if in_search {
                let tab = self.tab();
                let back = Location::Dir(root.clone());
                if tab.hist_pos > 0 && tab.history[tab.hist_pos - 1] == back {
                    self.go_history(-1);
                } else {
                    self.navigate(back, None);
                }
            }
            return;
        }
        if finder_rs::core::query::Query::parse(&text).content_needs_type() {
            return; // the status bar already asks for a file type
        }
        let loc = Location::Search { root, query: text };
        self.keep_filter = true;
        if in_search {
            // Refine in place: one history entry per search, not per key.
            let tab = &mut self.tabs[self.active];
            tab.loc = loc.clone();
            tab.col_root = loc.clone();
            let pos = tab.hist_pos;
            tab.history[pos] = loc;
            tab.pending_select = None;
            tab.pending_first = false;
            self.enter_location();
        } else {
            self.navigate(loc, None);
        }
    }

    fn clear_filter_text(&mut self) {
        // Both the text and its parsed query: a stale query kept hiding
        // everything ("No items match") after the box was emptied.
        self.opts.filter.clear();
        self.opts.query = Default::default();
        self.live_text.clear();
        self.search_timer.stop();
        if let Some(ui) = self.ui() {
            ui.global::<Bridge>().set_filter_text(SharedString::new());
        }
    }

    fn apply_opts(&mut self, resort: bool) {
        let opts = self.opts.clone();
        let tab = &mut self.tabs[self.active];
        if !tab.loading {
            if resort {
                tab.model.sort(&opts);
            } else {
                tab.model.refilter(&opts);
            }
        }
        tab.applied = opts;
        self.sync_view_opts();
        self.sync_status();
        if let Some(ui) = self.ui() {
            match self.tab().model.cursor_row() {
                Some(row) => ui.invoke_scroll_to_row(row as i32),
                None => ui.invoke_scroll_top(),
            }
        }
    }

    fn change_density(&mut self, delta: i32) {
        if self.view.shows_thumbnails() {
            // Bigger/smaller icons, like Finder's size slider.
            if let Some(ui) = self.ui() {
                let b = ui.global::<Bridge>();
                let factor = if delta < 0 { 1.25 } else { 0.8 };
                b.set_icon_size((b.get_icon_size() * factor).clamp(32.0, 256.0));
            }
            return;
        }
        if let Some(ui) = self.ui() {
            let theme = ui.global::<Theme>();
            theme.set_density((theme.get_density() + delta).clamp(0, 2));
        }
    }

    // ---- selection & keyboard -----------------------------------------------

    fn row_pressed(&mut self, row: usize, ctrl: bool, shift: bool) {
        // Icon view: the grid may rebuild its tiles after the first click
        // (selection, preview), which loses Slint's own double-click state,
        // so double-clicks are recognised here from the two presses.
        let now = Instant::now();
        let double = self.view == ViewMode::Icons
            && !ctrl
            && !shift
            && self.last_press.is_some_and(|(r, t)| r == row && now.duration_since(t) <= double_click_time());
        self.last_press = (!double && !ctrl && !shift).then_some((row, now));
        let model = self.model();
        let Some(idx) = model.entry_at(row) else { return };
        // Pressing an item that's already selected keeps the selection (it
        // may be the start of dragging them all); a click without dragging
        // narrows it on release, like Explorer.
        let selected = model.with_entry(idx, |e| e.selected).unwrap_or(false);
        self.pending_click = None;
        if !shift && selected && (ctrl || model.selected_count() > 1) {
            self.pending_click = Some((row, ctrl));
            model.set_cursor(Some(idx));
            self.type_ahead.clear();
            self.note = None;
            self.sync_status();
            return;
        }
        if shift {
            let anchor = model.anchor().or(model.cursor()).and_then(|a| model.row_of(a)).unwrap_or(row);
            model.select_range(anchor, row, ctrl);
        } else if ctrl {
            model.toggle(row);
            model.set_anchor(Some(idx));
        } else {
            model.select_only(row);
            model.set_anchor(Some(idx));
        }
        model.set_cursor(Some(idx));
        self.type_ahead.clear();
        self.note = None;
        self.size_selected_folder();
        self.sync_status();
        self.sync_strip();
        self.schedule_preview();
        if double {
            self.row_activated(row);
        }
    }

    /// Opens the folder that holds the selected item, with it selected.
    fn show_enclosing(&mut self) {
        let paths = self.selected_paths();
        let [path] = paths.as_slice() else {
            return self.note("Select one item to show its folder.");
        };
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { return };
        self.navigate(Location::Dir(parent.to_path_buf()), Some(name.to_string_lossy().into_owned()));
    }

    fn row_activated(&mut self, row: usize) {
        let model = self.model();
        let Some(idx) = model.entry_at(row) else { return };
        model.set_cursor(Some(idx));
        if !model.with_entry(idx, |e| e.selected).unwrap_or(false) {
            model.select_only(row);
        }
        self.open_selection(false);
    }

    fn row_middle(&mut self, row: usize) {
        if let Some(idx) = self.model().entry_at(row) {
            self.open_target(idx, true);
        }
    }

    /// Moves the cursor to `row`, selecting it (or extending from the anchor).
    fn move_cursor_to(&mut self, row: usize, extend: bool) {
        let model = self.model();
        let len = model.row_count();
        if len == 0 {
            return;
        }
        let row = row.min(len - 1);
        let Some(idx) = model.entry_at(row) else { return };
        if extend {
            let anchor = model.anchor().or(model.cursor()).unwrap_or(idx);
            let anchor_row = model.row_of(anchor).unwrap_or(row);
            model.set_anchor(Some(anchor));
            model.select_range(anchor_row, row, false);
        } else {
            model.select_only(row);
            model.set_anchor(Some(idx));
        }
        model.set_cursor(Some(idx));
        if let Some(ui) = self.ui() {
            ui.invoke_scroll_to_row(row as i32);
        }
        self.size_selected_folder();
        self.sync_status();
        self.sync_strip();
        self.schedule_preview();
    }

    /// Items per page in the current view.
    fn page_rows(&self) -> usize {
        let rows = self.ui().map_or(20, |ui| ui.invoke_page_rows().max(1) as usize);
        rows * self.grid_cols()
    }

    /// Tiles per row in icon view; 1 in the other views.
    fn grid_cols(&self) -> usize {
        match self.view {
            ViewMode::Icons => self.ui().map_or(1, |ui| ui.invoke_grid_cols().max(1) as usize),
            _ => 1,
        }
    }

    fn on_key(&mut self, text: &str, ctrl: bool, alt: bool, shift: bool) -> bool {
        // Ctrl+Alt+1…9: open a saved tab group.
        if ctrl
            && alt
            && !shift
            && text.chars().count() == 1
            && let Some(n) = text.chars().next().and_then(|c| c.to_digit(10)).filter(|n| (1..=9).contains(n))
            && self.open_group_by_shortcut(n as i32)
        {
            return true;
        }
        if let Some(cmd) = commands::from_key(text, ctrl, alt, shift) {
            self.run(cmd);
            return true;
        }
        if !ctrl && !alt {
            let mut chars = text.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                let special = c.is_control() || ('\u{F700}'..='\u{F8FF}').contains(&c);
                let typing = !self.type_ahead.is_empty() && self.type_ahead_at.elapsed() <= TYPE_AHEAD_TIMEOUT;
                if c == ' ' && !typing {
                    self.run(Command::QuickLook);
                    return true;
                }
                let space_ok = c != ' ' || typing;
                if !special && space_ok {
                    self.type_to_select(c);
                    return true;
                }
            }
        }
        false
    }

    fn type_to_select(&mut self, c: char) {
        if self.type_ahead_at.elapsed() > TYPE_AHEAD_TIMEOUT {
            self.type_ahead.clear();
        }
        self.type_ahead.extend(c.to_lowercase());
        self.type_ahead_at = Instant::now();
        let model = self.model();
        let cur = model.cursor_row();
        // The same letter again ("h", "h", "h"…) steps to the next name
        // starting with it, wrapping round, like Explorer; unless some name
        // really starts with the repeated letters ("hh…").
        let mut chars = self.type_ahead.chars();
        let first = chars.next().unwrap_or(c);
        let repeated = self.type_ahead.chars().count() > 1 && chars.all(|x| x == first);
        let typed = self.type_ahead.clone();
        let whole = if repeated { model.find_prefix(&typed, cur.unwrap_or(0)) } else { None };
        let (prefix, from) = match cur {
            _ if repeated && whole.is_none() => (first.to_string(), cur.map_or(0, |r| r + 1)),
            Some(r) if typed.chars().count() == 1 => (typed, r + 1),
            Some(r) => (typed, r),
            None => (typed, 0),
        };
        if let Some(row) = whole.or_else(|| model.find_prefix(&prefix, from)) {
            self.move_cursor_to(row, false);
        }
    }

    pub fn run(&mut self, cmd: Command) {
        match cmd {
            Command::Back => self.go_history(-1),
            Command::Forward => self.go_history(1),
            Command::Up => self.go_up(),
            // Ctrl+R in Tags / search / Recents: jump to the item's folder
            // (like Finder); elsewhere (and F5) it refreshes.
            Command::Refresh if !matches!(self.tab().loc, Location::Dir(_)) && self.selected_paths().len() == 1 => {
                self.show_enclosing()
            }
            Command::Refresh => self.reload(),
            Command::ShowEnclosing => self.show_enclosing(),
            Command::Palette => self.palette_show(""),
            Command::GoTo => self.palette_show("cd "),
            Command::Help => {
                if let Some(ui) = self.ui() {
                    ui.global::<Bridge>().set_help_open(true);
                }
            }
            Command::Open => self.open_selection(false),
            Command::OpenInNewTab => self.open_selection(true),
            Command::NewTab => {
                let loc = self.tab().loc.clone();
                self.open_tab(loc, true);
            }
            Command::CloseTab => self.close_tab(self.active),
            Command::NextTab => self.activate((self.active + 1) % self.tabs.len()),
            Command::PrevTab => self.activate((self.active + self.tabs.len() - 1) % self.tabs.len()),
            Command::MoveTabLeft => self.move_tab(-1),
            Command::MoveTabRight => self.move_tab(1),
            Command::ToggleHidden => {
                self.opts.show_hidden = !self.opts.show_hidden;
                self.apply_opts(false);
            }
            Command::SelectAll => {
                self.model().select_where(|_| true);
                self.sync_status();
            }
            Command::Escape => {
                if self.quicklook {
                    self.run(Command::QuickLook);
                } else if !self.opts.filter.is_empty() {
                    self.clear_filter_text();
                    self.apply_opts(false);
                } else {
                    self.model().select_where(|_| false);
                    self.sync_status();
                }
            }
            Command::ToggleSelectAtCursor => {
                if let Some(row) = self.model().cursor_row() {
                    self.model().toggle(row);
                    self.sync_status();
                }
            }
            Command::DensityTighter => self.change_density(1),
            Command::DensityLooser => self.change_density(-1),
            Command::ViewList => {
                self.set_view(ViewMode::List);
                self.remember_folder_view();
            }
            Command::ViewIcons => {
                self.set_view(ViewMode::Icons);
                self.remember_folder_view();
            }
            Command::ViewColumns => {
                self.set_view(ViewMode::Columns);
                self.remember_folder_view();
            }
            Command::ViewGallery => {
                self.set_view(ViewMode::Gallery);
                self.remember_folder_view();
            }
            Command::Copy => self.clipboard_put(false),
            Command::Cut => self.clipboard_put(true),
            Command::Paste => self.paste(),
            Command::Duplicate => self.duplicate(),
            Command::Rename => self.begin_rename(),
            Command::NewFolder => self.new_folder(),
            Command::Recycle => self.delete_selection(false),
            Command::DeleteForever => self.delete_selection(true),
            Command::CopyPath => self.copy_path(),
            Command::Properties => self.show_properties(),
            Command::QuickLook => {
                self.quicklook = !self.quicklook;
                if let Some(ui) = self.ui() {
                    ui.global::<Bridge>().set_quicklook(self.quicklook);
                }
                if !self.quicklook {
                    self.update_media();
                }
                self.schedule_preview();
            }
            Command::TogglePreviewPane => {
                self.preview_pane = !self.preview_pane;
                if let Some(ui) = self.ui() {
                    ui.global::<Bridge>().set_preview_pane(self.preview_pane);
                }
                self.schedule_preview();
            }
            Command::CursorBy { delta, extend } => {
                // In icon view Up/Down move a whole row of tiles; in Quick
                // Look they go to the previous/next file in sort order.
                let cols = if self.quicklook { 1 } else { self.grid_cols() };
                let step = delta as isize * cols as isize;
                let len = self.model().row_count();
                let row = match self.model().cursor_row() {
                    None => 0,
                    Some(r) => match r.checked_add_signed(step) {
                        Some(n) if n < len => n,
                        // Down from the second-to-last row lands on the last item.
                        Some(_) if step > 0 && len > 0 && r / cols < (len - 1) / cols => len - 1,
                        _ => r,
                    },
                };
                self.move_cursor_to(row, extend);
            }
            // Playing audio/video: ←/→ jump 10 seconds, Shift+←/→ a minute.
            Command::CursorSide { forward, extend } if self.quicklook && self.media.is_some() => {
                let secs = if extend { 60 } else { 10 };
                self.media_skip(if forward { secs } else { -secs })
            }
            Command::CursorSide { forward, .. } if self.quicklook => self.preview_page(forward),
            Command::CursorSide { forward, extend } => self.cursor_side(forward, extend),
            Command::CursorPage { down, .. } if self.quicklook && self.pv_pages > 1 => {
                self.pv_page_step(if down { 1 } else { -1 });
            }
            Command::CursorPage { down, extend } => {
                let page = self.page_rows();
                let cur = self.model().cursor_row().unwrap_or(0);
                let row = if down { cur + page } else { cur.saturating_sub(page) };
                self.move_cursor_to(row, extend);
            }
            Command::CursorHome { extend } => self.move_cursor_to(0, extend),
            Command::CursorEnd { extend } => self.move_cursor_to(usize::MAX, extend),
        }
    }

    // ---- views -------------------------------------------------------------

    fn set_view(&mut self, view: ViewMode) {
        self.view = view;
        if let Some(size) = self.ui().map(|ui| ui.global::<Bridge>().get_icon_size()) {
            self.icon_size_changed(size);
        }
        for tab in &self.tabs {
            tab.model.set_want_thumbs(view.shows_thumbnails());
        }
        if let Some(ui) = self.ui() {
            ui.global::<Bridge>().set_view_mode(view.index());
            if let Some(row) = self.model().cursor_row() {
                ui.invoke_scroll_to_row(row as i32);
            }
        }
        // Gallery always shows something: select the first item if nothing is.
        if view == ViewMode::Gallery && self.model().cursor().is_none() {
            self.move_cursor_to(0, false);
        }
        self.sync_columns();
        self.sync_strip();
        self.schedule_preview();
    }

    fn cursor_side(&mut self, forward: bool, extend: bool) {
        match self.view {
            // Like column view: Left goes to the parent (re-selecting the folder
            // we came from), Right steps into the selected folder.
            ViewMode::List if extend => {}
            ViewMode::List if !forward => self.go_up(),
            ViewMode::List => {
                let model = self.model();
                let Some(c) = model.cursor() else { return };
                let Some(target) = model.with_entry(c, |e| match &e.link_dir {
                    Some(t) => Some(t.clone()),
                    None if e.is_dir => Some(self.tab().loc.path_of(e)),
                    None => None,
                }) else {
                    return;
                };
                if let Some(path) = target {
                    self.navigate(Location::Dir(path), None);
                    // Land on the first item so arrows keep working.
                    self.tabs[self.active].pending_first = true;
                }
            }
            ViewMode::Icons | ViewMode::Gallery => {
                let row = match self.model().cursor_row() {
                    None => 0,
                    Some(r) if forward => r + 1,
                    Some(r) => r.saturating_sub(1),
                };
                self.move_cursor_to(row, extend);
            }
            ViewMode::Columns if !forward => {
                self.keep_col_root = true;
                self.go_up();
            }
            ViewMode::Columns => {
                // Step into the selected folder, selecting its first item.
                let model = self.model();
                let Some(c) = model.cursor() else { return };
                let Some((path, true)) = model.with_entry(c, |e| (self.tab().loc.path_of(e), e.is_dir)) else {
                    return;
                };
                let first = self.tab().preview.as_ref().filter(|p| p.entry == c).and_then(|p| {
                    let e = p.model.entry_at(0)?;
                    p.model.with_entry(e, |e| e.name.to_string())
                });
                self.keep_col_root = true;
                self.navigate(Location::Dir(path), first);
            }
        }
    }

    /// Options for parent columns and previews: same sort, but never filtered.
    fn pane_opts(&self) -> ViewOpts {
        ViewOpts { filter: String::new(), query: Default::default(), ..self.opts.clone() }
    }

    /// Makes the parent columns match the current folder, reusing columns that
    /// are already loaded. Clears everything when not in column view.
    fn sync_columns(&mut self) {
        let idx = self.active;
        if self.view != ViewMode::Columns {
            for tab in &mut self.tabs {
                for p in tab.panes.drain(..) {
                    p.cancel.cancel();
                }
                if let Some(p) = tab.preview.take() {
                    p.cancel.cancel();
                }
            }
            self.panes_model.set_vec(Vec::new());
            self.set_preview_mode(0);
            return;
        }

        let crumbs = self.tabs[idx].loc.crumbs();
        // First parent column to show. Relative mode starts at the folder you
        // opened; moving above it (Left, Back) moves the start up with you.
        let first = if self.columns_absolute {
            0
        } else {
            let root = self.tabs[idx].col_root.crumbs();
            let inside = root.len() <= crumbs.len() && root.iter().zip(&crumbs).all(|(a, b)| a.1 == b.1);
            if !inside {
                self.tabs[idx].col_root = self.tabs[idx].loc.clone();
            }
            if inside { root.len() - 1 } else { crumbs.len() - 1 }
        };
        let tab_id = self.tabs[idx].id;
        let opts = self.pane_opts();
        let mut old: Vec<Option<Pane>> = std::mem::take(&mut self.tabs[idx].panes).into_iter().map(Some).collect();
        let mut panes = Vec::new();
        for pair in crumbs.windows(2).skip(first) {
            let (loc, child) = (pair[0].1.clone(), pair[1].0.clone());
            let reused = old.iter_mut().find(|p| p.as_ref().is_some_and(|p| p.loc == loc)).and_then(Option::take);
            let pane = match reused {
                Some(mut p) => {
                    if !p.loading && p.select != child {
                        p.selected_row = select_name(&p.model, &child);
                    }
                    p.select = child;
                    p
                }
                None => {
                    let mut p = Pane {
                        loc: loc.clone(),
                        model: Rc::new(FileModel::default()),
                        cancel: Cancel::new(),
                        loading: true,
                        select: child,
                        selected_row: -1,
                    };
                    match &loc {
                        Location::Computer => {
                            let drives = sys::drives().iter().map(|d| Entry::drive(&d.name, d.kind)).collect();
                            p.model.append(drives, &opts);
                            p.model.sort(&opts);
                            p.loading = false;
                            p.selected_row = select_name(&p.model, &p.select);
                        }
                        Location::Dir(path) => {
                            let key = loc.clone();
                            spawn_list(path.clone(), p.cancel.clone(), move |a, ev| a.on_pane_event(tab_id, &key, ev));
                        }
                        _ => {}
                    }
                    p
                }
            };
            panes.push(pane);
        }
        for p in old.into_iter().flatten() {
            p.cancel.cancel();
        }
        self.tabs[idx].panes = panes;
        self.push_panes();
        self.schedule_preview();
    }

    fn pane_data(p: &Pane) -> PaneData {
        PaneData {
            title: p.loc.title().into(),
            files: ModelRc::from(p.model.clone()),
            selected: p.selected_row,
        }
    }

    /// Mirrors the active tab's parent columns into the UI model, updating
    /// rows in place so existing columns keep their scroll position.
    fn push_panes(&self) {
        let panes = &self.tab().panes;
        let m = &self.panes_model;
        for (i, p) in panes.iter().enumerate() {
            if i < m.row_count() {
                m.set_row_data(i, Self::pane_data(p));
            } else {
                m.push(Self::pane_data(p));
            }
        }
        while m.row_count() > panes.len() {
            m.remove(m.row_count() - 1);
        }
    }

    fn on_pane_event(&mut self, tab_id: u64, loc: &Location, ev: ListEvent) {
        let opts = self.pane_opts();
        let Some(ti) = self.tabs.iter().position(|t| t.id == tab_id) else { return };
        let Some(pi) = self.tabs[ti].panes.iter().position(|p| &p.loc == loc) else { return };
        let pane = &mut self.tabs[ti].panes[pi];
        match ev {
            ListEvent::Batch(batch) => pane.model.append(batch, &opts),
            ListEvent::Done { .. } => {
                pane.model.sort(&opts);
                pane.loading = false;
                pane.selected_row = select_name(&pane.model, &pane.select);
                if ti == self.active && pi < self.panes_model.row_count() {
                    self.panes_model.set_row_data(pi, Self::pane_data(&self.tabs[ti].panes[pi]));
                }
            }
        }
    }

    fn set_preview_mode(&self, mode: i32) {
        if let Some(ui) = self.ui() {
            ui.global::<Bridge>().set_preview_mode(mode);
        }
    }

    /// Whether anything currently shows a preview of the cursor item.
    fn preview_visible(&self) -> bool {
        self.quicklook
            || matches!(self.view, ViewMode::Gallery | ViewMode::Columns)
            || (self.preview_pane && matches!(self.view, ViewMode::List | ViewMode::Icons))
    }

    /// Debounced: refresh previews after the cursor settles.
    fn schedule_preview(&mut self) {
        if !self.preview_visible() {
            return;
        }
        self.preview_serial += 1;
        let serial = self.preview_serial;
        slint::Timer::single_shot(PREVIEW_DELAY, move || run_preview(serial));
    }

    fn update_preview(&mut self, serial: u64) {
        if serial != self.preview_serial || !self.preview_visible() {
            return;
        }
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let columns = self.view == ViewMode::Columns;
        let tab = &mut self.tabs[self.active];
        let tab_id = tab.id;
        // Nothing to preview without a cursor, or with a multi-selection.
        let cursor = tab.model.cursor().filter(|_| tab.model.selected_count() <= 1);
        let Some(c) = cursor else {
            if let Some(p) = tab.preview.take() {
                p.cancel.cancel();
            }
            b.set_preview_mode(0);
            b.set_pv_kind(0);
            b.set_pv_loading(false);
            self.pv_target = None;
            self.pv_serial += 1;
            return;
        };
        let Some((path, is_dir, card, modified)) = tab.model.with_entry(c, |e| {
            let card = PreviewInfo {
                name: e.name.as_ref().into(),
                kind: format::kind_label(e).as_ref().into(),
                size: if e.is_dir { "--".into() } else { format::size(e.size).into() },
                modified: format::date(e.modified).into(),
                created: format::date(e.created).into(),
                icon: format::icon(e).into(),
            };
            (tab.loc.path_of(e), e.is_dir, card, e.modified)
        }) else {
            return;
        };
        b.set_preview_info(card);

        if is_dir {
            // Column view lists the folder in the next column.
            if columns && !tab.preview.as_ref().is_some_and(|p| p.entry == c) {
                if let Some(p) = tab.preview.take() {
                    p.cancel.cancel();
                }
                let model = Rc::new(FileModel::default());
                let cancel = Cancel::new();
                b.set_preview_files(ModelRc::from(model.clone()));
                b.set_preview_mode(1);
                spawn_list(path.clone(), cancel.clone(), move |a, ev| a.on_preview_event(tab_id, c, ev));
                tab.preview = Some(Preview { entry: c, path, model, cancel });
            }
            // Elsewhere a folder previews as its icon.
            if self.pv_target != Some((tab_id, c)) {
                self.pv_target = Some((tab_id, c));
                self.pv_serial += 1;
                self.pv_pages = 0;
                b.set_pv_loading(false);
                b.set_pv_note("Folder".into());
                b.set_pv_kind(6);
            }
            return;
        }

        if columns {
            if let Some(p) = tab.preview.take() {
                p.cancel.cancel();
            }
            b.set_preview_mode(2);
        }
        if self.pv_target == Some((tab_id, c)) {
            return;
        }
        self.pv_target = Some((tab_id, c));
        // A video's tile thumbnail, if icon view already made one, shows at
        // once while the sharper frame loads.
        let quick = (card_is_video(&path)).then(|| self.thumbs.get(&thumbs::Thumbs::key(&path.to_string_lossy(), modified, self.thumbs.size()))).flatten();
        self.load_content(path, None);
        if let Some(img) = quick {
            self.pv_pending = false;
            b.set_pv_loading(false);
            b.set_pv_image(img);
            b.set_pv_kind(1);
        }
    }

    /// Builds a preview on a worker thread. `page` re-renders a PDF page.
    fn load_content(&mut self, path: PathBuf, page: Option<u32>) {
        self.pv_serial += 1;
        let serial = self.pv_serial;
        self.pv_pending = true;
        self.pv_path = Some(path.clone());
        // Render at ~1.5x the on-screen size, then box-filter down: sharp text.
        let px = self.preview_target_px();
        let px = ((px.0.max(px.1) as f32 * 1.5) as u32).clamp(600, 3200);
        self.pv_req_px = px;
        slint::Timer::single_shot(LOADING_HINT_DELAY, move || {
            with_app(|a| {
                if a.pv_serial == serial && a.pv_pending
                    && let Some(ui) = a.ui() {
                        ui.global::<Bridge>().set_pv_loading(true);
                    }
            });
        });
        std::thread::spawn(move || {
            shell::init_thread(false);
            let content = match page {
                Some(p) => preview::pdf(&path, p, px),
                None => preview::load(&path, px),
            };
            // Code colours are worked out here, off the UI thread; if the
            // highlighter ever fails the file shows as plain text.
            let code = match &content {
                Content::Text { lines, .. } => path
                    .extension()
                    .and_then(|e| highlight::for_extension(&e.to_string_lossy().to_lowercase()))
                    .and_then(|lang| {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| highlight::highlight(&lang, lines))).ok()
                    }),
                _ => None,
            };
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| a.show_content(serial, content, code));
            });
        });
    }

    fn show_content(&mut self, serial: u64, content: Content, code: Option<Vec<Vec<highlight::Span>>>) {
        if serial != self.pv_serial {
            return;
        }
        self.pv_pending = false;
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        b.set_pv_loading(false);
        b.set_pv_truncated(false);
        self.pv_pages = 0;
        self.pv_source = None;
        match content {
            Content::Image(px) => {
                b.set_pv_image(self.fitted(&px));
                self.pv_source = Some(px);
                b.set_pv_kind(1);
            }
            Content::Pdf { page, index, count } => {
                self.pv_page = index;
                self.pv_pages = count;
                b.set_pv_image(self.fitted(&page));
                self.pv_source = Some(page);
                b.set_pv_page(index as i32 + 1);
                b.set_pv_pages(count as i32);
                b.set_pv_kind(5);
            }
            Content::Text { truncated, .. } if code.is_some() => {
                let hl = code.unwrap_or_default();
                let mut cols = 0;
                let code: Vec<crate::CodeLine> = hl
                    .into_iter()
                    .enumerate()
                    .map(|(i, spans)| {
                        cols = cols.max(spans.iter().map(|(_, s)| s.chars().count()).sum::<usize>());
                        let spans: Vec<crate::CodeSpan> = spans
                            .into_iter()
                            .map(|(k, s)| crate::CodeSpan { text: s.into(), kind: k as i32 })
                            .collect();
                        crate::CodeLine { num: (i + 1).to_string().into(), spans: ModelRc::new(VecModel::from(spans)) }
                    })
                    .collect();
                b.set_pv_code_digits(code.len().max(1).to_string().len() as i32);
                b.set_pv_code_cols(cols.min(2000) as i32);
                b.set_pv_code(ModelRc::new(VecModel::from(code)));
                b.set_pv_truncated(truncated);
                b.set_pv_kind(7);
            }
            Content::Text { lines, mono, truncated } => {
                let lines: Vec<SharedString> = lines.into_iter().map(SharedString::from).collect();
                b.set_pv_lines(ModelRc::new(VecModel::from(lines)));
                b.set_pv_mono(mono);
                b.set_pv_truncated(truncated);
                b.set_pv_kind(2);
            }
            Content::Doc { blocks, truncated } => {
                let blocks: Vec<DocBlock> = blocks
                    .into_iter()
                    .map(|blk| DocBlock {
                        kind: match blk.kind {
                            BlockKind::Para => 0,
                            BlockKind::H1 => 1,
                            BlockKind::H2 => 2,
                            BlockKind::H3 => 3,
                            BlockKind::Bullet => 4,
                            BlockKind::Code => 5,
                            BlockKind::Quote => 6,
                            BlockKind::Rule => 7,
                        },
                        text: blk.text.into(),
                    })
                    .collect();
                b.set_pv_blocks(ModelRc::new(VecModel::from(blocks)));
                b.set_pv_truncated(truncated);
                b.set_pv_kind(3);
            }
            Content::Table { title, rows, truncated } => {
                let rows: Vec<TableRow> = rows
                    .into_iter()
                    .map(|cells| {
                        let cells: Vec<SharedString> = cells.into_iter().map(SharedString::from).collect();
                        TableRow { cells: ModelRc::new(VecModel::from(cells)) }
                    })
                    .collect();
                b.set_pv_table(ModelRc::new(VecModel::from(rows)));
                b.set_pv_title(title.into());
                b.set_pv_truncated(truncated);
                b.set_pv_kind(4);
            }
            Content::Nothing(note) => {
                b.set_pv_note(note.into());
                b.set_pv_kind(6);
            }
        }
    }

    /// Preview area in physical pixels.
    fn preview_target_px(&self) -> (u32, u32) {
        let scale = self.ui().map_or(1.0, |ui| ui.window().scale_factor());
        ((self.pv_box.0 * scale).max(64.0) as u32, (self.pv_box.1 * scale).max(64.0) as u32)
    }

    /// The preview image at exactly its on-screen size (the renderer doesn't
    /// smooth when scaling, so this is what keeps PDFs and photos sharp).
    fn fitted(&self, px: &shell::Pixels) -> slint::Image {
        let (w, h) = self.preview_target_px();
        thumbs::to_image(&shell::fit(px, w, h))
    }

    /// The visible preview area changed size (pane resized, Quick Look opened…).
    fn preview_box(&mut self, w: f32, h: f32) {
        if (w - self.pv_box.0).abs() < 2.0 && (h - self.pv_box.1).abs() < 2.0 {
            return;
        }
        self.pv_box = (w, h);
        self.pv_fit_serial += 1;
        let serial = self.pv_fit_serial;
        slint::Timer::single_shot(Duration::from_millis(80), move || {
            with_app(|a| {
                if a.pv_fit_serial != serial {
                    return;
                }
                let (tw, th) = a.preview_target_px();
                let Some(src) = a.pv_source.as_ref() else { return };
                // Grew beyond what was rendered: render again at the new size.
                if tw.max(th) > a.pv_req_px && (src.width < tw && src.height < th) {
                    if let Some(path) = a.pv_path.clone() {
                        let page = (a.pv_pages > 0).then_some(a.pv_page);
                        a.load_content(path, page);
                    }
                    return;
                }
                let img = a.fitted(src);
                if let Some(ui) = a.ui() {
                    ui.global::<Bridge>().set_pv_image(img);
                }
            });
        });
    }

    fn pv_page_step(&mut self, delta: i32) {
        if self.pv_pages <= 1 {
            return;
        }
        let page = (self.pv_page as i64 + delta as i64).clamp(0, self.pv_pages as i64 - 1) as u32;
        if page == self.pv_page {
            return;
        }
        self.pv_page = page;
        if let Some(path) = self.pv_path.clone() {
            self.load_content(path, Some(page));
        }
    }

    /// Quick Look's "Open" button.
    fn open_current(&mut self) {
        let Some(c) = self.model().cursor() else { return };
        if self.quicklook {
            self.run(Command::QuickLook);
        }
        self.open_target(c, false);
    }

    fn icon_size_changed(&mut self, size: f32) {
        let Some(ui) = self.ui() else { return };
        let before = self.thumbs.size();
        // Pictures are made at exactly the size they're drawn (shrinking on
        // screen would make them jagged): the gallery strip's tiles are a
        // fixed size, icon view follows the size slider.
        let size = if self.view == ViewMode::Gallery { GALLERY_THUMB } else { size };
        self.thumbs.set_icon_size(size, 16.0, ui.window().scale_factor());
        if self.thumbs.size() != before {
            // Fetch thumbnails again at the new size.
            self.tab().model.notify_all();
            self.sync_strip();
        }
    }

    fn on_thumb(&mut self, job: thumbs::Job, px: Option<shell::Pixels>) {
        if !self.thumbs.finish(&job, px) {
            return;
        }
        let Some(ti) = self.tabs.iter().position(|t| t.id == job.model_id) else { return };
        let model = &self.tabs[ti].model;
        if let Some(row) = model.row_of(job.entry) {
            model.notify_row(row);
        }
        if ti == self.active && self.view == ViewMode::Gallery {
            self.sync_strip();
        }
    }

    /// Gallery view: the strip shows a window of items around the cursor.
    fn sync_strip(&self) {
        if self.view != ViewMode::Gallery {
            return;
        }
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let model = &self.tab().model;
        let len = model.row_count();
        let cur = model.cursor_row().unwrap_or(0).min(len.saturating_sub(1));
        let start = cur.saturating_sub(STRIP_HALF);
        let end = (cur + STRIP_HALF + 1).min(len);
        let items: Vec<StripItem> =
            (start..end).filter_map(|r| Some(StripItem { index: r as i32, file: model.row_data(r)? })).collect();
        b.set_strip(ModelRc::new(VecModel::from(items)));
        b.set_strip_cursor((cur - start) as i32);
    }

    fn on_preview_event(&mut self, tab_id: u64, entry: u32, ev: ListEvent) {
        let opts = self.pane_opts();
        let Some(tab) = self.tabs.iter().find(|t| t.id == tab_id) else { return };
        let Some(p) = tab.preview.as_ref().filter(|p| p.entry == entry) else { return };
        match ev {
            ListEvent::Batch(batch) => p.model.append(batch, &opts),
            ListEvent::Done { .. } => p.model.sort(&opts),
        }
    }

    /// Click in a parent column: that folder becomes current, with the clicked
    /// item selected (and previewed). Double-click opens files.
    fn pane_clicked(&mut self, pane: usize, row: usize, activate: bool) {
        let Some(p) = self.tab().panes.get(pane) else { return };
        let (loc, model) = (p.loc.clone(), p.model.clone());
        let Some(e) = model.entry_at(row) else { return };
        let Some((name, is_dir)) = model.with_entry(e, |e| (e.name.to_string(), e.is_dir)) else { return };
        if activate && !is_dir {
            open_file(loc.child(&name));
            return;
        }
        self.keep_col_root = true;
        self.navigate(loc, Some(name));
    }

    /// Click in the preview column: step into the previewed folder.
    fn preview_clicked(&mut self, row: usize, activate: bool) {
        let Some(p) = self.tab().preview.as_ref() else { return };
        let (path, model) = (p.path.clone(), p.model.clone());
        let Some(e) = model.entry_at(row) else { return };
        let Some((name, is_dir)) = model.with_entry(e, |e| (e.name.to_string(), e.is_dir)) else { return };
        if activate && !is_dir {
            open_file(path.join(&name));
            return;
        }
        self.keep_col_root = true;
        self.navigate(Location::Dir(path), Some(name));
    }

    // ---- pushing state to the UI -------------------------------------------

    fn sync_all(&mut self) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let tab = self.tab();
        b.set_files(ModelRc::from(tab.model.clone()));
        let full = tab.loc.crumbs();
        let crumbs: Vec<Crumb> = visible_crumbs(full.len())
            .into_iter()
            .map(|i| Crumb {
                label: match (i, &tab.loc) {
                    (_, Location::Tag(t)) => self.settings.tag_name(*t as usize).into(),
                    (None, _) => "…".into(),
                    (Some(i), _) => full[i].0.as_str().into(),
                },
            })
            .collect();
        b.set_crumbs(ModelRc::new(VecModel::from(crumbs)));
        b.set_path_text(tab.loc.display().into());
        b.set_window_title(self.loc_title(&tab.loc).into());
        b.set_can_back(tab.hist_pos > 0);
        b.set_can_forward(tab.hist_pos + 1 < tab.history.len());
        b.set_can_up(tab.loc.parent().is_some());
        let current = self.places.iter().position(|p| p.loc.as_ref() == Some(&tab.loc)).map_or(-1, |i| i as i32);
        b.set_sidebar_current(current);
        self.sync_tabs();
        self.sync_view_opts();
        self.sync_status();
        self.sync_computer_page();
    }

    /// Display name of a location (tags use their custom names).
    fn loc_title(&self, loc: &Location) -> String {
        match loc {
            Location::Tag(t) => self.settings.tag_name(*t as usize),
            other => other.title(),
        }
    }

    fn sync_sidebar(&self) {
        let Some(ui) = self.ui() else { return };
        let items: Vec<SidebarItem> = self
            .places
            .iter()
            .map(|p| SidebarItem {
                label: p.label.as_str().into(),
                icon: p.icon.into(),
                header: p.loc.is_none(),
                tag: p.tag,
                favorite: p.favorite,
            })
            .collect();
        ui.global::<Bridge>().set_sidebar(ModelRc::new(VecModel::from(items)));
    }

    fn sync_tabs(&self) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let items: Vec<TabItem> = self.tabs.iter().map(|t| TabItem { title: self.loc_title(&t.loc).into() }).collect();
        b.set_tabs(ModelRc::new(VecModel::from(items)));
        b.set_active_tab(self.active as i32);
        self.sync_slots();
    }

    fn sync_view_opts(&self) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        b.set_sort_column(self.opts.sort.index());
        b.set_sort_ascending(self.opts.ascending);
        b.set_show_hidden(self.opts.show_hidden);
    }

    fn sync_status(&self) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let tab = self.tab();
        let model = &tab.model;
        let shown = model.row_count();

        let left = if tab.loading {
            let verb = if matches!(tab.loc, Location::Search { .. }) { "Searching…" } else { "Loading…" };
            format!("{verb} {} items", thousands(tab.loaded))
        } else {
            let _ = shown;
            self.selection_status()
        };
        let right = match (&self.note, tab.elapsed) {
            (Some(note), _) => note.clone(),
            (None, Some(t)) if tab.error.is_none() => format!("Listed in {} ms", t.as_millis()),
            _ => String::new(),
        };
        if b.get_status_left() != left.as_str() || b.get_status_right() != right.as_str() {
            b.set_status_serial(b.get_status_serial().wrapping_add(1));
        }
        b.set_status_left(left.into());
        b.set_status_right(right.into());
        b.set_loading(tab.loading);
        b.set_error_text(tab.error.clone().unwrap_or_default().into());
        let empty = if self.opts.filter.is_empty() {
            match &tab.loc {
                Location::Trash => "The Recycle Bin is empty.".to_string(),
                Location::Recent => "No recent files.".to_string(),
                Location::Tag(_) => "Nothing has this tag yet. Drag items onto the tag in the sidebar.".to_string(),
                Location::Search { .. } => "No matches.".to_string(),
                _ => "This folder is empty.".to_string(),
            }
        } else {
            match &tab.loc {
                Location::Search { .. } if tab.loading => "Searching…".to_string(),
                Location::Search { .. } => format!("Nothing matches “{}” here or in any subfolder.", self.live_text),
                // The subfolder search starts when typing pauses.
                Location::Dir(_) => format!("No items match “{}” in this folder.\nSearching subfolders…", self.live_text),
                _ => format!("No items match “{}” here.\nPress Enter to search your home folder.", self.live_text),
            }
        };
        // The "Search all subfolders" button: only where typing doesn't
        // already search them.
        b.set_can_search_deeper(!matches!(tab.loc, Location::Dir(_) | Location::Search { .. }));
        b.set_empty_text(empty.into());
        self.sync_selection();
    }
}

/// The user's double-click speed (Windows setting).
fn double_click_time() -> Duration {
    #[cfg(windows)]
    let ms = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() };
    #[cfg(not(windows))]
    let ms = 500;
    Duration::from_millis(ms.clamp(200, 900) as u64)
}

/// Opens a file in its default app on a worker thread; errors go to the status bar.
fn open_file(path: PathBuf) {
    std::thread::spawn(move || {
        if let Err(msg) = sys::shell_open(&path) {
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| {
                    a.note = Some(msg);
                    a.sync_status();
                });
            });
        }
    });
}

/// Lists `path` in the background and feeds each event to `on_event` on the
/// UI thread, unless `cancel` was triggered in the meantime.
fn spawn_list<F>(path: PathBuf, cancel: Cancel, on_event: F)
where
    F: Fn(&mut App, ListEvent) + Send + Clone + 'static,
{
    let token = cancel.clone();
    fs_list::list_dir(path, cancel, move |ev| {
        let token = token.clone();
        let f = on_event.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if !token.is_cancelled() {
                with_app(|a| f(a, ev));
            }
        });
    });
}

/// Selects the entry called `name` (only that one); returns its row or -1.
fn select_name(model: &FileModel, name: &str) -> i32 {
    match model.find_name(name).and_then(|e| model.row_of(e)) {
        Some(row) => {
            model.select_only(row);
            row as i32
        }
        None => {
            model.select_where(|_| false);
            -1
        }
    }
}

/// Which breadcrumbs to show: all of them for short paths; otherwise the first
/// two, a "…" (`None`), and the last three, so the current folder stays readable.
fn visible_crumbs(len: usize) -> Vec<Option<usize>> {
    if len <= 6 {
        return (0..len).map(Some).collect();
    }
    vec![Some(0), Some(1), None, Some(len - 3), Some(len - 2), Some(len - 1)]
}

#[cfg(test)]
mod tests {
    use super::visible_crumbs;

    #[test]
    fn crumbs_collapse() {
        assert_eq!(visible_crumbs(3), [Some(0), Some(1), Some(2)]);
        assert_eq!(visible_crumbs(9), [Some(0), Some(1), None, Some(6), Some(7), Some(8)]);
    }
}
