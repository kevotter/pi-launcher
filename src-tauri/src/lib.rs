//! Pi Launcher — Tauri v2 port.
//!
//! The Rust side owns configuration, backend lifecycle, window and tray.
//! The bundled frontend has a loading page and a compiled Vue tray. Pi Web
//! itself is an external origin with no IPC permissions granted to it.

mod autostart;
mod backend;
mod config;
mod http;
mod process;
mod tray_layout;
mod util;
mod window_state;

use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{
    App, AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    WindowEvent, Wry,
};
use tauri_plugin_positioner::{Position, WindowExt};

use backend::{PiBackend, ServerInfo};
use util::{err, lock, log, log_error, resolve_data_dir, APP_NAME};

/// Label of the frameless tray popover window.
const TRAY_WINDOW: &str = "tray-menu";
/// Command line switch written into the login-startup registration so a login
/// launch goes straight to the tray instead of stealing focus.
pub(crate) const START_HIDDEN_ARG: &str = "--tray";
/// Bounded wait for a worker to notice an unexpected backend exit.
const WATCH_INTERVAL: Duration = Duration::from_millis(250);
/// Liveness probe period and the number of consecutive misses that count as
/// "running but not answering". One miss is normal while a request saturates
/// the backend; two turn the tray badge red instead of flapping every minute.
const HEALTH_INTERVAL: Duration = Duration::from_secs(15);
const HEALTH_STRIKES: u32 = 2;
/// How long after opening the popover its own blur is ignored.
const TRAY_GRACE: Duration = Duration::from_millis(300);
/// Host used by the popover to signal actions without any Tauri IPC permission.
const TRAY_ACTION_HOST: &str = "pi-launcher.tray";
/// Held only by worker threads. A caller racing another build waits for that
/// window instead of losing an early request to show the menu.
static TRAY_CREATION: Mutex<()> = Mutex::new(());
/// Bound early right-click workers while the popover is still being built.
static TRAY_OPEN_PENDING: AtomicBool = AtomicBool::new(false);

/// Injected into every document. Keyboard zoom is signalled with window.open so
/// the remote Pi Web page needs no Tauri IPC permissions at all.
const ZOOM_SCRIPT: &str = r#"
(function () {
  document.addEventListener('keydown', function (event) {
    if (!event.ctrlKey || event.altKey || event.metaKey) return;
    var action = null;
    if (event.key === '+' || event.key === '=') action = 'in';
    else if (event.key === '-') action = 'out';
    else if (event.key === '0') action = 'reset';
    if (!action) return;
    event.preventDefault();
    event.stopPropagation();
    try { window.open('https://pi-launcher.zoom/' + action, '_blank'); } catch (error) {}
  }, true);
})();
"#;

fn internal_origin(windows: bool) -> (&'static str, &'static str) {
    if windows {
        ("http", "tauri.localhost")
    } else {
        ("tauri", "localhost")
    }
}

/// Match the actual bundled origin, not just a scheme or hostname. In
/// particular, HTTPS, custom ports, userinfo and the asset protocol are not
/// trusted. `Url` normalizes an explicit HTTP default port (80) to `None`.
fn is_internal_url_for_platform(url: &Url, windows: bool) -> bool {
    let (scheme, host) = internal_origin(windows);
    url.scheme() == scheme
        && url.host_str() == Some(host)
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
}

fn is_internal_url(url: &Url) -> bool {
    is_internal_url_for_platform(url, cfg!(windows))
}

fn is_loading_url(url: &Url) -> bool {
    is_internal_url(url) && matches!(url.path(), "/" | "/index.html")
}

fn is_tray_url(url: &Url) -> bool {
    is_internal_url(url) && url.path() == "/tray.html"
}

fn is_external_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
}

fn is_server_url(url: &Url, server_url: &str) -> bool {
    is_external_url(url)
        && Url::parse(server_url)
            .ok()
            .filter(is_external_url)
            .map(|base| base.origin() == url.origin())
            .unwrap_or(false)
}

/// Parse our window.open signals strictly; lookalike hosts, schemes, ports,
/// credentials, nested paths and URL suffixes must never become UI actions.
fn action_from_url<'a>(url: &'a Url, host: &str) -> Option<&'a str> {
    if url.scheme() != "https"
        || url.host_str() != Some(host)
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let action = url.path().strip_prefix('/')?;
    if action.is_empty() || action.contains('/') {
        None
    } else {
        Some(action)
    }
}

/// A single OS-opener worker with a small, nonblocking queue. Neither a slow
/// shell association nor repeated web content callbacks can grow a thread pool.
enum OpenTarget {
    Browser(String),
    Directory(PathBuf),
}

fn start_opener(state_dir: &Path) -> Option<mpsc::SyncSender<OpenTarget>> {
    let (sender, receiver) = mpsc::sync_channel::<OpenTarget>(4);
    let worker_dir = state_dir.to_path_buf();
    match std::thread::Builder::new()
        .name("os-opener".into())
        .spawn(move || {
            while let Ok(target) = receiver.recv() {
                let (result, failure) = match target {
                    OpenTarget::Browser(url) => (open::that(url), "Cannot open browser"),
                    OpenTarget::Directory(path) => (open::that(path), "Cannot open directory"),
                };
                if result.is_err() {
                    // OS opener errors may embed their command line, including
                    // URL credentials/query secrets. Never log the error/URL.
                    log(&worker_dir, failure);
                }
            }
        }) {
        Ok(_) => Some(sender),
        Err(_) => {
            log(state_dir, "Cannot start OS opener worker");
            None
        }
    }
}

fn queue_open(context: &Context, target: OpenTarget) {
    if let Some(opener) = &context.opener {
        if opener.try_send(target).is_err() {
            log(
                &context.state_dir,
                "OS opener request dropped: queue full or worker stopped",
            );
        }
    } else {
        log(&context.state_dir, "OS opener unavailable");
    }
}

/// What the startup thread is asked to do while it waits.
enum Command {
    /// Stop the backend (if we own it) and end the process.
    Quit,
    /// Stop our backend instance and start a fresh one, then reload the page.
    Restart,
}

/// Startup options from the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Options {
    /// Started by the login-startup entry: create the window but keep it hidden
    /// so logging in does not open a full-size app window.
    hidden: bool,
}

impl Options {
    fn from_args(args: impl IntoIterator<Item = String>) -> Self {
        Self {
            // Unknown arguments are ignored rather than fatal: a stale shortcut
            // must never turn into "double-clicking does nothing".
            hidden: args
                .into_iter()
                .any(|arg| arg == START_HIDDEN_ARG || arg == "/tray"),
        }
    }
}

