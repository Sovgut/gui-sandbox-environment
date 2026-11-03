//! Host-side model of gse.
//!
//! Everything that reads the machine goes through a [`Root`], so the same code
//! runs against `/` in production and against a fake tree in tests. Changes to
//! the machine are expressed as a [`plan::Plan`] of [`plan::Step`]s and carried
//! out by [`exec::Executor`], which can also only log them (dry run). An environment
//! VM is described by a [`vm::VmSpec`], which renders to a QEMU command line.

pub mod drivers;
pub mod exec;
pub mod families;
pub mod hardware;
pub mod holders;
#[cfg(test)]
mod machines;
pub mod net;
pub mod pci;
pub mod plan;
pub mod preset;
pub mod root;
pub mod seed;
pub mod session;
pub mod usb;
pub mod vm;

pub use root::Root;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid PCI address '{0}'")]
    PciAddress(String),
    #[error("no GPU at {0}")]
    NoGpu(String),
    #[error("{0}")]
    Command(String),
    #[error("{0}")]
    Step(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io_error(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.into();
    move |source| Error::Io { path, source }
}
