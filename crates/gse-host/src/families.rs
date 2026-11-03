//! GPU families by PCI vendor and device ID, and the guest drivers each can
//! run on.
//!
//! NVIDIA hands out device IDs in ascending blocks, one architecture after
//! another, so ranges tell its families apart: everything from `1e00`
//! (Turing) on runs on NVIDIA's open kernel module, `1340` (Maxwell) to
//! `1dff` (Volta) only on the legacy proprietary branch, and anything older
//! on no current NVIDIA driver at all. AMD and Intel IDs follow no order, so
//! their discrete cards are listed by the ID blocks of each chip; an AMD ID
//! missing here is an unknown card, an Intel ID missing here an integrated
//! GPU (Intel's discrete cards are all listed).

/// What gse does with a family: how it is supported and which guest drivers
/// it can run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Turing and newer: the open kernel module.
    NvidiaOpen,
    /// Maxwell, Pascal, Volta: the legacy branch's closed module.
    NvidiaLegacy,
    /// Kepler and older: no current proprietary driver.
    NvidiaOld,
    /// RDNA 2 and newer.
    AmdModern,
    /// GCN 3 to RDNA 1: cards that often fail to reset after a VM.
    AmdResetBug,
    /// GCN 1 and 2: `radeon` by default.
    AmdOld,
    AmdIntegrated,
    /// An AMD device ID not in the tables.
    AmdUnknown,
    /// Alchemist (DG2): `i915` by default, `xe` when forced.
    IntelAlchemist,
    /// Battlemage and newer: `xe` only.
    IntelBattlemage,
    /// DG1, which Intel's drivers take only when forced.
    IntelDg1,
    IntelIntegrated,
    /// A vendor gse knows no driver for.
    Unknown,
}

/// A GPU's family: the name people know it by and what gse does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Family {
    pub name: &'static str,
    pub kind: Kind,
}

impl Family {
    /// The drivers a sandbox can run the GPU with, its default first, as
    /// the guest's kernel module names them.
    #[must_use]
    pub const fn guest_drivers(&self) -> &'static [&'static str] {
        match self.kind {
            Kind::NvidiaOpen | Kind::NvidiaLegacy => &["nvidia", "nouveau"],
            Kind::NvidiaOld => &["nouveau"],
            Kind::AmdModern
            | Kind::AmdResetBug
            | Kind::AmdOld
            | Kind::AmdIntegrated
            | Kind::AmdUnknown => &["amdgpu"],
            Kind::IntelAlchemist => &["i915", "xe"],
            Kind::IntelBattlemage => &["xe"],
            Kind::IntelDg1 | Kind::IntelIntegrated => &["i915"],
            Kind::Unknown => &[],
        }
    }

    /// Whether the GPU is part of the CPU or the board.
    #[must_use]
    pub const fn integrated(&self) -> bool {
        matches!(self.kind, Kind::AmdIntegrated | Kind::IntelIntegrated)
    }
}

pub const NVIDIA: u16 = 0x10de;
pub const AMD: u16 = 0x1002;
pub const INTEL: u16 = 0x8086;

/// The family of the GPU `vendor:device`.
#[must_use]
pub fn family(vendor: u16, device: u16) -> Family {
    let (name, kind) = match vendor {
        NVIDIA => nvidia(device),
        AMD => amd(device),
        INTEL => intel(device),
        _ => ("unknown vendor", Kind::Unknown),
    };
    Family { name, kind }
}

fn nvidia(device: u16) -> (&'static str, Kind) {
    match device {
        0x0000..=0x0fbf => ("NVIDIA Fermi or older", Kind::NvidiaOld),
        0x0fc0..=0x133f => ("NVIDIA Kepler", Kind::NvidiaOld),
        0x15f0..=0x15ff | 0x1b00..=0x1d7f => ("NVIDIA Pascal", Kind::NvidiaLegacy),
        0x1d80..=0x1dff => ("NVIDIA Volta", Kind::NvidiaLegacy),
        0x1340..=0x1aff => ("NVIDIA Maxwell", Kind::NvidiaLegacy),
        0x20b0..=0x20ff | 0x2200..=0x232f | 0x2340..=0x267f => ("NVIDIA Ampere", Kind::NvidiaOpen),
        0x1e00..=0x21ff => ("NVIDIA Turing", Kind::NvidiaOpen),
        0x2330..=0x233f => ("NVIDIA Hopper", Kind::NvidiaOpen),
        0x2680..=0x28ff => ("NVIDIA Ada Lovelace", Kind::NvidiaOpen),
        0x2900..=0x2fff => ("NVIDIA Blackwell", Kind::NvidiaOpen),
        0x3000..=0xffff => ("NVIDIA, newer than Blackwell", Kind::NvidiaOpen),
    }
}

