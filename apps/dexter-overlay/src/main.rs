// `cocoa`/`objc` are deprecated in favor of `objc2`; migrating the overlay
// shell is tracked separately from the driver migration.
#![allow(deprecated)]
// objc's `class!`/`msg_send!` macros probe a `cargo-clippy` cfg.
#![allow(unexpected_cfgs)]

//! `dexter-overlay` — draw the agent's presence on screen.
//!
//! Usage: `dexter-overlay --events <journal.jsonl> [--agent NAME] [--replay]`
//!
//! The overlay tails a Dexter event journal and renders a labeled cursor
//! over the UI element the agent is acting on. The window is borderless,
//! transparent, floats above applications and is click-through
//! (`ignoresMouseEvents`) — it shows where the agent works without ever
//! touching the user's input.

use dexter_overlay::{reduce, JournalTail, PresenceState, PresenceStatus, UserControl};
use std::path::Path;
use std::sync::RwLock;
use std::time::{Duration, Instant};

static STATE: RwLock<Option<PresenceState>> = RwLock::new(None);
/// Lerped cursor position — the tag and arrow glide between targets.
static DISPLAY: RwLock<(f64, f64)> = RwLock::new((0.0, 0.0));

fn main() {
    let mut events: Option<String> = None;
    let mut agent = "dexter".to_string();
    let mut replay = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--events" => events = args.next(),
            "--agent" => agent = args.next().unwrap_or_else(|| "dexter".into()),
            "--replay" => replay = true,
            other => {
                eprintln!("unknown flag '{other}' — usage: dexter-overlay --events <journal.jsonl> [--agent NAME] [--replay]");
                std::process::exit(2);
            }
        }
    }
    let Some(events) = events else {
        eprintln!("usage: dexter-overlay --events <journal.jsonl> [--agent NAME] [--replay]");
        std::process::exit(2);
    };

    *STATE.write().unwrap() = Some(PresenceState::new(&agent));
    if cfg!(target_os = "macos") {
        platform::run(Path::new(&events), replay);
    } else {
        eprintln!("dexter-overlay renders on macOS only (journal contract is platform-agnostic)");
        std::process::exit(1);
    }
}

