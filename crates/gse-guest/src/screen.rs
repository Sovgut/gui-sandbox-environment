//! `screen`: copies the desktop into the memory the host shows as the environment
//! screen.
//!
//! The memory is QEMU's `ivshmem-plain` device. Each frame goes three ways:
//! the compositor's screencopy blits it on the GPU into a dmabuf this process
//! allocated, a Vulkan transfer brings it into system memory, and one
//! sequential copy puts it into a slot of the shared region, which is then
//! published. Screencopy into shared memory directly would make the
//! compositor read the frame back itself, which on NVIDIA goes through a
//! CPU-converted 24-bit format at a few frames per second.
//!
//! It runs as `sandbox`: screencopy is one of the privileged protocols `guest`
//! never gets.

use std::fs::{self, File, OpenOptions};
use std::os::fd::AsFd as _;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use gse_screen::readback::{DRM_FORMAT_XRGB8888, Readback};
use gse_screen::{FORMAT_XRGB8888, Geometry, Writer};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::{
    self, ZwpLinuxBufferParamsV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::{
    self, ZwpLinuxDmabufV1,
};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{
    self, ZwlrScreencopyFrameV1,
};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

/// QEMU's ivshmem device.
const IVSHMEM_VENDOR: &str = "0x1af4";
const IVSHMEM_DEVICE: &str = "0x1110";

/// Finds the ivshmem device in sysfs.
fn find_device() -> Result<PathBuf> {
    for entry in fs::read_dir("/sys/bus/pci/devices")? {
        let dir = entry?.path();
        let read = |name| fs::read_to_string(dir.join(name)).unwrap_or_default();
        if read("vendor").trim() == IVSHMEM_VENDOR && read("device").trim() == IVSHMEM_DEVICE {
            return Ok(dir);
        }
    }
    bail!("this VM has no environment screen memory (ivshmem)")
}

fn open_rw(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

enum Outcome {
    Pending,
    Ready,
    Failed,
}

/// The GPU target the compositor copies into, and its `wl_buffer`.
struct Target {
    readback: Readback,
    buffer: WlBuffer,
}

struct State {
    writer: Writer,
    dmabuf: ZwpLinuxDmabufV1,
    /// Modifiers the compositor accepts for `XRGB8888` dmabufs.
    modifiers: Vec<u64>,
    target: Option<Target>,
    /// The size the compositor asked for in this round.
    size: Option<(u32, u32)>,
    outcome: Outcome,
    error: Option<anyhow::Error>,
    /// Whether a frame went out yet. The first copy is immediate; later ones
    /// wait for the screen to change.
    started: bool,
}

impl State {
    /// The target for frames of `size`, made anew when the size changes.
    fn target(&mut self, size: (u32, u32), qh: &QueueHandle<Self>) -> Result<&WlBuffer> {
        let current = self
            .target
            .as_ref()
            .map(|t| (t.readback.width, t.readback.height));
        if current != Some(size) {
            if let Some(old) = self.target.take() {
                old.buffer.destroy();
            }
            let readback = Readback::new(size.0, size.1, 1, &self.modifiers)?;
            let t = &readback.targets[0];
            let params = self.dmabuf.create_params(qh, ());
            #[allow(clippy::cast_possible_truncation)]
            params.add(
                t.fd.as_fd(),
                0,
                t.offset,
                t.stride,
                (t.modifier >> 32) as u32,
                t.modifier as u32,
            );
            #[allow(clippy::cast_possible_wrap)]
            let buffer = params.create_immed(
                size.0 as i32,
                size.1 as i32,
                DRM_FORMAT_XRGB8888,
                zwp_linux_buffer_params_v1::Flags::empty(),
                qh,
                (),
            );
            params.destroy();
            self.target = Some(Target { readback, buffer });
        }
        Ok(&self.target.as_ref().expect("target was just set").buffer)
    }

    /// Brings the copied frame into the shared region and publishes it.
    fn publish(&mut self) -> Result<()> {
        let target = self.target.as_mut().ok_or_else(|| anyhow!("no target"))?;
        let slot = self.writer.begin();
        let out = self
            .writer
            .slot_mut(slot)
            .ok_or_else(|| anyhow!("the shared region is not mapped"))?;
        target.readback.read(0, out)?;
        let (width, height) = (target.readback.width, target.readback.height);
        self.writer.publish(
            slot,
            Geometry {
                width,
                height,
                stride: width * 4,
                format: FORMAT_XRGB8888,
            },
        );
        Ok(())
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        frame: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event;
        match event {
            Event::LinuxDmabuf {
                format: DRM_FORMAT_XRGB8888,
                width,
                height,
            } => state.size = Some((width, height)),
            Event::BufferDone => {
                let Some(size) = state.size else {
                    state.error = Some(anyhow!("the compositor offers no XRGB8888 dmabuf"));
                    state.outcome = Outcome::Failed;
                    return;
                };
                let started = state.started;
                match state.target(size, qh) {
                    Ok(buffer) if started => frame.copy_with_damage(buffer),
                    Ok(buffer) => frame.copy(buffer),
                    Err(e) => {
                        state.error = Some(e);
                        state.outcome = Outcome::Failed;
                    }
                }
            }
            Event::Ready { .. } => {
                if let Err(e) = state.publish() {
                    state.error = Some(e);
                    state.outcome = Outcome::Failed;
                } else {
                    state.outcome = Outcome::Ready;
                    state.started = true;
                }
                frame.destroy();
            }
            Event::Failed => {
                eprintln!("the compositor failed to copy a frame");
                state.outcome = Outcome::Failed;
                frame.destroy();
            }
            _ => {}
        }
    }
}

/// Copies frames until the compositor goes away.
pub fn serve() -> Result<()> {
    let device = find_device()?;
    // One write-combining mapping (`resource2_wc`) for header and pixels:
    // large sequential writes are fast there. A second, uncached mapping of
    // the same BAR would conflict in the page attribute table and turn the
    // whole region uncached. The writer's fences drain the write-combining
    // buffers before each publish.
    let wc = device.join("resource2_wc");
    let region = open_rw(&if wc.exists() {
        wc
    } else {
        device.join("resource2")
    })?;
    let mut writer = Writer::new(&region).context("mapping the environment screen header")?;
    writer
        .map_pixels(&region)
        .context("mapping the environment screen")?;

    let conn = Connection::connect_to_env().context("connecting to the compositor")?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let output: WlOutput = globals.bind(&qh, 1..=4, ())?;
    let dmabuf: ZwpLinuxDmabufV1 = globals
        .bind(&qh, 3..=3, ())
        .context("the compositor offers no linux-dmabuf")?;
    let manager: ZwlrScreencopyManagerV1 = globals
        .bind(&qh, 3..=3, ())
        .context("the compositor offers no screencopy")?;

    let mut state = State {
        writer,
        dmabuf,
        modifiers: Vec::new(),
        target: None,
        size: None,
        outcome: Outcome::Pending,
        error: None,
        started: false,
    };
    // The compositor lists its dmabuf formats and modifiers right after the
    // bind.
    queue.roundtrip(&mut state)?;
    loop {
        state.size = None;
        state.outcome = Outcome::Pending;
        manager.capture_output(1, &output, &qh, ());
        while matches!(state.outcome, Outcome::Pending) {
            queue.blocking_dispatch(&mut state)?;
        }
        if let Some(e) = state.error.take() {
            eprintln!("capturing a frame: {e:#}");
        }
        if matches!(state.outcome, Outcome::Failed) {
            sleep(Duration::from_millis(200));
        }
    }
}

macro_rules! ignore_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for State {
            fn event(
                _: &mut Self,
                _: &$proxy,
                _: <$proxy as wayland_client::Proxy>::Event,
                (): &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}

impl Dispatch<ZwpLinuxDmabufV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxDmabufV1,
        event: zwp_linux_dmabuf_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_linux_dmabuf_v1::Event::Modifier {
            format: DRM_FORMAT_XRGB8888,
            modifier_hi,
            modifier_lo,
        } = event
        {
            state
                .modifiers
                .push(u64::from(modifier_hi) << 32 | u64::from(modifier_lo));
        }
    }
}

ignore_events!(
    WlBuffer,
    WlOutput,
    ZwpLinuxBufferParamsV1,
    ZwlrScreencopyManagerV1
);

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