#[derive(Clone)]
struct Context {
    state_dir: Arc<PathBuf>,
    /// WebView2 profile directory. Deliberately **not** under `state_dir`: a
    /// Chromium profile is hundreds of MB of cache and must not live in the
    /// roaming AppData tree.
    webview_dir: Arc<PathBuf>,
    opener: Option<mpsc::SyncSender<OpenTarget>>,
    /// Refreshed only when opening the tray or clicking its startup switch.
    autostart: Arc<Mutex<autostart::Status>>,
    /// "Stop doing work": set when a quit is requested or startup has failed.
    closed: Arc<AtomicBool>,
    /// "A quit has already been requested", so the tray cannot queue two.
    quitting: Arc<AtomicBool>,
    zoom: Arc<Mutex<f64>>,
    server: Arc<Mutex<Option<ServerInfo>>>,
    shutdown: Arc<Mutex<Option<mpsc::Sender<Command>>>>,
    /// Last phase pushed to the loading page. Replayed on every page load so a
    /// phase emitted before the document was ready is never lost.
    last_phase: Arc<Mutex<Option<serde_json::Value>>>,
    /// Startup error, if any. Kept here (rather than only in the message box)
    /// so the loading page can show it and a late page load cannot overwrite it
    /// with a stale phase.
    last_error: Arc<Mutex<Option<ErrorReport>>>,
    /// Timestamps used to ignore the blur caused by showing the popover and to
    /// distinguish "click the tray icon again to dismiss" from a fresh open.
    tray_shown_at: Arc<Mutex<Option<Instant>>>,
    tray_hidden_at: Arc<Mutex<Option<Instant>>>,
    /// Last natural content measurement; None keeps the first opening hidden.
    tray_size: Arc<Mutex<Option<tray_layout::Size>>>,
    tray_open_requested: Arc<AtomicBool>,
    /// Remembered window geometry and zoom.
    window: window_state::Store,
    /// A start attempt is in progress, so "重启" must not queue a second one.
    starting: Arc<AtomicBool>,
    /// The backend is gone (crashed or failed to start) but the launcher is
    /// still alive and can restart it.
    stopped: Arc<AtomicBool>,
    /// Bumped for every start attempt. A watcher belongs to one generation, so
    /// an intentional restart cannot be reported as a crash.
    generation: Arc<AtomicU64>,
    /// Last liveness probe result. Only meaningful while `server` is set.
    healthy: Arc<AtomicBool>,
    /// Wall-clock start of the current backend, for the tray's uptime line.
    started_at: Arc<Mutex<Option<SystemTime>>>,
    /// The remembered window was maximized, but the window was created hidden
    /// (login launch). Applied the first time it is shown.
    pending_maximize: Arc<AtomicBool>,
}

/// A message for the loading page. The heading differs between "cannot start"
/// and "was running, then exited"; both keep the reason and the log path.
#[derive(Clone)]
struct ErrorReport {
    heading: &'static str,
    hint: &'static str,
    message: String,
}

impl Context {
    fn new(
        state_dir: PathBuf,
        webview_dir: PathBuf,
        window: window_state::Store,
        zoom: f64,
    ) -> Self {
        Self {
            opener: start_opener(&state_dir),
            state_dir: Arc::new(state_dir),
            webview_dir: Arc::new(webview_dir),
            autostart: Arc::new(Mutex::new(autostart::Status::unknown(
                "打开菜单后读取登录自启状态。",
            ))),
            closed: Arc::new(AtomicBool::new(false)),
            quitting: Arc::new(AtomicBool::new(false)),
            zoom: Arc::new(Mutex::new(zoom)),
            server: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(Mutex::new(None)),
            last_phase: Arc::new(Mutex::new(None)),
            last_error: Arc::new(Mutex::new(None)),
            tray_shown_at: Arc::new(Mutex::new(None)),
            tray_hidden_at: Arc::new(Mutex::new(None)),
            tray_size: Arc::new(Mutex::new(None)),
            tray_open_requested: Arc::new(AtomicBool::new(false)),
            window,
            starting: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
            healthy: Arc::new(AtomicBool::new(false)),
            started_at: Arc::new(Mutex::new(None)),
            pending_maximize: Arc::new(AtomicBool::new(false)),
        }
    }
}

pub fn run() {
    util::mark_start();
    let options = Options::from_args(std::env::args().skip(1));
    let state_dir = match resolve_data_dir() {
        Ok(directory) => directory,
        Err(error) => {
            process::show_message_box(APP_NAME, &error.to_string());
            return;
        }
    };
    log(&state_dir, "Launcher starting");
    let webview_dir = util::resolve_webview_dir(&state_dir);
    let (window_store, saved_window) = window_state::Store::open(&state_dir);
    window_store.spawn_writer();
    let context = Context::new(
        state_dir.clone(),
        webview_dir,
        window_store,
        saved_window.map(|saved| saved.zoom).unwrap_or(1.0),
    );
    let setup_context = context.clone();

    let built = tauri::Builder::default()
        // Must be the first plugin registered.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // A silent login launch (`--tray`) must never pull the existing
            // window to the front; a normal second launch still shows it.
            if !Options::from_args(args).hidden {
                show_app_window(app);
            }
        }))
        // Required for tray-relative positions: on_tray_event stores the icon
        // rect in this plugin's managed state.
        .plugin(tauri_plugin_positioner::init())
        .setup(move |app| {
            app.manage(setup_context.clone());
            log(&setup_context.state_dir, "Setup begin");
            if options.hidden {
                log(
                    &setup_context.state_dir,
                    "Login-startup launch: window stays hidden",
                );
            }
            let placement = window_state::place(saved_window, &monitors(app));

            // Backend first, window second. Creating the WebView2 controller
            // costs well over a second; the startup thread only needs the main
            // window at the very end (to navigate), and waits for it there.
            let (sender, receiver) = mpsc::channel();
            *lock(&setup_context.shutdown) = Some(sender);
            let thread_context = setup_context.clone();
            let handle = app.handle().clone();
            std::thread::spawn(move || startup(thread_context, handle, receiver));
            log(&setup_context.state_dir, "Startup thread spawned");

            build_window(app, &setup_context, placement, options.hidden)?;
            log(&setup_context.state_dir, "Main window created");

            build_tray(app)?;
            log(&setup_context.state_dir, "Tray icon ready");
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Persist the normal geometry only. A maximized window reports the
            // maximized rect, and storing that would restore a "normal" window
            // the size of the screen after the user un-maximizes.
            WindowEvent::Resized(_) | WindowEvent::Moved(_) if window.label() == "main" => {
                if let Some(context) = window.app_handle().try_state::<Context>() {
                    remember_geometry(window, &context);
                }
            }
            // Closing the main window hides it; the backend keeps running.
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            // A popover behaves like a native context menu: it closes as soon
            // as it loses focus. Ignore the blur emitted right after showing.
            WindowEvent::Focused(false) if window.label() == TRAY_WINDOW => {
                let app = window.app_handle().clone();
                let Some(context) = app.try_state::<Context>() else {
                    return;
                };
                let shown_at = *lock(&context.tray_shown_at);
                let settled = shown_at
                    .map(|at| at.elapsed() >= TRAY_GRACE)
                    .unwrap_or(true);
                if settled {
                    hide_tray_menu(&app, &context);
                    return;
                }
                // Showing the popover emits a blur of its own, so hiding right
                // away would make it impossible to click. Ignoring the event
                // outright is just as wrong in the other direction: clicking
                // anywhere else inside the grace window used to leave the
                // always-on-top popover stuck on screen until the tray icon was
                // right-clicked again. Re-check once, when the window has had
                // its chance to take focus.
                let context = context.inner().clone();
                std::thread::spawn(move || {
                    if let Some(at) = shown_at {
                        let wait = TRAY_GRACE.saturating_sub(at.elapsed());
                        std::thread::sleep(wait + Duration::from_millis(20));
                    }
                    // A later open resets the timestamp; only act on the one
                    // this event belonged to.
                    if *lock(&context.tray_shown_at) != shown_at {
                        return;
                    }
                    if let Some(window) = app.get_webview_window(TRAY_WINDOW) {
                        if window.is_visible().unwrap_or(false)
                            && !window.is_focused().unwrap_or(true)
                        {
                            hide_tray_menu(&app, &context);
                        }
                    }
                });
            }
            _ => {}
        })
        .build(tauri::generate_context!());
    // A panic here used to be invisible: release builds are `windows_subsystem
    // = "windows"`, so the only symptom was "double-clicking does nothing".
    // The usual cause is a missing or broken WebView2 runtime.
    let app = match built {
        Ok(app) => app,
        Err(error) => {
            log_error(&state_dir, "Cannot build application", &error);
            process::show_message_box(
                APP_NAME,
                &format!(
                    "无法创建应用窗口：{error}\n\n\
                     常见原因：没有安装 Microsoft Edge WebView2 运行时，或数据目录不可写。\n\
                     日志：{}",
                    state_dir.join("launcher.log").display()
                ),
            );
            return;
        }
    };

    app.run(|handle, event| {
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = &event
        {
            api.prevent_exit();
            if let Some(state) = handle.try_state::<Context>() {
                request_quit(&state, handle);
            }
        }
    });
}

