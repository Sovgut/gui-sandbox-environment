//! The environment screen: frames the guest writes into memory it shares with the
//! host (QEMU `ivshmem-plain` backed by a file in `/dev/shm`).
//!
//! The region starts with a one-page header of little-endian `u64` words,
//! followed by [`SLOTS`] frame slots of [`SLOT_SIZE`] bytes. The guest writes
//! a frame into a free slot, records its geometry in the slot's descriptor,
//! then publishes it:
//!
//! 1. slot `seq` = 0 (being written), pixels, geometry;
//! 2. slot `seq` = frame number;
//! 3. header `latest` = slot, header `published` = frame number.
//!
//! The host reads `published` and `latest`, accepts the slot only if its
//! `seq` equals that frame number before and after copying the pixels, and
//! validates the geometry against the slot size before touching a pixel.
//! Everything in the region is written by the untrusted guest; a reader never
//! trusts more than "these bytes are pixels of at most this size".

#![allow(unsafe_code)]

use std::fs::File;
use std::sync::atomic::{AtomicU64, Ordering, fence};

use memmap2::{Mmap, MmapMut, MmapOptions};

#[cfg(feature = "readback")]
pub mod readback;

pub const MAGIC: u64 = u64::from_le_bytes(*b"GSE-SCRN");
pub const VERSION: u64 = 1;
/// The whole region: a power of two, as an ivshmem BAR must be.
pub const SIZE: usize = 128 << 20;
pub const HEADER: usize = 4096;
pub const SLOTS: usize = 3;
pub const SLOT_SIZE: usize = (SIZE - HEADER) / SLOTS / 4096 * 4096;
/// Neither side of a frame may exceed this.
pub const MAX_SIDE: u32 = 8192;

/// `wl_shm` formats a frame may use; the values are Wayland's.
pub const FORMAT_ARGB8888: u32 = 0;
pub const FORMAT_XRGB8888: u32 = 1;
pub const FORMAT_ABGR8888: u32 = 0x3432_4241;
pub const FORMAT_XBGR8888: u32 = 0x3432_4258;
/// Three bytes per pixel, in memory B, G, R.
pub const FORMAT_RGB888: u32 = 0x3432_4752;
/// Three bytes per pixel, in memory R, G, B.
pub const FORMAT_BGR888: u32 = 0x3432_4742;

/// Bytes per pixel of a known format.
#[must_use]
pub const fn bytes_per_pixel(format: u32) -> Option<u32> {
    match format {
        FORMAT_ARGB8888 | FORMAT_XRGB8888 | FORMAT_ABGR8888 | FORMAT_XBGR8888 => Some(4),
        FORMAT_RGB888 | FORMAT_BGR888 => Some(3),
        _ => None,
    }
}

const WORD_MAGIC: usize = 0;
const WORD_VERSION: usize = 1;
const WORD_PUBLISHED: usize = 2;
const WORD_LATEST: usize = 3;
const WORD_SLOTS: usize = 8;
const SLOT_WORDS: usize = 8;
const SEQ: usize = 0;
const WIDTH: usize = 1;
const HEIGHT: usize = 2;
const STRIDE: usize = 3;
const FORMAT: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("mapping the screen memory: {0}")]
    Map(#[from] std::io::Error),
    #[error("the screen memory is {0} bytes, expected at least {SIZE}")]
    TooSmall(usize),
}

/// Offset of a slot's pixels from the start of the region.
#[must_use]
pub const fn slot_offset(slot: usize) -> usize {
    HEADER + slot * SLOT_SIZE
}

// The pointer is 8-byte aligned: mappings start on a page boundary and
// `index * 8` keeps that alignment, which the assertion checks.
#[allow(clippy::cast_ptr_alignment)]
fn word(header: &[u8], index: usize) -> &AtomicU64 {
    let bytes = &header[index * 8..index * 8 + 8];
    assert!(bytes.as_ptr().addr().is_multiple_of(8));
    // SAFETY: the header lies at the page-aligned start of a mapping that
    // outlives the returned reference, `bytes` is 8 bytes at an 8-byte
    // aligned offset, and every access to it goes through atomics.
    unsafe { &*bytes.as_ptr().cast::<AtomicU64>() }
}