/// How a status maps to the tag/arrow color.
fn status_color(status: PresenceStatus, control: UserControl) -> (f64, f64, f64) {
    if control == UserControl::Exclusive {
        return (1.0, 0.35, 0.40); // physical input — unmistakable
    }
    match status {
        PresenceStatus::Observing
        | PresenceStatus::Thinking
        | PresenceStatus::Acting
        | PresenceStatus::Verifying => (0.30, 0.89, 1.0), // cyan
        PresenceStatus::WaitingApproval | PresenceStatus::Retrying => (1.0, 0.75, 0.30),
        PresenceStatus::Verified | PresenceStatus::Completed => (0.24, 1.0, 0.63),
        PresenceStatus::Abstained | PresenceStatus::Idle => (0.55, 0.55, 0.58),
        PresenceStatus::Denied | PresenceStatus::Failed => (1.0, 0.35, 0.40),
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use cocoa::appkit::{
        NSApp, NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSWindow,
        NSWindowStyleMask,
    };
    use cocoa::base::{id, nil, NO, YES};
    use cocoa::foundation::{NSAutoreleasePool, NSDate, NSPoint, NSRect, NSSize, NSString};
    use objc::declare::ClassDecl;
    use objc::runtime::{Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};

    extern "C" fn draw_rect(_this: &Object, _cmd: Sel, _rect: NSRect) {
        unsafe { draw_overlay() }
    }

    /// Draw arrow + reticle from current DISPLAY/STATE. Called on drawRect.
    unsafe fn draw_overlay() {
        let Some(s) = STATE.read().unwrap().clone() else {
            return;
        };
        if !s.visible {
            return;
        }
        // Cursor anchor: the target's center when the journal carries
        // real bounds; a degenerate rect (menubar items report 0×0) or
        // no bounds at all (failed observe, navigate) anchors lower-
        // center — presence must never be invisible in a corner.
        let d = *DISPLAY.read().unwrap();
        let (r, g, b) = status_color(s.status, s.user_control);
        let color: id = msg_send![class!(NSColor), colorWithSRGBRed:r green:g blue:b alpha:1.0f64];

        // Lock-on reticle around the target rect (screen → view coords).
        if let Some(t) = s.target {
            let h = screen_height();
            let rect = NSRect::new(
                NSPoint::new(t.x - 4.0, h - (t.y + t.h) - 4.0),
                NSSize::new(t.w + 8.0, t.h + 8.0),
            );
            let path: id = msg_send![class!(NSBezierPath), bezierPathWithRoundedRect:rect xRadius:6.0f64 yRadius:6.0f64];
            let _: () = msg_send![path, setLineWidth: 1.5f64];
            let _: () = msg_send![color, setStroke];
            let _: () = msg_send![path, stroke];
        }

        // Cursor arrow — the SVG arrow polygon, y-flipped for Cocoa,
        // scaled up so it reads at a glance on a full desktop.
        let (px, py) = (d.0, screen_height() - d.1);
        let path: id = msg_send![class!(NSBezierPath), bezierPath];
        let s = 1.7f64;
        let pts = [
            (0.0, 0.0),
            (14.0, -11.2),
            (7.6, -12.4),
            (11.0, -18.8),
            (8.2, -20.2),
            (4.8, -13.8),
            (0.0, -18.0),
        ];
        let _: () =
            msg_send![path, moveToPoint: NSPoint::new(px + pts[0].0 * s, py + pts[0].1 * s)];
        for (x, y) in &pts[1..] {
            let _: () = msg_send![path, lineToPoint: NSPoint::new(px + x * s, py + y * s)];
        }
        let _: () = msg_send![path, closePath];
        let _: () = msg_send![color, setFill];
        let _: () = msg_send![path, fill];
        // Thin dark edge so the arrow reads on light backgrounds too.
        let edge: id =
            msg_send![class!(NSColor), colorWithSRGBRed:0.02 green:0.05 blue:0.08 alpha:0.9f64];
        let _: () = msg_send![path, setLineWidth: 1.4f64];
        let _: () = msg_send![edge, setStroke];
        let _: () = msg_send![path, stroke];
    }

    fn screen_height() -> f64 {
        unsafe {
            let screen: id = msg_send![class!(NSScreen), mainScreen];
            let frame: NSRect = msg_send![screen, frame];
            frame.size.height
        }
    }

    fn screen_width() -> f64 {
        unsafe {
            let screen: id = msg_send![class!(NSScreen), mainScreen];
            let frame: NSRect = msg_send![screen, frame];
            frame.size.width
        }
    }

    fn ns(s: &str) -> id {
        unsafe { NSString::alloc(nil).init_str(s) }
    }

    pub fn run(events_path: &Path, replay: bool) {
        unsafe {
            let _pool = NSAutoreleasePool::new(nil);
            let app = NSApp();
            app.setActivationPolicy_(
                NSApplicationActivationPolicy::NSApplicationActivationPolicyAccessory,
            );

            let screen: id = msg_send![class!(NSScreen), mainScreen];
            let frame: NSRect = msg_send![screen, frame];

            let win = NSWindow::alloc(nil).initWithContentRect_styleMask_backing_defer_(
                frame,
                NSWindowStyleMask::NSBorderlessWindowMask,
                NSBackingStoreType::NSBackingStoreBuffered,
                NO,
            );
            win.setOpaque_(NO);
            win.setBackgroundColor_(NSColor::clearColor(nil));
            // Above status-bar level; never intercepts clicks.
            let _: () = msg_send![win, setLevel: 25i64];
            win.setIgnoresMouseEvents_(YES);
            // all spaces + stationary + over fullscreen apps' spaces
            let _: () = msg_send![win, setCollectionBehavior: 1u64 | 16u64 | 256u64];

            // Custom view: draws arrow + reticle.
            let superclass = class!(NSView);
            let mut decl = ClassDecl::new("DexterOverlayView", superclass).unwrap();
            decl.add_method(
                sel!(drawRect:),
                draw_rect as extern "C" fn(&Object, Sel, NSRect),
            );
            decl.register();
            let view: id = msg_send![class!(DexterOverlayView), alloc];
            let view: id = msg_send![view, initWithFrame: frame];
            win.setContentView_(view);

            // Tag: an NSTextField — name + status, styled via its layer.
            let tag: id = msg_send![class!(NSTextField), alloc];
            let tag: id = msg_send![tag, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 18.0))];
            let _: () = msg_send![tag, setBezeled: NO];
            let _: () = msg_send![tag, setDrawsBackground: NO];
            let _: () = msg_send![tag, setEditable: NO];
            let _: () = msg_send![tag, setSelectable: NO];
            let _: () = msg_send![tag, setWantsLayer: YES];
            let layer: id = msg_send![tag, layer];
            let _: () = msg_send![layer, setCornerRadius: 9.0f64];
            let _: () = msg_send![layer, setMasksToBounds: YES];
            let font: id = msg_send![class!(NSFont), fontWithName: ns("Menlo") size: 11.0f64];
            let _: () = msg_send![tag, setFont: font];
            let _: () = msg_send![tag, setAlignment: 1i64]; // center
            let _: () = msg_send![view, addSubview: tag];

            win.orderFrontRegardless();
            app.finishLaunching();

            let mut tail = if replay {
                JournalTail::replay()
            } else {
                JournalTail::live(events_path).unwrap_or_else(|_| JournalTail::replay())
            };
            let mut terminal_since: Option<Instant> = None;
            // Watchdogs — a writer that dies without a terminal event
            // must not pin a fullscreen window forever.
            let mut last_activity = Instant::now();
            let writer_pid = writer_pid(events_path);
            let mut loops = 0u32;

            loop {
                // Pump pending events so the window server stays happy.
                loop {
                    let mode = ns("kCFRunLoopDefaultMode");
                    let ev: id = msg_send![app,
                        nextEventMatchingMask: u64::MAX
                        untilDate: NSDate::distantPast(nil)
                        inMode: mode
                        dequeue: YES];
                    if ev == nil {
                        break;
                    }
                    let _: () = msg_send![app, sendEvent: ev];
                }

                // Fold new journal events into presence state.
                let mut dirty = false;
                {
                    let mut guard = STATE.write().unwrap();
                    if let Some(s) = guard.as_mut() {
                        for ev in tail.poll(events_path) {
                            reduce(s, &ev);
                            dirty = true;
                        }
                        if dirty {
                            last_activity = Instant::now();
                        }
                        if dirty {
                            let label = format!("{} · {}", s.agent, s.status_line);
                            let _: () = msg_send![tag, setStringValue: ns(&label)];
                            let (r, g, b) = status_color(s.status, s.user_control);
                            let cg: id = msg_send![class!(NSColor), colorWithSRGBRed:r green:g blue:b alpha:1.0f64];
                            let cgref: id = msg_send![cg, CGColor];
                            let _: () = msg_send![layer, setBackgroundColor: cgref];
                        }
                        let done = matches!(
                            s.status,
                            PresenceStatus::Completed
                                | PresenceStatus::Failed
                                | PresenceStatus::Abstained
                                | PresenceStatus::Denied
                        );
                        if done && terminal_since.is_none() {
                            terminal_since = Some(Instant::now());
                        }
                    }
                }

                // Lerp the displayed cursor toward the journal target.
                // No usable bounds (degenerate rect, target-less act, or
                // a terminal event cleared the target): hold the last
                // position; before any placement, anchor lower-center —
                // presence stays on screen, never parked in a corner.
                if let Some(s) = STATE.read().unwrap().clone() {
                    let placed = *DISPLAY.read().unwrap() != (0.0, 0.0);
                    // `cursor` outlives the terminal event that clears
                    // `target` — a fast act read in one poll still lands.
                    let tgt = s.cursor.or_else(|| {
                        (!placed).then(|| (screen_width() * 0.5, screen_height() * 0.72))
                    });
                    if let Some((tx, ty)) = tgt {
                        let mut d = DISPLAY.write().unwrap();
                        if *d == (0.0, 0.0) {
                            *d = (tx, ty); // first frame snaps — no cross-screen slide
                        } else {
                            d.0 += (tx - d.0) * 0.22;
                            d.1 += (ty - d.1) * 0.22;
                        }
                    }
                    // Tag floats below-right of the arrow tip.
                    let d = *DISPLAY.read().unwrap();
                    let w = (label_width(&s) as f64).max(48.0);
                    let h = screen_height();
                    let _: () = msg_send![tag, setFrame: NSRect::new(
                        NSPoint::new(d.0 + 30.0, h - d.1 - 40.0),
                        NSSize::new(w, 18.0))];
                }

                let _: () = msg_send![view, setNeedsDisplay: YES];
                let _: () = msg_send![win, displayIfNeeded];
                std::thread::sleep(Duration::from_millis(33));

                if terminal_since.is_some_and(|t| t.elapsed() > Duration::from_secs(8)) {
                    break; // terminal state shown long enough — leave quietly
                }
                loops += 1;
                if loops.is_multiple_of(30) {
                    // ~1s cadence: writer gone mid-run (crashed task,
                    // killed CLI) means nothing more is coming. A writer
                    // that died AFTER its terminal event still gets the
                    // 8s linger — the check only applies pre-terminal.
                    if terminal_since.is_none() && writer_pid.is_some_and(|pid| !pid_alive(pid)) {
                        break;
                    }
                    // No pid in the name (custom --events path): give up
                    // only after a long silence while not mid-approval.
                    let s = STATE.read().unwrap().clone();
                    let waiting = matches!(
                        s.as_ref().map(|s| s.status),
                        Some(PresenceStatus::WaitingApproval)
                    );
                    if !waiting && last_activity.elapsed() > Duration::from_secs(120) {
                        break;
                    }
                }
            }
        }
    }

    /// `dexter-{pid}*.jsonl` journal names carry the writer's pid —
    /// used to detect a dead writer and drop the window.
    fn writer_pid(path: &Path) -> Option<i32> {
        let stem = path.file_stem()?.to_str()?;
        let rest = stem.strip_prefix("dexter-")?;
        rest.split('-').next()?.parse().ok()
    }

    /// Signal 0 probes existence without delivering anything. (Not
    /// NSRunningApplication — it only knows GUI apps, so a CLI writer
    /// always looked dead and the overlay quit after one second.)
    fn pid_alive(pid: i32) -> bool {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        unsafe { kill(pid, 0) == 0 }
    }

    /// Rough mono-font width estimate for the tag frame.
    fn label_width(s: &PresenceState) -> usize {
        (s.agent.len() + s.status_line.len() + 3) * 7 + 14
    }
}
