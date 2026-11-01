# Vendored crates

Third-party crates kept in the tree because gse needs changes to them. Each
directory is the crate as published on crates.io plus the changes listed here;
to move to a newer release, take the new crate and apply the same changes.

## gpui 0.2.2 (Apache-2.0)

Zed's UI framework, used by `crates/gse-app`. Changes:

- `src/taffy.rs`: the grid track literals are `0.0_f32` and `1.0_f32`, which
  current Rust requires where it used to fall back to `f32`.
- Pointer lock, for the environment screen's mouse capture:
  `Window::set_pointer_lock(bool)` (`src/window.rs`) with a default no-op
  `PlatformWindow::set_pointer_lock` (`src/platform.rs`), and the event
  `MouseMotionEvent` with the input `PlatformInput::MouseMotion`
  (`src/interactive.rs`), carrying accelerated and unaccelerated relative
  motion. The Wayland backend (`src/platform/linux/wayland/client.rs`,
  `window.rs`) binds `zwp_pointer_constraints_v1` and
  `zwp_relative_pointer_manager_v1`, locks the pointer on the window's
  surface and sends relative motion to the window under the pointer. X11 does
  not lock.
- Raw keys, so the environment screen gets keys by their physical position
  whatever the host's layout: `Window::set_raw_key_handler` (`src/window.rs`)
  with the event `RawKeyEvent` (`src/interactive.rs`, the evdev code and
  whether it went down) and a default no-op `PlatformWindow::on_raw_key`
  (`src/platform.rs`). The Wayland backend offers every key, modifiers
  included, to the handler before any other handling (`wl_keyboard::Event::Key`
  in `client.rs`, `handle_raw_key` in `window.rs`); a key the handler consumes
  produces no other event. X11 offers none.
- Copying after a click: the Wayland backend sets the clipboard with the
  newer of the last key press and mouse press serials
  (`write_to_clipboard` in `src/platform/linux/wayland/client.rs`); with the
  key press serial alone the compositor refuses a copy a click asked for.

## xattr 0.2.3 (MIT or Apache-2.0)

Pulled in by gpui's HTTP client through `zed-async-tar`, with no way to turn it
off from outside. Changes:

- `src/sys/mod.rs`: `ENOATTR` is `libc::ENODATA`. Current `libc` releases no
  longer define `ENOATTR` for Linux, where the two are the same error.
