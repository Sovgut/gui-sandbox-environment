//! The host user's keyboard layouts, which a sandbox takes at each start:
//! the layouts and options KDE keeps in `kxkbrc`, and KDE's own shortcut for
//! switching layouts as the XKB option that does the same. Elsewhere, the
//! system's X11 keyboard as `localectl` reports it.

use std::path::PathBuf;
use std::process::Command;

use gse_protocol::Keyboard;

/// The host user's keyboard, if anything says what it is.
pub fn host() -> Option<Keyboard> {
    let keyboard = kde().or_else(localectl)?;
    tracing::info!(
        "keyboard: {} with {}",
        keyboard.layouts.join(","),
        keyboard.options.join(",")
    );
    Some(keyboard)
}

fn config(name: &str) -> Option<String> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    std::fs::read_to_string(dir.join(name)).ok()
}

/// The value of `key` in the `[group]` of an INI-like file.
fn value<'a>(text: &'a str, group: &str, key: &str) -> Option<&'a str> {
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == format!("[{group}]");
        } else if inside
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim());
        }
    }
    None
}

fn kde() -> Option<Keyboard> {
    let kxkbrc = config("kxkbrc")?;
    from_kde(&kxkbrc, config("kglobalshortcutsrc").as_deref())
}

/// The keyboard KDE's files describe.
fn from_kde(kxkbrc: &str, shortcuts: Option<&str>) -> Option<Keyboard> {
    if value(kxkbrc, "Layout", "Use") != Some("true") {
        return None;
    }
    let layouts = value(kxkbrc, "Layout", "LayoutList")?;
    let variants: Vec<&str> = value(kxkbrc, "Layout", "VariantList")
        .unwrap_or("")
        .split(',')
        .collect();
    let layouts: Vec<String> = layouts
        .split(',')
        .filter(|l| !l.is_empty())
        .enumerate()
        .map(
            |(i, layout)| match variants.get(i).filter(|v| !v.is_empty()) {
                Some(variant) => format!("{layout}({variant})"),
                None => layout.to_owned(),
            },
        )
        .collect();
    if layouts.is_empty() {
        return None;
    }
    let mut options: Vec<String> = value(kxkbrc, "Layout", "Options")
        .unwrap_or("")
        .split(',')
        .filter(|o| !o.is_empty())
        .map(str::to_owned)
        .collect();
    let switches = options.iter().any(|o| o.starts_with("grp:"));
    if layouts.len() > 1 && !switches {
        // KDE switches with a shortcut of its own, which XKB knows as an
        // option; one it has no option for falls back to Alt+Shift.
        let shortcut = shortcuts
            .and_then(|s| {
                value(
                    s,
                    "KDE Keyboard Layout Switcher",
                    "Switch to Next Keyboard Layout",
                )
            })
            .and_then(|v| v.split(',').next())
            .unwrap_or("");
        options.push(grp_option(shortcut).to_owned());
    }
    Some(Keyboard { layouts, options })
}

/// The XKB option that switches layouts on KDE's `shortcut`.
fn grp_option(shortcut: &str) -> &'static str {
    match shortcut {
        "Meta+Space" => "grp:win_space_toggle",
        "Ctrl+Shift" | "Shift+Ctrl" => "grp:ctrl_shift_toggle",
        "Alt+Space" => "grp:alt_space_toggle",
        "Ctrl+Space" => "grp:ctrl_space_toggle",
        "Caps Lock" | "CapsLock" => "grp:caps_toggle",
        _ => "grp:alt_shift_toggle",
    }
}

/// The system keyboard from `localectl status`.
fn localectl() -> Option<Keyboard> {
    let out = Command::new("localectl").arg("status").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(name))
            .map(|v| v.trim().to_owned())
    };
    let layouts: Vec<String> = field("X11 Layout:")?
        .split(',')
        .map(str::to_owned)
        .collect();
    let options = field("X11 Options:")
        .map(|o| o.split(',').map(str::to_owned).collect())
        .unwrap_or_default();
    Some(Keyboard { layouts, options })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kde_layouts_take_the_switch_shortcut_as_an_option() {
        let kxkbrc = "[Layout]\nDisplayNames=,\nLayoutList=us,ru\nUse=true\nVariantList=,\n";
        let shortcuts = "[KDE Keyboard Layout Switcher]\nSwitch to Next Keyboard Layout=Meta+Space,Meta+Alt+K,Switch to Next Keyboard Layout\n";
        assert_eq!(
            from_kde(kxkbrc, Some(shortcuts)),
            Some(Keyboard {
                layouts: vec!["us".into(), "ru".into()],
                options: vec!["grp:win_space_toggle".into()],
            })
        );
    }

    #[test]
    fn variants_and_own_options_stay() {
        let kxkbrc = "[Layout]\nLayoutList=us,ua\nVariantList=intl,\nOptions=grp:alt_shift_toggle,caps:escape\nUse=true\n";
        assert_eq!(
            from_kde(kxkbrc, None),
            Some(Keyboard {
                layouts: vec!["us(intl)".into(), "ua".into()],
                options: vec!["grp:alt_shift_toggle".into(), "caps:escape".into()],
            })
        );
        assert_eq!(from_kde("[Layout]\nUse=false\n", None), None);
    }
}
