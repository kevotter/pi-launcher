//! Remembered main-window geometry and maximized state.
//!
//! Stored as `window.json` next to `launcher-config.json`. This is UI state
//! only. A corrupt or nonsensical file is ignored and replaced on the next
//! save, never repaired in place: a parseable but absurd geometry would put
//! the window off-screen with no obvious way to get it back.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::util::lock;

const FILE_NAME: &str = "window.json";
const SCHEMA: u32 = 1;
/// Dragging or resizing emits one event per frame; writing each one would be
/// hundreds of AppData writes per second.
const WRITE_DELAY: Duration = Duration::from_millis(700);
const DEFAULT_SIZE: (u32, u32) = (1280, 860);
const MIN_SIZE: (u32, u32) = (320, 240);
const MAX_SIZE: (u32, u32) = (8000, 8000);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Saved {
    pub x: i32,
    pub y: i32,
    /// Logical pixels: the same perceived size on a 100% and a 150% monitor.
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

impl Saved {
    fn with_size(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
            maximized: false,
        }
    }
}

/// Physical work area of one monitor, already excluding the taskbar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Monitor {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

impl Monitor {
    fn right(self) -> i64 {
        i64::from(self.left) + i64::from(self.width)
    }

    fn bottom(self) -> i64 {
        i64::from(self.top) + i64::from(self.height)
    }

    fn contains(self, x: i32, y: i32) -> bool {
        i64::from(x) >= i64::from(self.left)
            && i64::from(x) < self.right()
            && i64::from(y) >= i64::from(self.top)
            && i64::from(y) < self.bottom()
    }

    fn distance(self, x: i32, y: i32) -> i64 {
        let dx = (i64::from(self.left) - i64::from(x))
            .max(i64::from(x) - (self.right() - 1))
            .max(0);
        let dy = (i64::from(self.top) - i64::from(y))
            .max(i64::from(y) - (self.bottom() - 1))
            .max(0);
        dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
    }
}

/// What the window should be created with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    /// Physical position, or `None` on a first run without any saved state
    /// (the builder then centres the window once).
    pub position: Option<(i32, i32)>,
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
}

pub(crate) fn default_placement() -> Placement {
    Placement {
        position: None,
        width: f64::from(DEFAULT_SIZE.0),
        height: f64::from(DEFAULT_SIZE.1),
        maximized: false,
    }
}

/// Nearest monitor, preferring one that actually contains the saved top-left.
fn pick(x: i32, y: i32, monitors: &[Monitor]) -> Option<Monitor> {
    monitors
        .iter()
        .copied()
        .find(|monitor| monitor.contains(x, y))
        .or_else(|| {
            monitors
                .iter()
                .copied()
                .min_by_key(|monitor| monitor.distance(x, y))
        })
}

/// Turn saved state into a placement that is guaranteed to be visible.
///
/// A monitor can be unplugged or rearranged between sessions. Clamping into
/// the work area (rather than only requiring a sliver on screen) is what makes
/// the window usable again without the user hunting for it.
pub(crate) fn place(saved: Option<Saved>, monitors: &[Monitor]) -> Placement {
    let Some(saved) = saved else {
        return default_placement();
    };
    let mut placement = Placement {
        position: None,
        width: f64::from(saved.width.clamp(MIN_SIZE.0, MAX_SIZE.0)),
        height: f64::from(saved.height.clamp(MIN_SIZE.1, MAX_SIZE.1)),
        maximized: saved.maximized,
    };
    let Some(monitor) = pick(saved.x, saved.y, monitors) else {
        return placement;
    };
    if !monitor.scale.is_finite() || monitor.scale <= 0.0 {
        return placement;
    }
    // Never ask for more window than the target monitor can show.
    let available_width = f64::from(monitor.width) / monitor.scale;
    let available_height = f64::from(monitor.height) / monitor.scale;
    placement.width = placement.width.min(available_width).max(1.0);
    placement.height = placement.height.min(available_height).max(1.0);

    let width = (placement.width * monitor.scale).round() as i64;
    let height = (placement.height * monitor.scale).round() as i64;
    let x = if width >= i64::from(monitor.width) {
        i64::from(monitor.left)
    } else {
        i64::from(saved.x).clamp(i64::from(monitor.left), monitor.right() - width)
    };
    let y = if height >= i64::from(monitor.height) {
        i64::from(monitor.top)
    } else {
        i64::from(saved.y).clamp(i64::from(monitor.top), monitor.bottom() - height)
    };
    placement.position = Some((x as i32, y as i32));
    placement
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(default)]
    schema: u32,
    x: Option<i32>,
    y: Option<i32>,
    width: Option<u32>,
    height: Option<u32>,
    #[serde(default)]
    maximized: bool,
}