fn slot_word(header: &[u8], slot: usize, field: usize) -> &AtomicU64 {
    word(header, WORD_SLOTS + slot * SLOT_WORDS + field)
}

/// A frame's geometry as the guest described it, checked against the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
}

impl Geometry {
    /// Whether a frame of this geometry fits in a slot and is a format the
    /// host can show.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let Some(bpp) = bytes_per_pixel(self.format) else {
            return false;
        };
        (1..=MAX_SIDE).contains(&self.width)
            && (1..=MAX_SIDE).contains(&self.height)
            && u64::from(self.stride) >= u64::from(self.width) * u64::from(bpp)
            && u64::from(self.stride) * u64::from(self.height) <= SLOT_SIZE as u64
    }
}

/// The guest side: owns the header and hands out slots to fill.
pub struct Writer {
    header: MmapMut,
    pixels: Option<MmapMut>,
    next: usize,
    frame: u64,
}

impl Writer {
    /// Maps the header page of the shared region from `file` (the ivshmem
    /// BAR) and stamps it.
    pub fn new(file: &File) -> Result<Self, Error> {
        // SAFETY: the mapping covers only the header page, which this
        // process alone writes inside the guest; the host only reads it.
        let header = unsafe { MmapOptions::new().len(HEADER).map_mut(file)? };
        for slot in 0..SLOTS {
            slot_word(&header, slot, SEQ).store(0, Ordering::Release);
        }
        word(&header, WORD_PUBLISHED).store(0, Ordering::Release);
        word(&header, WORD_VERSION).store(VERSION, Ordering::Release);
        word(&header, WORD_MAGIC).store(MAGIC, Ordering::Release);
        Ok(Self {
            header,
            pixels: None,
            next: 0,
            frame: 0,
        })
    }

    /// Maps the whole region from `file` for writing pixels into slots. On
    /// the ivshmem BAR this is the write-combining mapping (`resource2_wc`),
    /// where large sequential writes are fast.
    pub fn map_pixels(&mut self, file: &File) -> Result<(), Error> {
        // SAFETY: inside the guest only this process writes the slots, and
        // only into the slot `begin` handed out.
        self.pixels = Some(unsafe { MmapOptions::new().len(SIZE).map_mut(file)? });
        Ok(())
    }

    /// The pixel bytes of `slot`, once [`Writer::map_pixels`] ran.
    pub fn slot_mut(&mut self, slot: usize) -> Option<&mut [u8]> {
        let pixels = self.pixels.as_mut()?;
        pixels.get_mut(slot_offset(slot)..slot_offset(slot) + SLOT_SIZE)
    }

    /// Takes the next slot for writing; its old frame stops being valid.
    pub fn begin(&mut self) -> usize {
        let slot = self.next;
        self.next = (self.next + 1) % SLOTS;
        slot_word(&self.header, slot, SEQ).store(0, Ordering::Release);
        fence(Ordering::SeqCst);
        slot
    }

    /// Publishes `slot` as the newest frame once its pixels are in place.
    pub fn publish(&mut self, slot: usize, geometry: Geometry) {
        self.frame += 1;
        let h = &self.header;
        slot_word(h, slot, WIDTH).store(u64::from(geometry.width), Ordering::Relaxed);
        slot_word(h, slot, HEIGHT).store(u64::from(geometry.height), Ordering::Relaxed);
        slot_word(h, slot, STRIDE).store(u64::from(geometry.stride), Ordering::Relaxed);
        slot_word(h, slot, FORMAT).store(u64::from(geometry.format), Ordering::Relaxed);
        fence(Ordering::SeqCst);
        slot_word(h, slot, SEQ).store(self.frame, Ordering::Release);
        word(h, WORD_LATEST).store(slot as u64, Ordering::Release);
        word(h, WORD_PUBLISHED).store(self.frame, Ordering::Release);
        fence(Ordering::SeqCst);
    }
}