/// The startup page's own paper colour. Kept in sync with `ui/index.html` by
/// hand; it is only on screen for a frame or two before the page paints over
/// it, and the page is light in every Windows theme, so this is not themed.
fn window_background() -> tauri::window::Color {
    tauri::window::Color(0xfd, 0xf3, 0xf1, 0xff)
}

/// Work areas of every monitor, in physical pixels.
fn monitors(app: &App) -> Vec<window_state::Monitor> {
    let Ok(available) = app.available_monitors() else {
        return Vec::new();
    };
    available
        .into_iter()
        .filter_map(|monitor| {
            let area = monitor.work_area();
            let scale = monitor.scale_factor();
            (area.size.width > 0 && area.size.height > 0).then_some(window_state::Monitor {
                left: area.position.x,
                top: area.position.y,
                width: area.size.width,
                height: area.size.height,
                scale,
            })
        })
        .collect()
}

fn build_window(
    app: &mut App,
    context: &Context,
    placement: window_state::Placement,
    hidden: bool,
) -> tauri::Result<()> {
    let navigation_context = context.clone();
    let new_window_context = context.clone();
    let new_window_handle = app.handle().clone();
    let page_load_context = context.clone();

    let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title(APP_NAME)
        .inner_size(placement.width, placement.height)
        .min_inner_size(900.0, 600.0)
        // Paint the window in the loading page's own canvas colour. Without
        // this the window shows a plain white surface for the ~1 s WebView2
        // needs to produce its first frame, which reads as a flash in dark mode
        // and as a "the app is broken" blank window in light mode.
        .background_color(window_background())
        // Keep the WebView2 profile out of the roaming tree; it is a cache and
        // is large enough to make profile sync fail.
        .data_directory(context.webview_dir.as_ref().clone())
        // Native file-drop interception consumes WebView2's HTML5 drag/drop
        // events. Pi Web needs the standard DOM drop path instead.
        .disable_drag_drop_handler()
        .initialization_script(ZOOM_SCRIPT)
        .on_document_title_changed(|window, _title| {
            let _ = window.set_title(APP_NAME);
        })
        .on_page_load(move |window, payload| {
            let zoom = *lock(&page_load_context.zoom);
            let _ = window.set_zoom(zoom);
            // Only the bundled loading page understands the phase payload; the
            // remote Pi Web document must stay untouched.
            let internal = is_loading_url(payload.url());
            // Replay once the document is parsed. `Started` is WebView2's
            // ContentLoading, which fires before the DOM exists — `document.body`
            // is still null there, so the old replay threw and rendered nothing.
            if internal && payload.event() == PageLoadEvent::Finished {
                // The only milestone between "window exists" and "backend ready"
                // that a user can actually see. Without it, the log cannot tell
                // an early window with nothing painted from a healthy startup.
                log(
                    &page_load_context.state_dir,
                    &format!("Page rendered: {}", payload.url()),
                );
                let error = lock(&page_load_context.last_error).clone();
                match error {
                    Some(report) => apply_error(&window, &report),
                    None => {
                        let phase = lock(&page_load_context.last_phase).clone();
                        if let Some(phase) = phase {
                            apply_phase(&window, &phase);
                        }
                    }
                }
            }
        })
        .on_navigation(move |url| {
            // The bundled loading page lives on the internal origin.
            if is_loading_url(url) {
                return true;
            }
            // Other bundled documents (notably tray.html) do not belong in
            // the main window, and must not be sent to the system browser.
            if is_internal_url(url) || !is_external_url(url) {
                return false;
            }
            let allowed = lock(&navigation_context.server)
                .as_ref()
                .map(|server| is_server_url(url, &server.url))
                .unwrap_or(false);
            if allowed {
                true
            } else {
                open_external(url.as_str(), &navigation_context);
                false
            }
        })
        .on_new_window(move |url, _features| {
            if let Some(action) = action_from_url(&url, "pi-launcher.zoom") {
                if matches!(action, "in" | "out" | "reset") {
                    apply_zoom(&new_window_handle, &new_window_context, action);
                }
            } else if !matches!(url.host_str(), Some("pi-launcher.zoom" | TRAY_ACTION_HOST)) {
                open_external(url.as_str(), &new_window_context);
            }
            NewWindowResponse::Deny
        })
        .visible(!hidden)
        // Leave the placement to `set_position` below: the builder only accepts
        // logical coordinates, which cannot express a position on a monitor
        // whose DPI differs from the primary one.
        .center()
        .build()?;
    if let Some((x, y)) = placement.position {
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    }
    if placement.maximized {
        if hidden {
            // `maximize` shows the window, which would defeat a login launch.
            // Applied by `show_window` the first time the user asks for it.
            context.pending_maximize.store(true, Ordering::SeqCst);
        } else {
            let _ = window.maximize();
        }
    }
    Ok(())
}

/// The tray "menu" is a real, frameless webview window so it can be styled
/// with CSS. It is created hidden, on demand, and only ever from a background
/// thread: window creation blocks until the main thread answers, so calling it
/// from the main thread (setup or a tray event) would stall or deadlock.
fn ensure_tray_menu_window(app: &AppHandle) -> bool {
    if app.get_webview_window(TRAY_WINDOW).is_some() {
        return true;
    }
    let _creation = lock(&TRAY_CREATION);
    // Another worker may have completed its build after our first lookup.
    if app.get_webview_window(TRAY_WINDOW).is_some() {
        return true;
    }
    // Same profile as the main window on purpose: a second, implicit profile
    // would live in the default WebView2 location with its own locks and cache.
    let Some(webview_dir) = app
        .try_state::<Context>()
        .map(|context| context.webview_dir.as_ref().clone())
    else {
        return false;
    };
    let action_app = app.clone();
    let built = WebviewWindowBuilder::new(app, TRAY_WINDOW, WebviewUrl::App("tray.html".into()))
        .title(APP_NAME)
        .data_directory(webview_dir)
        .inner_size(f64::from(tray_layout::DEFAULT_WIDTH), 440.0)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(false)
        .visible(false)
        .focused(false)
        .on_navigation(|url| {
            // Only the bundled page may load here. Everything else is denied.
            is_tray_url(url)
        })
        .on_page_load(|window, payload| {
            // The Vue entry also explicitly requests replay after mount, since
            // native page completion can race module initialization.
            if payload.event() == PageLoadEvent::Finished && is_tray_url(payload.url()) {
                update_tray_state(window.app_handle());
            }
        })
        .on_new_window(move |url, _features| {
            if let Some(action) = action_from_url(&url, TRAY_ACTION_HOST) {
                if let Some(context) = action_app.try_state::<Context>() {
                    handle_tray_action(&action_app, &context, action);
                }
            }
            NewWindowResponse::Deny
        })
        .build();
    match built {
        Ok(_) => {
            if let Some(context) = app.try_state::<Context>() {
                log(&context.state_dir, "Tray popover created");
            }
            true
        }
        Err(error) => {
            if let Some(context) = app.try_state::<Context>() {
                log_error(&context.state_dir, "Cannot create tray popover", &error);
            }
            false
        }
    }
}