fn parse(text: &str) -> Option<Saved> {
    let record: Record = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    if record.schema != SCHEMA {
        return None;
    }
    let width = record.width?;
    let height = record.height?;
    if !(MIN_SIZE.0..=MAX_SIZE.0).contains(&width) || !(MIN_SIZE.1..=MAX_SIZE.1).contains(&height) {
        return None;
    }
    Some(Saved {
        x: record.x?,
        y: record.y?,
        width,
        height,
        maximized: record.maximized,
    })
}

fn read(path: &Path) -> Option<Saved> {
    parse(&fs::read_to_string(path).ok()?)
}

struct Inner {
    saved: Option<Saved>,
    dirty: bool,
    last_write: Instant,
    /// Bumped by every change. A write only clears `dirty` when it captured the
    /// latest revision, so a change made while the file was being written is
    /// still flushed instead of being swallowed.
    revision: u64,
}

/// Debounced writer for the remembered state.
#[derive(Clone)]
pub(crate) struct Store {
    path: PathBuf,
    inner: Arc<Mutex<Inner>>,
    /// Held across snapshot-and-write, so the debounce tick and the quit flush
    /// cannot interleave on the same temporary file, and a flush waits for a
    /// write already in flight instead of returning as if the state were saved.
    io: Arc<Mutex<()>>,
}

impl Store {
    /// Returns the store plus the state to restore, if any.
    pub(crate) fn open(state_dir: &Path) -> (Self, Option<Saved>) {
        let path = state_dir.join(FILE_NAME);
        let saved = read(&path);
        (
            Self {
                path,
                inner: Arc::new(Mutex::new(Inner {
                    saved,
                    dirty: false,
                    last_write: Instant::now(),
                    revision: 0,
                })),
                io: Arc::new(Mutex::new(())),
            },
            saved,
        )
    }

    fn update(&self, change: impl FnOnce(&mut Saved)) {
        let mut inner = lock(&self.inner);
        let mut saved = inner
            .saved
            .unwrap_or_else(|| Saved::with_size(DEFAULT_SIZE.0, DEFAULT_SIZE.1));
        change(&mut saved);
        inner.saved = Some(saved);
        inner.dirty = true;
        inner.revision = inner.revision.wrapping_add(1);
    }

    pub(crate) fn record_geometry(&self, x: i32, y: i32, width: u32, height: u32) {
        let width = width.clamp(MIN_SIZE.0, MAX_SIZE.0);
        let height = height.clamp(MIN_SIZE.1, MAX_SIZE.1);
        self.update(|saved| {
            saved.x = x;
            saved.y = y;
            saved.width = width;
            saved.height = height;
        });
    }

    pub(crate) fn record_maximized(&self, maximized: bool) {
        self.update(|saved| saved.maximized = maximized);
    }

    /// One detached thread per process; it ends with the process.
    pub(crate) fn spawn_writer(&self) {
        let store = self.clone();
        let _ = std::thread::Builder::new()
            .name("window-state".into())
            .spawn(move || loop {
                std::thread::sleep(WRITE_DELAY);
                store.write_pending(false);
            });
    }

    /// Write immediately (quit path), so a size change in the last second of
    /// the session is not lost.
    pub(crate) fn flush(&self) {
        self.write_pending(true);
    }

    fn write_pending(&self, force: bool) {
        let _io = lock(&self.io);
        let (saved, revision) = {
            let mut inner = lock(&self.inner);
            if !inner.dirty || (!force && inner.last_write.elapsed() < WRITE_DELAY) {
                return;
            }
            let Some(saved) = inner.saved else {
                return;
            };
            inner.last_write = Instant::now();
            (saved, inner.revision)
        };
        // `dirty` is cleared only after the bytes are on disk. Clearing it up
        // front let the quit path see a clean store while this write was still
        // in flight, and the process then exited before it landed.
        if write(&self.path, saved).is_ok() {
            let mut inner = lock(&self.inner);
            if inner.revision == revision {
                inner.dirty = false;
            }
        }
    }
}