/// The host side: a read-only view of the whole region.
pub struct Reader {
    map: Mmap,
}

/// A copied frame: pixels as `0x00RRGGBB`, `width * height` of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub number: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

impl Reader {
    pub fn new(file: &File) -> Result<Self, Error> {
        // SAFETY: the guest changes this memory at any time. It is only read
        // through atomics (header) or copied byte-wise (pixels) and then
        // validated, so concurrent writes can produce a torn or garbage
        // frame, never undefined behaviour in this process.
        let map = unsafe { Mmap::map(file)? };
        if map.len() < SIZE {
            return Err(Error::TooSmall(map.len()));
        }
        Ok(Self { map })
    }

    /// The number of the newest published frame, 0 before the first one or
    /// while the header is not stamped.
    #[must_use]
    pub fn published(&self) -> u64 {
        let h = &self.map[..HEADER];
        if word(h, WORD_MAGIC).load(Ordering::Acquire) != MAGIC
            || word(h, WORD_VERSION).load(Ordering::Acquire) != VERSION
        {
            return 0;
        }
        word(h, WORD_PUBLISHED).load(Ordering::Acquire)
    }

    /// Copies the newest frame if it is newer than `after`, converting it to
    /// `0x00RRGGBB`. Returns `None` when there is none, or when the guest
    /// overwrote it during the copy.
    #[must_use]
    pub fn read(&self, after: u64, into: Vec<u32>) -> Option<Frame> {
        let h = &self.map[..HEADER];
        let number = self.published();
        if number == 0 || number == after {
            return None;
        }
        let slot = usize::try_from(word(h, WORD_LATEST).load(Ordering::Acquire)).ok()?;
        if slot >= SLOTS || slot_word(h, slot, SEQ).load(Ordering::Acquire) != number {
            return None;
        }
        let field = |f| u32::try_from(slot_word(h, slot, f).load(Ordering::Relaxed)).ok();
        let geometry = Geometry {
            width: field(WIDTH)?,
            height: field(HEIGHT)?,
            stride: field(STRIDE)?,
            format: field(FORMAT)?,
        };
        if !geometry.is_valid() {
            return None;
        }
        let pixels = convert(
            &self.map[slot_offset(slot)..slot_offset(slot) + SLOT_SIZE],
            geometry,
            into,
        );
        fence(Ordering::SeqCst);
        if slot_word(h, slot, SEQ).load(Ordering::Acquire) != number {
            return None;
        }
        Some(Frame {
            number,
            width: geometry.width,
            height: geometry.height,
            pixels,
        })
    }
}

