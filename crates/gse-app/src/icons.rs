//! The app's icons: small line drawings on a 24 by 24 grid, kept in the
//! binary and served to gpui as `icons/gse/<name>.svg`, next to GPUI Kit's
//! own icons. gpui paints an icon in the text colour of its element, so the
//! drawings only need a shape.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Settings,
    Sliders,
    Play,
    Stop,
    Reset,
    Trash,
    Plus,
    Search,
    SortNewest,
    SortName,
    SortPreset,
    Close,
    Upload,
    Download,
    Refresh,
    Edit,
    Transfer,
    Copy,
    Folder,
    Monitor,
    Fullscreen,
    ExitFullscreen,
    Loader,
    WindowMinimize,
    WindowMaximize,
    WindowRestore,
    WindowClose,
}

impl Icon {
    const ALL: [Self; 27] = [
        Self::Settings,
        Self::Sliders,
        Self::Play,
        Self::Stop,
        Self::Reset,
        Self::Trash,
        Self::Plus,
        Self::Search,
        Self::SortNewest,
        Self::SortName,
        Self::SortPreset,
        Self::Close,
        Self::Upload,
        Self::Download,
        Self::Refresh,
        Self::Edit,
        Self::Transfer,
        Self::Copy,
        Self::Folder,
        Self::Monitor,
        Self::Fullscreen,
        Self::ExitFullscreen,
        Self::Loader,
        Self::WindowMinimize,
        Self::WindowMaximize,
        Self::WindowRestore,
        Self::WindowClose,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Sliders => "sliders",
            Self::Play => "play",
            Self::Stop => "stop",
            Self::Reset => "reset",
            Self::Trash => "trash",
            Self::Plus => "plus",
            Self::Search => "search",
            Self::SortNewest => "sort-newest",
            Self::SortName => "sort-name",
            Self::SortPreset => "sort-preset",
            Self::Close => "close",
            Self::Upload => "upload",
            Self::Download => "download",
            Self::Refresh => "refresh",
            Self::Edit => "edit",
            Self::Transfer => "transfer",
            Self::Copy => "copy",
            Self::Folder => "folder",
            Self::Monitor => "monitor",
            Self::Fullscreen => "fullscreen",
            Self::ExitFullscreen => "exit-fullscreen",
            Self::Loader => "loader",
            Self::WindowMinimize => "window-minimize",
            Self::WindowMaximize => "window-maximize",
            Self::WindowRestore => "window-restore",
            Self::WindowClose => "window-close",
        }
    }

    /// The asset path gpui loads the icon from.
    pub fn path(self) -> SharedString {
        format!("icons/gse/{}.svg", self.name()).into()
    }

    /// The drawing: SVG elements on the 24 by 24 grid, stroked.
    fn drawing(self) -> &'static str {
        match self {
            Self::Settings => {
                r#"<circle cx="12" cy="12" r="3"/><circle cx="12" cy="12" r="6.5"/><path d="M18.5 12H21.5M16.6 16.6L18.72 18.72M12 18.5V21.5M7.4 16.6L5.28 18.72M5.5 12H2.5M7.4 7.4L5.28 5.28M12 5.5V2.5M16.6 7.4L18.72 5.28"/>"#
            }
            Self::Sliders => {
                r#"<path d="M4 6H11M15 6H20M4 12H6M10 12H20M4 18H14M18 18H20"/><circle cx="13" cy="6" r="2"/><circle cx="8" cy="12" r="2"/><circle cx="16" cy="18" r="2"/>"#
            }
            Self::Play => r#"<path d="M7 4.5V19.5L19.5 12Z" fill="black"/>"#,
            Self::Stop => r#"<rect x="6" y="6" width="12" height="12" rx="1.5" fill="black"/>"#,
            Self::Reset => r#"<path d="M4 12A8 8 0 1 0 6.34 6.34L4 8.7"/><path d="M4 4V8.7H8.7"/>"#,
            Self::Trash => {
                r#"<path d="M4 7H20M9 7V4.5H15V7M6.5 7L7.5 20H16.5L17.5 7M10.5 11V16M13.5 11V16"/>"#
            }
            Self::Plus => r#"<path d="M12 5V19M5 12H19"/>"#,
            Self::Search => r#"<circle cx="10.5" cy="10.5" r="6"/><path d="M15 15L20 20"/>"#,
            Self::SortNewest => r#"<circle cx="12" cy="12" r="8"/><path d="M12 7.5V12L15 14"/>"#,
            Self::SortName => {
                r#"<path d="M3.5 11L6 4L8.5 11M4.4 8.5H7.6M3.5 14H8.5L3.5 20H8.5M17 4V20M13.5 16.5L17 20L20.5 16.5"/>"#
            }
            Self::SortPreset => {
                r#"<path d="M12 4L20 8L12 12L4 8Z"/><path d="M4 12L12 16L20 12M4 16L12 20L20 16"/>"#
            }
            Self::Close => r#"<path d="M6 6L18 18M18 6L6 18"/>"#,
            Self::Upload => r#"<path d="M12 16V4M7 9L12 4L17 9M4 15V20H20V15"/>"#,
            Self::Download => r#"<path d="M12 4V16M7 11L12 16L17 11M4 15V20H20V15"/>"#,
            Self::Edit => r#"<path d="M4 20H8L19 9L15 5L4 16Z"/><path d="M13 7L17 11"/>"#,
            Self::Transfer => {
                r#"<path d="M7 20V8M3.5 11.5L7 8L10.5 11.5M17 4V16M13.5 12.5L17 16L20.5 12.5"/>"#
            }
            Self::Copy => {
                r#"<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5.5A1.5 1.5 0 0 0 14.5 4H5.5A1.5 1.5 0 0 0 4 5.5V14.5A1.5 1.5 0 0 0 5.5 16H8"/>"#
            }
            Self::Folder => r#"<path d="M3 7V18H21V9H12L10 7Z"/>"#,
            Self::Monitor => {
                r#"<rect x="3" y="5" width="18" height="12" rx="2"/><path d="M8 21H16M12 17V21"/>"#
            }
            Self::Fullscreen => r#"<path d="M4 9V4H9M20 9V4H15M4 15V20H9M20 15V20H15"/>"#,
            Self::ExitFullscreen => r#"<path d="M9 4V9H4M15 4V9H20M4 15H9V20M20 15H15V20"/>"#,
            // Three quarters of a ring, turned while something runs.
            Self::Loader => r#"<path d="M12 3A9 9 0 1 1 3 12"/>"#,
            // The window buttons, drawn the way KDE's Breeze draws them.
            Self::WindowMinimize => r#"<path d="M6 9.5L12 15.5L18 9.5"/>"#,
            Self::WindowMaximize => r#"<path d="M6 14.5L12 8.5L18 14.5"/>"#,
            Self::WindowRestore => r#"<path d="M12 6L18 12L12 18L6 12Z"/>"#,
            Self::WindowClose => r#"<path d="M7 7L17 17M17 7L7 17"/>"#,
            Self::Refresh => {
                r#"<path d="M20 12A8 8 0 1 1 17.66 6.34L20 8.7"/><path d="M20 4V8.7H15.3"/>"#
            }
        }
    }

    fn svg(self) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
            self.drawing()
        )
    }
}

/// Serves [`Icon`]s to gpui, and GPUI Kit's icons that its components draw.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match Icon::ALL.iter().find(|icon| icon.path() == path) {
            Some(icon) => Ok(Some(Cow::Owned(icon.svg().into_bytes()))),
            // GPUI Kit's source answers an unknown path with an error;
            // gpui takes `None` as "not found".
            None => Ok(gpui_kit::assets::Assets.load(path).ok().flatten()),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut listed: Vec<SharedString> = Icon::ALL
            .iter()
            .map(|icon| icon.path())
            .filter(|p| p.starts_with(path))
            .collect();
        listed.extend(gpui_kit::assets::Assets.list(path)?);
        Ok(listed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_served_as_svg() {
        for icon in Icon::ALL {
            let bytes = Assets.load(&icon.path()).unwrap().unwrap();
            assert!(bytes.starts_with(b"<svg"));
        }
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
        assert!(Assets.load("icons/check.svg").unwrap().is_some());
        assert_eq!(Assets.list("icons/gse/").unwrap().len(), Icon::ALL.len());
    }
}