/// AMD discrete chips by device ID block, and the APUs.
const AMD_FAMILIES: &[(u16, u16, &str, Kind)] = &[
    (0x7550, 0x759f, "AMD RDNA 4 (Navi 4x)", Kind::AmdModern),
    (0x7440, 0x74ff, "AMD RDNA 3 (Navi 3x)", Kind::AmdModern),
    (0x73a0, 0x743f, "AMD RDNA 2 (Navi 2x)", Kind::AmdModern),
    (0x7310, 0x737f, "AMD RDNA 1 (Navi 1x)", Kind::AmdResetBug),
    (0x66a0, 0x66af, "AMD Vega 20", Kind::AmdResetBug),
    (0x6860, 0x687f, "AMD Vega 10", Kind::AmdResetBug),
    (0x67c0, 0x67ff, "AMD Polaris", Kind::AmdResetBug),
    (0x6980, 0x699f, "AMD Polaris 12", Kind::AmdResetBug),
    (0x6fdf, 0x6fdf, "AMD Polaris", Kind::AmdResetBug),
    (0x6920, 0x693f, "AMD Tonga", Kind::AmdResetBug),
    (0x7300, 0x730f, "AMD Fiji", Kind::AmdResetBug),
    (0x67a0, 0x67bf, "AMD Hawaii (GCN 2)", Kind::AmdOld),
    (0x6640, 0x665f, "AMD Bonaire (GCN 2)", Kind::AmdOld),
    (0x6780, 0x679f, "AMD Tahiti (GCN 1)", Kind::AmdOld),
    (
        0x6800,
        0x683f,
        "AMD Pitcairn or Cape Verde (GCN 1)",
        Kind::AmdOld,
    ),
    (0x6600, 0x663f, "AMD Oland (GCN 1)", Kind::AmdOld),
    (0x6660, 0x666f, "AMD Hainan (GCN 1)", Kind::AmdOld),
    (
        0x1300,
        0x19ff,
        "AMD integrated graphics",
        Kind::AmdIntegrated,
    ),
    (
        0x9800,
        0x98ff,
        "AMD integrated graphics",
        Kind::AmdIntegrated,
    ),
];

fn amd(device: u16) -> (&'static str, Kind) {
    AMD_FAMILIES
        .iter()
        .find(|(from, to, _, _)| (*from..=*to).contains(&device))
        .map_or(("AMD, not in gse's tables", Kind::AmdUnknown), |f| {
            (f.2, f.3)
        })
}

fn intel(device: u16) -> (&'static str, Kind) {
    match device {
        0x5690..=0x56ff => ("Intel Arc Alchemist", Kind::IntelAlchemist),
        0xe200..=0xe2ff => ("Intel Arc Battlemage", Kind::IntelBattlemage),
        0x4905..=0x4909 => ("Intel DG1", Kind::IntelDg1),
        _ => ("Intel integrated graphics", Kind::IntelIntegrated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_generations_follow_the_id_blocks() {
        assert_eq!(family(NVIDIA, 0x2484).kind, Kind::NvidiaOpen); // RTX 3070
        assert_eq!(family(NVIDIA, 0x2484).name, "NVIDIA Ampere");
        assert_eq!(family(NVIDIA, 0x1e87).name, "NVIDIA Turing"); // RTX 2080
        assert_eq!(family(NVIDIA, 0x2182).name, "NVIDIA Turing"); // GTX 1660 Ti
        assert_eq!(family(NVIDIA, 0x2684).name, "NVIDIA Ada Lovelace"); // RTX 4090
        assert_eq!(family(NVIDIA, 0x2b85).name, "NVIDIA Blackwell"); // RTX 5090
        assert_eq!(family(NVIDIA, 0x1b80).kind, Kind::NvidiaLegacy); // GTX 1080
        assert_eq!(family(NVIDIA, 0x1b80).name, "NVIDIA Pascal");
        assert_eq!(family(NVIDIA, 0x13c2).name, "NVIDIA Maxwell"); // GTX 970
        assert_eq!(family(NVIDIA, 0x1d81).name, "NVIDIA Volta"); // Titan V
        assert_eq!(family(NVIDIA, 0x1180).kind, Kind::NvidiaOld); // GTX 680
    }

    #[test]
    fn amd_and_intel_cards_come_from_their_tables() {
        assert_eq!(family(AMD, 0x73bf).kind, Kind::AmdModern); // RX 6800
        assert_eq!(family(AMD, 0x744c).kind, Kind::AmdModern); // RX 7900 XTX
        assert_eq!(family(AMD, 0x7550).kind, Kind::AmdModern); // RX 9070 XT
        assert_eq!(family(AMD, 0x67df).kind, Kind::AmdResetBug); // RX 580
        assert_eq!(family(AMD, 0x731f).kind, Kind::AmdResetBug); // RX 5700 XT
        assert_eq!(family(AMD, 0x6798).kind, Kind::AmdOld); // HD 7970
        assert_eq!(family(AMD, 0x1638).kind, Kind::AmdIntegrated); // Cezanne
        assert_eq!(family(AMD, 0x7fff).kind, Kind::AmdUnknown);
        assert_eq!(family(INTEL, 0x56a5).kind, Kind::IntelAlchemist); // A380
        assert_eq!(family(INTEL, 0xe20b).kind, Kind::IntelBattlemage); // B580
        assert_eq!(family(INTEL, 0x4680).kind, Kind::IntelIntegrated); // UHD 770
        assert_eq!(family(0x1af4, 0x1050).kind, Kind::Unknown); // virtio-gpu
    }

    #[test]
    fn guest_drivers_put_the_default_first() {
        assert_eq!(
            family(NVIDIA, 0x2484).guest_drivers(),
            ["nvidia", "nouveau"]
        );
        assert_eq!(family(INTEL, 0x56a5).guest_drivers(), ["i915", "xe"]);
        assert_eq!(family(INTEL, 0xe20b).guest_drivers(), ["xe"]);
        assert_eq!(family(0x1af4, 0x1050).guest_drivers(), [] as [&str; 0]);
    }
}