/// Push the current status into the popover. `eval` is a Rust-side API, so the
/// popover needs no IPC capability at all.
fn update_tray_state(app: &AppHandle) {
    let Some(context) = app.try_state::<Context>() else {
        return;
    };
    // Only what the popover actually renders. The address and version used to
    // be pushed for a header row the menu no longer has; copying the address
    // stays available through the `copy` action.
    let ready = lock(&context.server).is_some();
    let autostart = lock(&context.autostart).clone();
    let starting = context.starting.load(Ordering::SeqCst);
    let started_at = lock(&context.started_at)
        .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis() as u64);
    let max_height = app
        .get_webview_window(TRAY_WINDOW)
        .map(|window| tray_available_size(&window).height)
        .unwrap_or(tray_layout::MAX_HEIGHT);
    let payload = serde_json::json!({
        "autostart": autostart,
        "maxHeight": max_height,
        "ready": ready,
        "healthy": context.healthy.load(Ordering::SeqCst),
        "starting": starting,
        "stopped": context.stopped.load(Ordering::SeqCst),
        "startedAt": started_at,
        "canRestart": !starting && !context.quitting.load(Ordering::SeqCst),
    });
    let script = format!("if (window.__setTrayState) window.__setTrayState({payload});");
    if let Some(window) = app.get_webview_window(TRAY_WINDOW) {
        let _ = window.eval(script);
    }
}

fn build_tray(app: &mut App) -> tauri::Result<()> {
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip(APP_NAME)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            // Forward every event so positioner learns the icon's rect.
            tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
            let TrayIconEvent::Click {
                button,
                button_state: MouseButtonState::Up,
                ..
            } = event
            else {
                return;
            };
            let app = tray.app_handle();
            match button {
                MouseButton::Left => show_app_window(app),
                MouseButton::Right => {
                    if let Some(context) = app.try_state::<Context>() {
                        toggle_tray_menu(app, &context);
                    }
                }
                MouseButton::Middle => {}
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn toggle_tray_menu(app: &AppHandle, context: &Context) {
    // A second click while the first content measurement is pending cancels it.
    if context.tray_open_requested.swap(false, Ordering::SeqCst) {
        return;
    }
    let Some(window) = app.get_webview_window(TRAY_WINDOW) else {
        // Popover is normally created right after the main page starts
        // loading; this only happens on a very early right-click. Build it on a
        // worker thread, then open it. Only one such request may wait on the
        // creation lock, even if the icon is clicked repeatedly.
        if TRAY_OPEN_PENDING.swap(true, Ordering::SeqCst) {
            return;
        }
        let app = app.clone();
        std::thread::spawn(move || {
            if ensure_tray_menu_window(&app) {
                if let (Some(context), Some(window)) = (
                    app.try_state::<Context>(),
                    app.get_webview_window(TRAY_WINDOW),
                ) {
                    if !context.quitting.load(Ordering::SeqCst) {
                        show_tray_menu(&app, &context, &window);
                    }
                }
            }
            TRAY_OPEN_PENDING.store(false, Ordering::SeqCst);
        });
        return;
    };
    if window.is_visible().unwrap_or(false) {
        hide_tray_menu(app, context);
        return;
    }
    // Right-clicking the icon first blurs (and hides) the popover. Treat that
    // same click as a dismiss instead of immediately reopening it.
    let just_hidden = lock(&context.tray_hidden_at)
        .map(|at| at.elapsed() < Duration::from_millis(400))
        .unwrap_or(false);
    if just_hidden {
        return;
    }
    show_tray_menu(app, context, &window);
}

fn tray_available_size(window: &WebviewWindow<Wry>) -> tray_layout::Size {
    window
        .current_monitor()
        .ok()
        .flatten()
        .and_then(|monitor| {
            let area = monitor.work_area();
            tray_layout::Size::from_work_area(
                area.size.width,
                area.size.height,
                monitor.scale_factor(),
            )
        })
        .unwrap_or(tray_layout::Size {
            width: tray_layout::MAX_WIDTH,
            height: tray_layout::MAX_HEIGHT,
        })
}

fn resize_tray_menu(
    window: &WebviewWindow<Wry>,
    context: &Context,
    natural: tray_layout::Size,
) -> bool {
    let fitted = natural.fit(tray_available_size(window));
    if let Err(error) = window.set_size(tauri::LogicalSize::new(fitted.width, fitted.height)) {
        log_error(&context.state_dir, "Cannot fit tray content", &error);
        return false;
    }
    true
}

fn position_tray_menu(window: &WebviewWindow<Wry>, context: &Context) {
    let positioned = window
        .move_window_constrained(Position::TrayCenter)
        .or_else(|_| window.move_window(Position::TrayBottomCenter))
        .or_else(|_| window.move_window(Position::BottomRight));
    if let Err(error) = positioned {
        log_error(&context.state_dir, "Cannot position tray menu", &error);
        return;
    }
    // Positioner's constraint uses the full monitor. Also respect the work area
    // so a large-font fallback cannot overlap the taskbar or leave Quit offscreen.
    if let (Ok(Some(monitor)), Ok(position), Ok(size)) = (
        window.current_monitor(),
        window.outer_position(),
        window.outer_size(),
    ) {
        let area = monitor.work_area();
        let left = i64::from(area.position.x);
        let top = i64::from(area.position.y);
        let right = (left + i64::from(area.size.width) - i64::from(size.width)).max(left);
        let bottom = (top + i64::from(area.size.height) - i64::from(size.height)).max(top);
        let x = i64::from(position.x).clamp(left, right) as i32;
        let y = i64::from(position.y).clamp(top, bottom) as i32;
        if x != position.x || y != position.y {
            let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
        }
    }
}

fn show_tray_menu(app: &AppHandle, context: &Context, window: &WebviewWindow<Wry>) {
    if context.quitting.load(Ordering::SeqCst) {
        return;
    }
    context.tray_open_requested.store(true, Ordering::SeqCst);
    // Move the hidden window to the tray's monitor BEFORE converting its work
    // area to logical pixels. Stored measurements survive subsequent openings.
    position_tray_menu(window, context);
    let measured = *lock(&context.tray_size);
    if let Some(size) = measured {
        if !resize_tray_menu(window, context, size) {
            context.tray_open_requested.store(false, Ordering::SeqCst);
            return;
        }
        position_tray_menu(window, context);
    }
    // Read-only: opening the menu never opts the user into login startup.
    // Serialize with toggles so an older read cannot overwrite their result.
    {
        let mut status = lock(&context.autostart);
        *status = autostart::status();
    }
    update_tray_state(app);
    if measured.is_none() {
        // No guessed fixed-height flash on the first opening. The Vue bridge
        // measures even while this native window is hidden, then retries show.
        let _ = window.eval("if (window.__measureTray) void window.__measureTray();");
        return;
    }
    if !context.tray_open_requested.swap(false, Ordering::SeqCst) {
        return;
    }
    let _ = window.show();
    let _ = window.set_focus();
    *lock(&context.tray_shown_at) = Some(Instant::now());
}

fn hide_tray_menu(app: &AppHandle, context: &Context) {
    context.tray_open_requested.store(false, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window(TRAY_WINDOW) {
        let _ = window.hide();
    }
    *lock(&context.tray_hidden_at) = Some(Instant::now());
}

fn handle_tray_action(app: &AppHandle, context: &Context, action: &str) {
    // This route exists only on the bundled tray webview, never on Pi Web.
    if action.starts_with("resize-") {
        if let (Some(size), Some(window)) = (
            tray_layout::Size::from_action(action),
            app.get_webview_window(TRAY_WINDOW),
        ) {
            if resize_tray_menu(&window, context, size) {
                *lock(&context.tray_size) = Some(size);
                if context.tray_open_requested.load(Ordering::SeqCst) {
                    show_tray_menu(app, context, &window);
                } else if window.is_visible().unwrap_or(false) {
                    position_tray_menu(&window, context);
                }
                // Limit-only feedback must not release a pending startup click.
                let height = tray_available_size(&window).height;
                let _ = window.eval(format!(
                    "if (window.__setTrayState) window.__setTrayState({{maxHeight:{height}}});"
                ));
            }
        }
        return;
    }
    // State handshakes and the startup switch never dismiss the menu.
    let keep_open = matches!(action, "autostart-toggle" | "ready");
    if !keep_open {
        hide_tray_menu(app, context);
    }
    match action {
        "ready" => update_tray_state(app),
        "show" => show_app_window(app),
        "browser" => {
            let url = lock(&context.server).as_ref().map(|s| s.url.clone());
            if let Some(url) = url {
                open_external(&url, context);
            }
        }
        "copy" => {
            let url = lock(&context.server).as_ref().map(|s| s.url.clone());
            if let Some(url) = url {
                copy_server_url(app, context, &url);
            }
        }
        "config-dir" | "log-dir" => {
            queue_open(
                context,
                OpenTarget::Directory(context.state_dir.as_ref().clone()),
            );
        }
        "autostart-toggle" => {
            // This dispatch is reachable only from the bundled tray window's
            // on_new_window handler, never from the remote main document.
            let mut status = lock(&context.autostart);
            *status = autostart::toggle();
            drop(status);
            update_tray_state(app);
        }
        "restart" => request_restart(context),
        "quit" => request_quit(context, app),
        "dismiss" => {}
        _ => {}
    }
}

fn apply_zoom(app: &AppHandle, context: &Context, action: &str) {
    // Keyboard zoom remains scoped to the main window, not the tray menu.
    let zoom = {
        let mut current = lock(&context.zoom);
        let next = match action {
            "in" => *current + 0.05,
            "out" => *current - 0.05,
            "reset" => 1.0,
            "125" => 1.25,
            _ => return,
        };
        *current = (next.clamp(0.75, 2.0) * 100.0).round() / 100.0;
        *current
    };
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_zoom(zoom);
    }
    context.window.record_zoom(zoom);
}

fn show_app_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        show_window(&window);
    }
}