fn write(path: &Path, saved: Saved) -> std::io::Result<()> {
    let body = serde_json::json!({
        "schema": SCHEMA,
        "x": saved.x,
        "y": saved.y,
        "width": saved.width,
        "height": saved.height,
        "maximized": saved.maximized,
    });
    let text = serde_json::to_vec_pretty(&body)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &text)?;
    // Rust's rename replaces an existing file on Windows (MoveFileEx).
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(x: i32, y: i32, width: u32, height: u32) -> Saved {
        Saved {
            x,
            y,
            width,
            height,
            maximized: false,
        }
    }

    fn monitor(left: i32, top: i32, width: u32, height: u32, scale: f64) -> Monitor {
        Monitor {
            left,
            top,
            width,
            height,
            scale,
        }
    }

    /// 1920x1080 at 100% with a 40px taskbar, plus a 2560x1440 at 150% to the right.
    fn desktops() -> Vec<Monitor> {
        vec![
            monitor(0, 0, 1920, 1040, 1.0),
            monitor(1920, 0, 2560, 1400, 1.5),
        ]
    }

    #[test]
    fn first_run_has_no_position_and_the_default_size() {
        let placement = place(None, &desktops());
        assert_eq!(placement, default_placement());
        assert!(placement.position.is_none());
        // Even without any monitor information the remembered size is used.
        assert_eq!(place(Some(saved(0, 0, 1024, 640)), &[]).width, 1024.0);
    }

    #[test]
    fn a_position_inside_the_work_area_is_kept_exactly() {
        let placement = place(Some(saved(120, 80, 1200, 800)), &desktops());
        assert_eq!(placement.position, Some((120, 80)));
        assert_eq!((placement.width, placement.height), (1200.0, 800.0));
    }

    #[test]
    fn an_unplugged_monitor_pulls_the_window_into_the_nearest_one() {
        // Saved on a third monitor that no longer exists.
        let placement = place(Some(saved(6000, 200, 1200, 800)), &desktops());
        let (x, y) = placement.position.expect("clamped position");
        assert_eq!(position_monitor(x, y, &desktops()).left, 1920);
        assert_eq!(y, 200);
        assert!(x + 1200 <= 1920 + 2560, "window must fit horizontally");
    }

    fn position_monitor(x: i32, y: i32, monitors: &[Monitor]) -> Monitor {
        pick(x, y, monitors).expect("a monitor")
    }

    #[test]
    fn odd_sizes_and_negative_coordinates_are_clamped_in() {
        let placement = place(Some(saved(-4000, -4000, 900, 600)), &desktops());
        assert_eq!(placement.position, Some((0, 0)));
    }

    #[test]
    fn a_window_larger_than_the_screen_is_shrunk_and_placed_at_the_origin() {
        let placement = place(Some(saved(300, 300, 9000, 9000)), &desktops());
        assert_eq!(placement.position, Some((0, 0)));
        assert_eq!((placement.width, placement.height), (1920.0, 1040.0));
    }

    #[test]
    fn restoring_on_a_scaled_monitor_keeps_the_logical_size() {
        // 1200 logical px on the 150% monitor occupies 1800 physical px.
        let placement = place(Some(saved(2000, 100, 1200, 800)), &desktops());
        assert_eq!(placement.width, 1200.0);
        assert_eq!(placement.height, 800.0);
        assert_eq!(placement.position, Some((2000, 100)));
        let (x, _) = placement.position.expect("position");
        let physical_width = (placement.width * 1.5).round() as i32;
        assert!(x + physical_width <= 1920 + 2560);
    }

    #[test]
    fn a_maximized_window_is_restored_as_maximized() {
        let mut state = saved(10, 10, 1200, 800);
        state.maximized = true;
        assert!(place(Some(state), &desktops()).maximized);
    }

    #[test]
    fn parsing_rejects_foreign_or_absurd_files() {
        assert_eq!(
            parse(r#"{"schema":1,"x":10,"y":20,"width":1200,"height":800}"#),
            Some(saved(10, 20, 1200, 800))
        );
        // A BOM is what Notepad writes; it must not invalidate the file.
        assert!(
            parse("\u{feff}{\"schema\":1,\"x\":0,\"y\":0,\"width\":1200,\"height\":800}").is_some()
        );
        for text in [
            "",
            "{}",
            "[]",
            r#"{"schema":2,"x":10,"y":20,"width":1200,"height":800}"#,
            r#"{"x":10,"y":20,"width":1200,"height":800}"#,
            r#"{"schema":1,"y":20,"width":1200,"height":800}"#,
            r#"{"schema":1,"x":10,"y":20,"width":10,"height":800}"#,
            r#"{"schema":1,"x":10,"y":20,"width":1200,"height":99999}"#,
            r#"{"schema":1,"x":10,"y":20,"width":"1200","height":800}"#,
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    #[test]
    fn store_round_trips_and_ignores_a_corrupt_file() {
        let dir = std::env::temp_dir().join(format!("pi-launcher-window-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        fs::write(dir.join(FILE_NAME), b"not json").expect("write junk");

        let (store, restored) = Store::open(&dir);
        assert_eq!(restored, None);
        store.record_geometry(64, 64, 1000, 700);
        store.record_maximized(true);
        store.flush();

        let (_, restored) = Store::open(&dir);
        assert_eq!(
            restored,
            Some(Saved {
                maximized: true,
                ..saved(64, 64, 1000, 700)
            })
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn geometry_is_clamped_before_it_is_ever_written() {
        let dir =
            std::env::temp_dir().join(format!("pi-launcher-window-clamp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        let (store, _) = Store::open(&dir);
        store.record_geometry(0, 0, 10, 10);
        store.flush();
        let (_, restored) = Store::open(&dir);
        let restored = restored.expect("saved state");
        assert_eq!((restored.width, restored.height), MIN_SIZE);
        let _ = fs::remove_dir_all(&dir);
    }
}
