//! Graphical login sessions, as `loginctl` reports them.

/// A Wayland or X11 session and the user it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub user: String,
}

/// Session IDs from `loginctl list-sessions --no-legend`.
#[must_use]
pub fn parse_session_ids(list: &str) -> Vec<String> {
    list.lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// A graphical session from `loginctl show-session <id> -p Type -p Name`,
/// or `None` for TTY, manager and other non-graphical sessions.
#[must_use]
pub fn parse_graphical_session(id: &str, properties: &str) -> Option<Session> {
    let value = |key: &str| {
        properties
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    match value("Type")? {
        "wayland" | "x11" | "mir" => Some(Session {
            id: id.to_owned(),
            user: value("Name")?.to_owned(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_ids_and_keeps_graphical_sessions_only() {
        let list = "2 1000 dreamer seat0 3010 user    tty2 no -\n3 1000 dreamer -     3016 manager -    no -\n";
        assert_eq!(parse_session_ids(list), ["2", "3"]);
        assert_eq!(
            parse_graphical_session("2", "Type=wayland\nName=dreamer\n"),
            Some(Session {
                id: "2".into(),
                user: "dreamer".into()
            })
        );
        assert_eq!(
            parse_graphical_session("3", "Type=unspecified\nName=dreamer\n"),
            None
        );
    }
}
