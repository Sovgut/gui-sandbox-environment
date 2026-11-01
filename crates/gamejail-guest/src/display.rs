//! The restricted Wayland socket for the jail account.

use std::fs;
use std::os::fd::AsFd as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::Path;

use anyhow::{Context, Result};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::security_context::v1::client::wp_security_context_manager_v1::WpSecurityContextManagerV1;
use wayland_protocols::wp::security_context::v1::client::wp_security_context_v1::WpSecurityContextV1;

/// Sandbox engine name the compositor sees for jail clients.
const ENGINE: &str = "org.gamejail";
const APP_ID: &str = "jail";

struct State;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpSecurityContextManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &WpSecurityContextManagerV1,
        _: <WpSecurityContextManagerV1 as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpSecurityContextV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &WpSecurityContextV1,
        _: <WpSecurityContextV1 as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Opens `socket` as a security-context listener of the compositor this
/// process is a client of, and serves until the compositor goes away. The
/// compositor stops accepting on the socket once this process exits.
pub fn serve(socket: &Path) -> Result<()> {
    let conn = Connection::connect_to_env().context("connecting to the compositor")?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let manager: WpSecurityContextManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .context("the compositor has no security-context-v1")?;

    if socket.exists() {
        fs::remove_file(socket).with_context(|| format!("removing {}", socket.display()))?;
    }
    let listener =
        UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    // The directory is setgid `jail`; members of that group may connect.
    fs::set_permissions(socket, fs::Permissions::from_mode(0o660))?;

    // The compositor keeps listening until the read end of this pipe sees the
    // write end close, which happens when this process exits.
    let (close_read, close_write) = std::io::pipe()?;
    let context = manager.create_listener(listener.as_fd(), close_read.as_fd(), &qh, ());
    context.set_sandbox_engine(ENGINE.into());
    context.set_app_id(APP_ID.into());
    context.set_instance_id(std::process::id().to_string());
    context.commit();
    queue.roundtrip(&mut State)?;
    drop(listener);
    drop(close_read);
    eprintln!("jail display ready on {}", socket.display());

    loop {
        queue
            .blocking_dispatch(&mut State)
            .context("the compositor went away")?;
        let _ = &close_write;
    }
}
