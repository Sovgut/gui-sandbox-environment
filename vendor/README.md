# Vendored crates

Third-party crates kept in the tree because gse needs changes to them. Each
directory is the crate as published on crates.io plus the changes listed here;
to move to a newer release, take the new crate and apply the same changes.

The app builds on GPUI Kit, which pins this GPUI snapshot (`gpui-pre`
0.3.10); the workspace patches the crates below in place of the published
ones, so Kit and the app use the same changed GPUI and components.

## gpui-pre 0.3.10 (Apache-2.0)

GPUI's core. Changes, for the environment screen:

- Pointer lock: `Window::set_pointer_lock` (`src/window.rs`) and the
  platform hook `PlatformWindow::set_pointer_lock` (`src/platform.rs`, a no-op
  by default). While the pointer is locked the window gets
  `MouseMotionEvent`s (`src/interactive.rs`, new `PlatformInput::MouseMotion`)
  with the motion as accelerated and as the device reported it; games that
  lock the pointer need the latter.
- Raw keys, so the screen gets keys by their physical position whatever the
  host's layout: `Window::set_raw_key_handler` with the event `RawKeyEvent`
  (the evdev code and whether it went down) and the platform hook
  `PlatformWindow::on_raw_key`. A key the handler consumes produces no other
  event.
- `CursorStyle::Hidden` (`src/platform.rs`): no cursor over an element that
  draws its own; the screen shows the guest's pointer in the frame.

## gpui-pre-linux 0.3.10 (Apache-2.0)

GPUI's Linux backend. Changes, the Wayland side of the above
(`src/linux/wayland/`):

- `client.rs` binds `zwp_pointer_constraints_v1` and
  `zwp_relative_pointer_manager_v1`, locks the pointer on a window's surface
  and sends relative motion to the window under the pointer
  (`WaylandClientStatePtr::set_pointer_lock`). It offers every key, modifiers
  included, to the window's raw key handler before any other handling
  (`wl_keyboard::Event::Key`).
- `window.rs` keeps the raw key callback (`handle_raw_key`) and implements
  both platform hooks. X11 neither locks the pointer nor reports raw keys.
- `CursorStyle::Hidden` sets no cursor surface on Wayland (`client.rs`,
  wherever a style is applied) and the invisible cursor on X11
  (`src/linux/x11/client.rs`, `get_cursor_icon`); `src/linux/wayland.rs` and
  `src/linux/platform.rs` give it a fallback shape and name never used.

## gpui-component 0.8.1 (Apache-2.0)

GPUI Kit's styled components. Changes, so the window looks like a KDE window:

- `src/window_border.rs`: the client-side frame rounds all four corners by
  `BORDER_RADIUS` (10 px, as Breeze does) wherever the window does not meet
  a screen edge, and `window_corner_radius` tells the application the radius
  of each corner now. Both are exported from `src/lib.rs`.
- `src/root.rs`: the root paints no background. GPUI clips to rectangles,
  so a square background would show in the round corners; the application
  paints its own background with the corners `window_corner_radius` gives.