/// Copies a validated frame out of its slot as `0x00RRGGBB` pixels.
fn convert(slot: &[u8], g: Geometry, mut out: Vec<u32>) -> Vec<u32> {
    let (width, height, stride) = (g.width as usize, g.height as usize, g.stride as usize);
    // Byte positions of red and blue within a pixel; green is always 1.
    let (bpp, red, blue) = match g.format {
        FORMAT_ABGR8888 | FORMAT_XBGR8888 => (4, 0, 2),
        FORMAT_BGR888 => (3, 0, 2),
        FORMAT_RGB888 => (3, 2, 0),
        _ => (4, 2, 0),
    };
    out.clear();
    out.reserve(width * height);
    for row in slot.chunks_exact(stride).take(height) {
        out.extend(
            row[..width * bpp]
                .chunks_exact(bpp)
                .map(|p| u32::from(p[red]) << 16 | u32::from(p[1]) << 8 | u32::from(p[blue])),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    fn region() -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.set_len(SIZE as u64).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.flush().unwrap();
        file
    }

    fn put_pixels(file: &File, slot: usize, bytes: &[u8]) {
        let mut map = unsafe { MmapMut::map_mut(file).unwrap() };
        map[slot_offset(slot)..slot_offset(slot) + bytes.len()].copy_from_slice(bytes);
    }

    #[test]
    fn slots_fit_in_the_region() {
        const { assert!(slot_offset(SLOTS) <= SIZE) };
        const { assert!(SLOT_SIZE >= 3840 * 2160 * 4, "a 4K frame fits a slot") };
    }

    #[test]
    fn a_published_frame_reads_back_as_rgb() {
        let file = region();
        let reader = Reader::new(&file).unwrap();
        assert_eq!(reader.read(0, Vec::new()), None, "nothing before stamping");
        let mut writer = Writer::new(&file).unwrap();
        let slot = writer.begin();
        // Two XRGB pixels, little-endian B, G, R, X, then padding to stride.
        put_pixels(&file, slot, &[1, 2, 3, 0, 4, 5, 6, 0, 9, 9, 9, 9]);
        let g = Geometry {
            width: 2,
            height: 1,
            stride: 12,
            format: FORMAT_XRGB8888,
        };
        writer.publish(slot, g);
        let frame = reader.read(0, Vec::new()).unwrap();
        assert_eq!(frame.number, 1);
        assert_eq!(frame.pixels, [0x0003_0201, 0x0006_0504]);
        assert_eq!(reader.read(1, Vec::new()), None, "no newer frame");
    }

    #[test]
    fn bgr_formats_are_swapped() {
        let file = region();
        let reader = Reader::new(&file).unwrap();
        let mut writer = Writer::new(&file).unwrap();
        let slot = writer.begin();
        put_pixels(&file, slot, &[1, 2, 3, 0]);
        let g = Geometry {
            width: 1,
            height: 1,
            stride: 4,
            format: FORMAT_XBGR8888,
        };
        writer.publish(slot, g);
        assert_eq!(reader.read(0, Vec::new()).unwrap().pixels, [0x0001_0203]);
    }

    #[test]
    fn three_byte_formats_read_back() {
        let file = region();
        let reader = Reader::new(&file).unwrap();
        let mut writer = Writer::new(&file).unwrap();
        let slot = writer.begin();
        put_pixels(&file, slot, &[1, 2, 3, 4, 5, 6]);
        let g = Geometry {
            width: 2,
            height: 1,
            stride: 6,
            format: FORMAT_BGR888,
        };
        writer.publish(slot, g);
        assert_eq!(
            reader.read(0, Vec::new()).unwrap().pixels,
            [0x0001_0203, 0x0004_0506]
        );
        assert!(
            !Geometry { stride: 5, ..g }.is_valid(),
            "stride below width"
        );
    }

    #[test]
    fn geometry_from_the_guest_is_bounded() {
        let ok = Geometry {
            width: 2560,
            height: 1440,
            stride: 2560 * 4,
            format: FORMAT_XRGB8888,
        };
        assert!(ok.is_valid());
        assert!(
            !Geometry { stride: 4, ..ok }.is_valid(),
            "stride below width"
        );
        assert!(
            !Geometry {
                height: 100_000,
                ..ok
            }
            .is_valid()
        );
        assert!(!Geometry { format: 7, ..ok }.is_valid());
        let too_big = Geometry {
            width: 8192,
            height: 8192,
            stride: 8192 * 4,
            ..ok
        };
        assert!(!too_big.is_valid(), "larger than a slot");
    }

    #[test]
    fn a_slot_reused_during_the_copy_is_rejected() {
        let file = region();
        let reader = Reader::new(&file).unwrap();
        let mut writer = Writer::new(&file).unwrap();
        let slot = writer.begin();
        let g = Geometry {
            width: 1,
            height: 1,
            stride: 4,
            format: FORMAT_XRGB8888,
        };
        writer.publish(slot, g);
        // The guest takes the same slot again (after SLOTS frames) while the
        // header still names it as the latest.
        for _ in 0..SLOTS {
            writer.begin();
        }
        assert_eq!(reader.read(0, Vec::new()), None);
    }
}