/// The main window is created on the main thread; this thread may get here
/// first when the backend starts quickly.
fn wait_for_main_window(app: &AppHandle, timeout: Duration) -> Option<WebviewWindow<Wry>> {
    let started = Instant::now();
    loop {
        if let Some(window) = app.get_webview_window("main") {
            return Some(window);
        }
        if started.elapsed() > timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn show_window(window: &tauri::WebviewWindow<Wry>) {
    if let Some(context) = window.app_handle().try_state::<Context>() {
        // A maximized login-start window was created hidden on purpose; apply
        // the remembered state now that the user actually asked for it.
        if context.pending_maximize.swap(false, Ordering::SeqCst) {
            let _ = window.maximize();
        }
    }
    if window.is_minimized().unwrap_or(false) {
        let _ = window.unminimize();
    }
    let _ = window.show();
    let _ = window.set_focus();
}

/// Debounced by `window_state::Store`; this only copies a few numbers.
fn remember_geometry(window: &tauri::Window<Wry>, context: &Context) {
    let maximized = window.is_maximized().unwrap_or(false);
    context.window.record_maximized(maximized);
    if maximized {
        return;
    }
    let (Ok(position), Ok(size), Ok(scale)) = (
        window.outer_position(),
        window.inner_size(),
        window.scale_factor(),
    ) else {
        return;
    };
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    // Logical pixels, so the window keeps its perceived size when the saved
    // position lands on a monitor with a different DPI.
    let width = (f64::from(size.width) / scale).round();
    let height = (f64::from(size.height) / scale).round();
    if !width.is_finite() || !height.is_finite() || width < 1.0 || height < 1.0 {
        return;
    }
    context
        .window
        .record_geometry(position.x, position.y, width as u32, height as u32);
}

fn open_external(url: &str, context: &Context) {
    // Validate before queuing: invalid schemes cannot consume worker capacity
    // or invoke arbitrary system protocol handlers.
    if let Ok(url) = Url::parse(url) {
        if is_external_url(&url) && !is_internal_url(&url) {
            queue_open(context, OpenTarget::Browser(url.into()));
        }
    }
}

fn copy_server_url(app: &AppHandle, context: &Context, url: &str) {
    #[cfg(windows)]
    {
        // EmptyClipboard requires a real owner HWND for SetClipboardData to
        // succeed. Prefer the long-lived main window; the tray is a fallback.
        let owner = ["main", TRAY_WINDOW].iter().find_map(|label| {
            app.get_webview_window(label)
                .and_then(|window| window.hwnd().ok())
                .map(|handle| handle.0 as windows_sys::Win32::Foundation::HWND)
                .filter(|handle| !handle.is_null())
        });
        match owner {
            Some(owner) => {
                if !process::set_clipboard(owner, url) {
                    log(
                        &context.state_dir,
                        "Cannot copy server address to clipboard",
                    );
                }
            }
            None => log(
                &context.state_dir,
                "Cannot copy server address: no owner window",
            ),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (app, url);
        log(&context.state_dir, "Clipboard is only supported on Windows");
    }
}

/// Push the current startup phase into the loading page.
///
/// The page explains progress with a status line, a three-step list and the
/// loopback address it is about to open, so every payload carries all three;
/// `data-phase` alone also drives the failure layout.
fn emit_phase(app: &AppHandle, phase: &str, port: u16) {
    let status = match phase {
        "environment" => "正在检查运行环境",
        "backend" => "正在连接本地服务",
        _ => "正在打开工作空间",
    };
    let payload = serde_json::json!({
        "phase": phase,
        "status": status,
        "endpoint": format!("127.0.0.1:{port}"),
    });
    if let Some(context) = app.try_state::<Context>() {
        *lock(&context.last_phase) = Some(payload.clone());
    }
    // `eval` is the only channel. `app.emit` used to push an IPC message into
    // *every* webview — including the remote Pi Web document — and the loading
    // page no longer has a script that could listen for it.
    if let Some(window) = app.get_webview_window("main") {
        apply_phase(&window, &payload);
    }
    update_tray_state(app);
}

/// Push one phase into the loading page. `eval` is a Rust-side API, so the page
/// needs no IPC permission.
fn apply_phase(window: &WebviewWindow<Wry>, payload: &serde_json::Value) {
    let script = r#"(function(){if(!document.body)return;const s=__STATE__;document.body.dataset.phase=s.phase;const t=document.getElementById('status-text');if(t)t.textContent=s.status;const e=document.getElementById('endpoint');if(e)e.textContent=s.endpoint;const steps=document.querySelectorAll('[data-step]');for(let i=0;i<steps.length;i++){if(steps[i].dataset.step===s.phase)steps[i].setAttribute('aria-current','step');else steps[i].removeAttribute('aria-current');}})()"#
        .replace("__STATE__", &payload.to_string());
    eval_loading_page(window, &script);
}

/// Render a failure inside the loading page.
///
/// The native message box is the primary channel for startup failures, but it
/// can be refused (locked down desktops) and it disappears as soon as the
/// process exits. The page keeps the reason and the log path on screen instead
/// of looping an animation that never explains itself.
fn apply_error(window: &WebviewWindow<Wry>, report: &ErrorReport) {
    let payload = serde_json::json!({
        "message": report.message,
        "heading": report.heading,
        "hint": report.hint,
    });
    let script = r#"(function(){if(!document.body)return;const s=__STATE__;document.body.dataset.phase='failed';const h=document.getElementById('heading');if(h)h.textContent=s.heading;const p=document.querySelector('.subtitle');if(p)p.textContent=s.hint;const box=document.getElementById('error-box');if(box){box.textContent=s.message;box.hidden=false;}})()"#
        .replace("__STATE__", &payload.to_string());
    eval_loading_page(window, &script);
}

fn loading_url() -> Option<Url> {
    let (scheme, host) = internal_origin(cfg!(windows));
    Url::parse(&format!("{scheme}://{host}/index.html")).ok()
}

/// Bring the main window back to the bundled loading page.
///
/// Used when the backend is gone: showing the dead Pi Web document (or a
/// browser-style "connection refused" page) is worse than the launcher's own
/// explanation. `reload` forces a fresh document so stale error markup is not
/// left behind — a same-URL navigation alone is a reload in WebView2, but the
/// cache-busting query makes that explicit and keeps the path unchanged.
fn go_to_loading_page(app: &AppHandle, context: &Context, reload: bool) {
    let Some(base) = loading_url() else {
        return;
    };
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let url = if reload {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(0);
        Url::parse(&format!("{base}?reload={stamp}")).unwrap_or_else(|_| base.clone())
    } else {
        base
    };
    let already_there = window.url().map(|current| current == url).unwrap_or(false);
    if already_there {
        return;
    }
    if let Err(error) = window.navigate(url) {
        log_error(
            &context.state_dir,
            "Cannot return to the loading page",
            &error,
        );
    }
}

/// Check both before scheduling eval and inside the script. Navigation can
/// commit between the Rust URL lookup and WebView2 executing the queued script;
/// in that case the remote document must not be modified by a stale phase/error.
fn eval_loading_page(window: &WebviewWindow<Wry>, script: &str) {
    if !showing_loading_page(window) {
        return;
    }
    let (scheme, host) = internal_origin(cfg!(windows));
    let protocol = format!("{scheme}:");
    let guarded = format!(
        // Read the unforgeable Location object directly: a remote document
        // could replace the global URL constructor before a queued eval runs.
        "(function(){{const l=window.location;\
         if(l.protocol!=={protocol:?}||l.hostname!=={host:?}||l.port!==''||\
         (l.pathname!=='/'&&l.pathname!=='/index.html'))return;{script}}})()"
    );
    let _ = window.eval(guarded);
}

/// Whether the main window is currently showing the bundled loading page rather
/// than the remote Pi Web document.
fn showing_loading_page(window: &WebviewWindow<Wry>) -> bool {
    window
        .url()
        .map(|url| is_loading_url(&url))
        .unwrap_or(false)
}

/// One successfully started backend plus the handle that owns it.
struct Running {
    backend: PiBackend,
    port: u16,
    server: ServerInfo,
}

/// Owns the backend for the life of the process.
///
/// The loop exists so a failed start or an unexpected backend exit is
/// recoverable: the tray can ask for a restart instead of the user closing and
/// reopening the launcher (which used to be the only way back). A fresh
/// `PiBackend` is built for every attempt, so a restart also picks up an edited
/// `launcher-config.json` — including a changed backend path or port.
fn startup(context: Context, app: AppHandle, receiver: mpsc::Receiver<Command>) {
    let state_dir = context.state_dir.as_ref().clone();
    let mut running: Option<Running>;

    loop {
        context.starting.store(true, Ordering::SeqCst);
        let mut started = None;
        let mut failure = None;
        match PiBackend::new(state_dir.clone(), context.closed.clone()) {
            Ok(mut backend) => {
                log(&state_dir, "Config loaded, backend handle ready");
                let port = backend.port();
                emit_phase(&app, "environment", port);
                emit_phase(&app, "backend", port);
                if let Some(server) = launch(&context, &app, &mut backend, port) {
                    started = Some(Running {
                        backend,
                        port,
                        server,
                    });
                }
            }
            Err(error) => failure = Some(error),
        }
        context.starting.store(false, Ordering::SeqCst);
        if let Some(error) = failure {
            if !context.quitting.load(Ordering::SeqCst) {
                context.stopped.store(true, Ordering::SeqCst);
                report_start_failure(&context, &app, &state_dir, &error);
            }
        }
        running = started;
        update_tray_state(&app);

        match receiver.recv() {
            Ok(Command::Restart) if !context.quitting.load(Ordering::SeqCst) => {
                // Silence both watchers before stopping: the exit we are about
                // to cause must not be reported as a crash.
                context.generation.fetch_add(1, Ordering::SeqCst);
                if let Some(mut current) = running.take() {
                    stop_backend(
                        &mut current.backend,
                        current.port,
                        &current.server,
                        &state_dir,
                    );
                }
                *lock(&context.server) = None;
                *lock(&context.started_at) = None;
                *lock(&context.last_error) = None;
                *lock(&context.last_phase) = None;
                context.stopped.store(false, Ordering::SeqCst);
                context.healthy.store(false, Ordering::SeqCst);
                log(&state_dir, "Restart requested from the tray");
                go_to_loading_page(&app, &context, true);
            }
            _ => break,
        }
    }

    if let Some(mut current) = running {
        stop_backend(
            &mut current.backend,
            current.port,
            &current.server,
            &state_dir,
        );
    }
    context.window.flush();
    app.exit(0);
}

/// One start attempt: spawn/adopt the backend, show it and start its watchers.
fn launch(
    context: &Context,
    app: &AppHandle,
    backend: &mut PiBackend,
    port: u16,
) -> Option<ServerInfo> {
    let state_dir = context.state_dir.as_ref().clone();
    let started = backend.start(port);

    let server = match started {
        Ok(server) => server,
        Err(error) => {
            if context.quitting.load(Ordering::SeqCst) {
                log(&state_dir, "Start cancelled: the launcher is quitting");
                return None;
            }
            context.stopped.store(true, Ordering::SeqCst);
            report_start_failure(context, app, &state_dir, &error);
            return None;
        }
    };

    *lock(&context.server) = Some(server.clone());
    *lock(&context.started_at) = Some(SystemTime::now());
    *lock(&context.last_error) = None;
    context.stopped.store(false, Ordering::SeqCst);
    context.healthy.store(true, Ordering::SeqCst);
    let generation = context.generation.fetch_add(1, Ordering::SeqCst) + 1;
    emit_phase(app, "workspace", port);

    let url = match Url::parse(&server.url) {
        Ok(url) => url,
        Err(error) => {
            fatal(context, app, &state_dir, &error);
            return None;
        }
    };
    // The window is built on the main thread right after this thread starts, so
    // it may not exist yet when the backend wins the race.
    let window = match wait_for_main_window(app, Duration::from_secs(20)) {
        Some(window) => window,
        None => {
            fatal(context, app, &state_dir, &err("主窗口不存在"));
            return None;
        }
    };
    if let Err(error) = window.navigate(url) {
        fatal(context, app, &state_dir, &error);
        return None;
    }

    log(
        &state_dir,
        &format!(
            "{APP_NAME} ready at {}; version={}",
            server.url,
            server.version.as_deref().unwrap_or("external")
        ),
    );

    // Now that the main page is loading, build the tray popover on this
    // (worker) thread and seed it with the current state. Doing it here keeps a
    // second WebView2 controller off the startup critical path.
    ensure_tray_menu_window(app);
    update_tray_state(app);

    if backend.spawned() {
        spawn_child_watcher(context, app, backend.child_handle(), generation);
    }
    spawn_health_watcher(context, app, server.url.clone(), generation);
    Some(server)
}

/// The backend keeps owning the `Child`; this thread only polls it. Handing
/// ownership over (the old `take_child`) left nothing to kill if the record
/// file disappeared, which leaked the backend *and* the port.
fn spawn_child_watcher(
    context: &Context,
    app: &AppHandle,
    child: Arc<Mutex<Option<std::process::Child>>>,
    generation: u64,
) {
    let worker = context.clone();
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("backend-watch".into())
        .spawn(move || loop {
            let status = {
                let mut guard = lock(&child);
                match guard.as_mut() {
                    Some(child) => child.try_wait().ok().flatten(),
                    None => return,
                }
            };
            if let Some(status) = status {
                // A restart or quit bumps the generation before it stops the
                // backend, so only a genuinely unexpected exit gets here.
                if worker.generation.load(Ordering::SeqCst) == generation
                    && !worker.quitting.load(Ordering::SeqCst)
                {
                    backend_exited(&worker, &app, status.code().unwrap_or(-1));
                }
                return;
            }
            if worker.closed.load(Ordering::SeqCst)
                || worker.generation.load(Ordering::SeqCst) != generation
            {
                return;
            }
            std::thread::sleep(WATCH_INTERVAL);
        });
    if spawned.is_err() {
        log(
            &context.state_dir,
            "Cannot start the backend watcher thread",
        );
    }
}

/// Liveness probe for "process alive, service not answering".
fn spawn_health_watcher(context: &Context, app: &AppHandle, url: String, generation: u64) {
    let worker = context.clone();
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("backend-health".into())
        .spawn(move || {
            let mut strikes = 0;
            loop {
                std::thread::sleep(HEALTH_INTERVAL);
                if worker.closed.load(Ordering::SeqCst)
                    || worker.generation.load(Ordering::SeqCst) != generation
                {
                    return;
                }
                if !http::check_server(&url, &worker.closed) {
                    strikes += 1;
                } else {
                    strikes = 0;
                }
                let healthy = strikes < HEALTH_STRIKES;
                if worker.healthy.swap(healthy, Ordering::SeqCst) != healthy {
                    log(
                        &worker.state_dir,
                        if healthy {
                            "Backend answers again"
                        } else {
                            "Backend is running but stopped answering"
                        },
                    );
                    update_tray_state(&app);
                }
            }
        });
    if spawned.is_err() {
        log(&context.state_dir, "Cannot start the backend health thread");
    }
}

/// The backend exited on its own. Keep the launcher alive: the user may want to
/// read the log, open the config directory, or simply restart.
fn backend_exited(context: &Context, app: &AppHandle, code: i32) {
    let state_dir = context.state_dir.as_ref().clone();
    log(
        &state_dir,
        &format!("Pi Web exited unexpectedly: code={code}"),
    );
    // Stop the health probe as well: it would keep probing a dead port.
    context.generation.fetch_add(1, Ordering::SeqCst);
    *lock(&context.server) = None;
    *lock(&context.started_at) = None;
    context.healthy.store(false, Ordering::SeqCst);
    context.stopped.store(true, Ordering::SeqCst);

    let report = ErrorReport {
        heading: "本地服务已停止",
        hint: "可以从托盘菜单重新启动它。",
        message: format!(
            "Pi Web 意外退出（code={code}）。\n\n\
             右键托盘图标，选择“重启本地服务”即可重新启动；也可以退出后查看日志。\n\
             日志：{}",
            state_dir.join("launcher.log").display()
        ),
    };
    // Record it first: a page that loads after this point replays it instead of
    // a stale "正在连接本地服务".
    *lock(&context.last_error) = Some(report.clone());
    if let Some(window) = app.get_webview_window("main") {
        if showing_loading_page(&window) {
            apply_error(&window, &report);
        } else {
            go_to_loading_page(app, context, false);
        }
    }
    update_tray_state(app);
}

/// A start attempt failed. The launcher stays alive so the tray can retry after
/// the configuration is fixed; only unrecoverable failures call `fatal`.
fn report_start_failure(context: &Context, app: &AppHandle, state_dir: &Path, error: &dyn Display) {
    let report = ErrorReport {
        heading: "无法启动本地服务",
        hint: "按下方原因检查配置，然后从托盘菜单重试。",
        message: format!(
            "{}\n\n\
             配置：{}\n\
             日志：{}\n\n\
             修正后可从托盘菜单“重启本地服务”重试，不必重新打开启动器。",
            error,
            state_dir.join("launcher-config.json").display(),
            state_dir.join("launcher.log").display()
        ),
    };
    log_error(state_dir, "Backend start failed", error);
    *lock(&context.last_error) = Some(report.clone());
    *lock(&context.server) = None;
    *lock(&context.started_at) = None;
    context.healthy.store(false, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window("main") {
        if showing_loading_page(&window) {
            apply_error(&window, &report);
        } else {
            go_to_loading_page(app, context, false);
        }
    }
    update_tray_state(app);

    // A login launch (`--tray`) has no visible window, so the dialog is the
    // only way the user learns that nothing started.
    log(state_dir, "Showing startup error dialog");
    if !process::show_message_box(APP_NAME, &report.message) {
        log(
            state_dir,
            "Windows did not display the error dialog; the window and tray still show it",
        );
    }
}

fn stop_backend(backend: &mut PiBackend, port: u16, server: &ServerInfo, state_dir: &Path) {
    // An adopted external Pi Web is not ours to stop.
    if !server.owned {
        return;
    }
    if let Err(error) = backend.stop(port, server.info.as_ref()) {
        log_error(state_dir, "Failed to stop backend", &error);
    }
}

fn request_restart(context: &Context) {
    if context.quitting.load(Ordering::SeqCst) || context.starting.swap(true, Ordering::SeqCst) {
        return;
    }
    let sender = lock(&context.shutdown).clone();
    match sender {
        Some(sender) => {
            if sender.send(Command::Restart).is_err() {
                context.starting.store(false, Ordering::SeqCst);
                log(
                    &context.state_dir,
                    "Restart request dropped: the startup thread is gone",
                );
            }
        }
        None => {
            context.starting.store(false, Ordering::SeqCst);
            log(
                &context.state_dir,
                "Restart request dropped: nothing to restart",
            );
        }
    }
}

fn request_quit(context: &Context, app: &AppHandle) {
    // Guard on a dedicated flag: `closed` is also set by `fatal`, and a failed
    // start must not make the tray's "退出" a no-op.
    if context.quitting.swap(true, Ordering::SeqCst) {
        return;
    }
    context.closed.store(true, Ordering::SeqCst);
    // Synchronous: the process ends as soon as the startup thread answers.
    context.window.flush();
    let sender = lock(&context.shutdown).take();
    match sender {
        Some(sender) => {
            // The startup thread is gone once it has failed; then there is
            // nothing to wait for and the send fails immediately.
            if sender.send(Command::Quit).is_err() {
                app.exit(0);
            }
        }
        None => app.exit(0),
    }
}

fn fatal(context: &Context, app: &AppHandle, state_dir: &Path, error: &dyn Display) {
    let report = ErrorReport {
        heading: "启动失败",
        hint: "按下方原因检查配置或环境。",
        message: format!(
            "{}\n\n日志：{}",
            error,
            state_dir.join("launcher.log").display()
        ),
    };
    log_error(state_dir, "Fatal error", error);
    context.closed.store(true, Ordering::SeqCst);

    // Record the failure first: a page that loads after this point replays it
    // instead of a stale "正在连接本地服务".
    if let Some(state) = app.try_state::<Context>() {
        *lock(&state.last_error) = Some(report.clone());
        // The backend is gone; a stale `server` made the tray keep showing a
        // green "running" dot and offer "copy address"/"open in browser" for a
        // port nothing listens on any more.
        *lock(&state.server) = None;
    }
    update_tray_state(app);
    // Show it in the window as well, but only while the loading page is up —
    // writing into the remote Pi Web document would be an intrusion.
    if let Some(window) = app.get_webview_window("main") {
        if showing_loading_page(&window) {
            apply_error(&window, &report);
            let _ = window.show();
            let _ = window.set_focus();
        }
    }

    log(state_dir, "Showing startup error dialog");
    if process::show_message_box(APP_NAME, &report.message) {
        // The user has read the dialog; there is nothing left to keep alive.
        // `app.exit` only queues the shutdown, so returning matters: without it
        // the "did not display" branch below would log a contradiction.
        log(state_dir, "Error acknowledged; exiting");
        app.exit(1);
        return;
    }
    // If Windows refused the dialog, stay alive: the loading page still shows
    // the reason, and the tray can quit. Exiting here would leave a machine
    // that cannot display message boxes with no way to learn what happened.
    log(
        state_dir,
        "Windows did not display the error dialog; keeping the window open",
    );
}

#[cfg(test)]
mod navigation_tests {
    use super::*;

    fn url(value: &str) -> Url {
        Url::parse(value).unwrap()
    }

    #[test]
    fn only_an_exact_tray_switch_hides_the_window() {
        let parse = |args: &[&str]| Options::from_args(args.iter().map(|arg| (*arg).to_string()));
        assert!(parse(&[START_HIDDEN_ARG]).hidden);
        assert!(parse(&["/tray"]).hidden);
        assert!(parse(&["--tray", "--other"]).hidden);
        assert!(!parse(&[]).hidden);
        // A stale or mistyped switch must not turn a double-click into nothing.
        for other in [
            "--trays", "-tray", "--tray=1", "tray", "--TRAY", "", " --tray",
        ] {
            assert!(!parse(&[other]).hidden, "{other:?}");
        }
    }

    #[test]
    fn bundled_origin_matches_the_platform_and_default_port_only() {
        for value in [
            "http://tauri.localhost/",
            "http://tauri.localhost/index.html",
            "http://tauri.localhost:80/tray.html",
            "HTTP://TAURI.LOCALHOST/index.html?view=1#status",
        ] {
            assert!(is_internal_url_for_platform(&url(value), true), "{value}");
            assert!(!is_internal_url_for_platform(&url(value), false), "{value}");
        }
        for value in ["tauri://localhost/", "tauri://localhost/tray.html"] {
            assert!(is_internal_url_for_platform(&url(value), false), "{value}");
            assert!(!is_internal_url_for_platform(&url(value), true), "{value}");
        }
    }

    #[test]
    fn malicious_bundled_origin_variants_are_untrusted() {
        for value in [
            "https://tauri.localhost/index.html",
            "https://tauri.localhost:443/index.html",
            "http://tauri.localhost:443/index.html",
            "http://tauri.localhost:30141/index.html",
            "http://tauri.localhost.evil.test/index.html",
            "http://tauri.localhost./index.html",
            "http://evil.test/tauri.localhost/index.html",
            "http://tauri.localhost@evil.test/index.html",
            "http://evil.test@tauri.localhost/index.html",
            "http://user:secret@tauri.localhost/index.html",
            "http://:secret@tauri.localhost/index.html",
            "ftp://tauri.localhost/index.html",
            "asset://tauri.localhost/index.html",
            "tauri://evil.test/index.html",
            "tauri://tauri.localhost/index.html",
            "tauri://localhost.evil.test/index.html",
            "tauri://localhost:80/index.html",
            "tauri://localhost:30141/index.html",
            "tauri://user:secret@localhost/index.html",
            "tauri:///index.html",
            "tauri:index.html",
            "file:///index.html",
            "data:text/html,tauri://localhost",
            "javascript:location='http://tauri.localhost'",
        ] {
            let parsed = url(value);
            assert!(
                !is_internal_url_for_platform(&parsed, true),
                "Windows: {value}"
            );
            assert!(
                !is_internal_url_for_platform(&parsed, false),
                "Other: {value}"
            );
        }
    }

    #[test]
    fn bundled_documents_cannot_navigate_into_each_other() {
        let (scheme, host) = internal_origin(cfg!(windows));
        let base = format!("{scheme}://{host}");
        for path in ["/", "/index.html", "/index.html#status"] {
            let parsed = url(&format!("{base}{path}"));
            assert!(is_loading_url(&parsed), "{path}");
            assert!(!is_tray_url(&parsed), "{path}");
        }
        assert!(is_tray_url(&url(&format!("{base}/tray.html"))));
        assert!(!is_loading_url(&url(&format!("{base}/tray.html"))));
        for path in [
            "/other.html",
            "/tray.html/",
            "/%74ray.html",
            "/index.html/tray.html",
        ] {
            let parsed = url(&format!("{base}{path}"));
            assert!(!is_tray_url(&parsed), "{path}");
            assert!(!is_loading_url(&parsed), "{path}");
        }
    }

    #[test]
    fn server_navigation_requires_the_exact_http_origin() {
        let base = "http://127.0.0.1:30141/";
        for value in [base, "http://127.0.0.1:30141/session/123?tab=chat#bottom"] {
            assert!(is_server_url(&url(value), base), "{value}");
        }
        for value in [
            "http://127.0.0.1/",
            "http://127.0.0.1:30142/",
            "https://127.0.0.1:30141/",
            "http://localhost:30141/",
            "http://127.0.0.1.evil.test:30141/",
            "http://127.0.0.1:30141@evil.test/",
            "http://user:secret@127.0.0.1:30141/",
            "ftp://127.0.0.1:30141/",
            "tauri://localhost/",
        ] {
            assert!(!is_server_url(&url(value), base), "{value}");
        }
        assert!(!is_server_url(&url(base), "not a URL"));
        assert!(!is_server_url(&url(base), "ftp://127.0.0.1:30141/"));
    }

    #[test]
    fn action_signals_require_exact_authority_and_single_path() {
        assert_eq!(
            action_from_url(&url("https://pi-launcher.tray/show"), TRAY_ACTION_HOST),
            Some("show")
        );
        assert_eq!(
            action_from_url(&url("https://pi-launcher.zoom/in"), "pi-launcher.zoom"),
            Some("in")
        );
        for value in [
            "http://pi-launcher.tray/show",
            "tauri://pi-launcher.tray/show",
            "https://pi-launcher.tray:444/show",
            "https://pi-launcher.tray.evil.test/show",
            "https://pi-launcher.tray./show",
            "https://pi-launcher.tray@evil.test/show",
            "https://user:secret@pi-launcher.tray/show",
            "https://pi-launcher.tray/show?quit",
            "https://pi-launcher.tray/show#quit",
            "https://pi-launcher.tray//show",
            "https://pi-launcher.tray/show/quit",
            "https://pi-launcher.tray/show/",
            "https://pi-launcher.tray/",
        ] {
            assert_eq!(
                action_from_url(&url(value), TRAY_ACTION_HOST),
                None,
                "{value}"
            );
        }
    }

    #[test]
    fn external_opener_rejects_non_web_schemes_and_credentials() {
        for value in ["https://example.test/path?q=1", "http://localhost:30141/"] {
            assert!(is_external_url(&url(value)), "{value}");
        }
        for value in [
            "file:///C:/Windows/notepad.exe",
            "javascript:alert(1)",
            "data:text/html,test",
            "mailto:person@example.test",
            "tauri://localhost/",
            "https://user:secret@example.test/",
        ] {
            assert!(!is_external_url(&url(value)), "{value}");
        }
    }
}
