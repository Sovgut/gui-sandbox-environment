//! The journal filter. Its words are a regular expression, matched without
//! regard to case against each line as `YYYY-MM-DD HH:MM:SS text`, so times
//! can be searched as text too. Words starting with `@` keep a time or a
//! span of time instead:
//!
//! - `@14:05`, `@14:05:30`: that minute or second, on any day;
//! - `@2026-11-01`: that day; `@2026-11-01T14:05`: that minute of that day;
//! - `@14:00..14:30`, `@2026-10-30..2026-11-01`: from the first through the
//!   last, either of which may be left out (`@14:00..` is from 14:00 on);
//!   two times of day may also be joined by `-` (`@14:00-14:30`).
//!
//! A line has to pass every `@` word and the expression. An expression that
//! does not compile is matched as plain text, and the filter says so.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone as _, Timelike as _};
use regex::{Regex, RegexBuilder};

/// One side of a span: what it names and how much of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Point {
    /// A time of day, on any day.
    Time(NaiveTime, Unit),
    Date(NaiveDate),
    DateTime(NaiveDateTime, Unit),
}

/// The precision a time was written with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Minute,
    Second,
}

/// The lines from `from` through `to`; a missing side is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    from: Option<Point>,
    to: Option<Point>,
}

pub struct Filter {
    pattern: Option<Regex>,
    /// Whether the words compiled as a regular expression, not as text.
    pub expression: bool,
    spans: Vec<Span>,
    /// What the user should know about the filter as typed.
    pub problem: Option<String>,
}

/// How far back the journal shows, besides the filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Everything,
    LastHour,
    Today,
    Week,
}

impl Period {
    pub const ALL: [Self; 4] = [Self::Everything, Self::Today, Self::LastHour, Self::Week];

    pub const fn text(self) -> &'static str {
        match self {
            Self::Everything => "Everything",
            Self::LastHour => "Last hour",
            Self::Today => "Today",
            Self::Week => "This week",
        }
    }

    /// Whether a line written at `time` (seconds since the epoch) is in it.
    pub fn keeps(self, time: u64) -> bool {
        let Some(when) = i64::try_from(time)
            .ok()
            .and_then(|t| Local.timestamp_opt(t, 0).single())
        else {
            return self == Self::Everything;
        };
        let now = Local::now();
        match self {
            Self::Everything => true,
            Self::LastHour => now - when <= chrono::Duration::hours(1),
            Self::Today => when.date_naive() == now.date_naive(),
            Self::Week => now - when <= chrono::Duration::days(7),
        }
    }
}

fn time(text: &str) -> Option<(NaiveTime, Unit)> {
    NaiveTime::parse_from_str(text, "%H:%M:%S")
        .map(|t| (t, Unit::Second))
        .or_else(|_| NaiveTime::parse_from_str(text, "%H:%M").map(|t| (t, Unit::Minute)))
        .ok()
}

fn point(text: &str) -> Option<Point> {
    if let Some((date, clock)) = text.split_once('T') {
        let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
        let (clock, unit) = time(clock)?;
        return Some(Point::DateTime(date.and_time(clock), unit));
    }
    if let Some((clock, unit)) = time(text) {
        return Some(Point::Time(clock, unit));
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .map(Point::Date)
}

/// The span an `@` word names, without its `@`.
fn span(text: &str) -> Option<Span> {
    let side = |s: &str| {
        if s.is_empty() {
            Some(None)
        } else {
            point(s).map(Some)
        }
    };
    if let Some((from, to)) = text.split_once("..") {
        let span = Span {
            from: side(from)?,
            to: side(to)?,
        };
        return (span.from.is_some() || span.to.is_some()).then_some(span);
    }
    if let Some((from, to)) = text.split_once('-')
        && let (Some((from, fu)), Some((to, tu))) = (time(from), time(to))
    {
        return Some(Span {
            from: Some(Point::Time(from, fu)),
            to: Some(Point::Time(to, tu)),
        });
    }
    let one = point(text)?;
    Some(Span {
        from: Some(one),
        to: Some(one),
    })
}

/// Seconds of the day `time` starts at, and the last second it covers.
fn seconds(time: NaiveTime, unit: Unit) -> (u32, u32) {
    let start = time.num_seconds_from_midnight();
    match unit {
        Unit::Second => (start, start),
        Unit::Minute => (start, start + 59),
    }
}

impl Span {
    fn keeps(&self, when: &DateTime<Local>) -> bool {
        let day = when.date_naive();
        let at = when.naive_local();
        let second = at.time().num_seconds_from_midnight();
        let last = |t: NaiveDateTime, u: Unit| match u {
            Unit::Second => t,
            Unit::Minute => t + chrono::Duration::seconds(59),
        };
        let after = |p: Point| match p {
            Point::Time(t, u) => second >= seconds(t, u).0,
            Point::Date(d) => day >= d,
            Point::DateTime(t, _) => at >= t,
        };
        let before = |p: Point| match p {
            Point::Time(t, u) => second <= seconds(t, u).1,
            Point::Date(d) => day <= d,
            Point::DateTime(t, u) => at <= last(t, u),
        };
        self.from.is_none_or(after) && self.to.is_none_or(before)
    }
}

impl Filter {
    pub fn new(text: &str) -> Self {
        let mut spans = Vec::new();
        let mut words = Vec::new();
        let mut problem = None;
        for word in text.split_whitespace() {
            match word.strip_prefix('@') {
                Some(rest) => match span(rest) {
                    Some(span) => spans.push(span),
                    None => {
                        problem = Some(format!(
                            "{word} is not a time: use @14:05, @2026-11-01 or @14:00..14:30"
                        ));
                    }
                },
                None => words.push(word),
            }
        }
        let expression = words.join(" ");
        let build = |p: &str| RegexBuilder::new(p).case_insensitive(true).build();
        let (pattern, compiled) = if expression.is_empty() {
            (None, false)
        } else if let Ok(pattern) = build(&expression) {
            (Some(pattern), true)
        } else {
            problem.get_or_insert_with(|| {
                "Not a regular expression; matched as plain text".to_owned()
            });
            let escaped = build(&regex::escape(&expression)).expect("an escaped pattern compiles");
            (Some(escaped), false)
        };
        Self {
            pattern,
            expression: compiled,
            spans,
            problem,
        }
    }

    /// Whether the line written at `time` (seconds since the epoch) with
    /// `text` passes.
    pub fn keeps(&self, time: u64, text: &str) -> bool {
        let when = i64::try_from(time)
            .ok()
            .and_then(|t| Local.timestamp_opt(t, 0).single());
        let Some(when) = when else {
            return self.spans.is_empty() && self.pattern.is_none();
        };
        if !self.spans.iter().all(|s| s.keeps(&when)) {
            return false;
        }
        self.pattern
            .as_ref()
            .is_none_or(|p| p.is_match(&format!("{} {text}", when.format("%Y-%m-%d %H:%M:%S"))))
    }
}

impl Filter {
    /// Where the expression matches in `text`, to mark them.
    pub fn matches(&self, text: &str) -> Vec<std::ops::Range<usize>> {
        self.pattern.as_ref().map_or_else(Vec::new, |p| {
            p.find_iter(text)
                .filter(|m| !m.is_empty())
                .map(|m| m.range())
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> u64 {
        let local = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").unwrap();
        u64::try_from(Local.from_local_datetime(&local).unwrap().timestamp()).unwrap()
    }

    #[test]
    fn words_are_a_case_blind_regular_expression() {
        let filter = Filter::new("(st)art|boo");
        assert!(filter.problem.is_none());
        assert!(filter.keeps(at("2026-11-01 14:00:00"), "Starting for sovhut"));
        assert!(filter.keeps(at("2026-11-01 14:00:00"), "Booting"));
        assert!(!filter.keeps(at("2026-11-01 14:00:00"), "Stopped"));
        assert!(Filter::new("").keeps(at("2026-11-01 14:00:00"), "anything"));
    }

    #[test]
    fn a_broken_expression_is_matched_as_text() {
        let filter = Filter::new("(cr");
        assert!(filter.problem.is_some());
        assert!(filter.keeps(at("2026-11-01 14:00:00"), "x (CR) y"));
        assert!(!filter.keeps(at("2026-11-01 14:00:00"), "created"));
    }

    #[test]
    fn times_are_text_too() {
        let filter = Filter::new("14:0[0-4]");
        assert!(filter.keeps(at("2026-11-01 14:03:10"), "Booting"));
        assert!(!filter.keeps(at("2026-11-01 14:05:10"), "Booting"));
    }

    #[test]
    fn at_words_keep_a_minute_a_second_or_a_day() {
        let line = at("2026-11-01 14:05:30");
        assert!(Filter::new("@14:05").keeps(line, "x"));
        assert!(!Filter::new("@14:06").keeps(line, "x"));
        assert!(Filter::new("@14:05:30").keeps(line, "x"));
        assert!(!Filter::new("@14:05:31").keeps(line, "x"));
        assert!(Filter::new("@2026-11-01").keeps(line, "x"));
        assert!(!Filter::new("@2026-10-31").keeps(line, "x"));
        assert!(Filter::new("@2026-11-01T14:05").keeps(line, "x"));
        assert!(!Filter::new("@2026-11-02T14:05").keeps(line, "x"));
    }

    #[test]
    fn at_words_keep_spans() {
        let line = at("2026-11-01 14:05:30");
        for kept in [
            "@14:00..14:30",
            "@14:00-14:05",
            "@14:05..",
            "@..14:05",
            "@2026-10-30..2026-11-01",
            "@2026-11-01T14:00..2026-11-01T14:05",
        ] {
            assert!(Filter::new(kept).keeps(line, "x"), "{kept}");
        }
        for dropped in ["@14:06..14:30", "@..14:04", "@2026-11-02..", "@15:00-16:00"] {
            assert!(!Filter::new(dropped).keeps(line, "x"), "{dropped}");
        }
    }

    #[test]
    fn at_words_and_the_expression_combine() {
        let filter = Filter::new("@14:00..14:30 boot");
        assert!(filter.keeps(at("2026-11-01 14:10:00"), "Booting"));
        assert!(!filter.keeps(at("2026-11-01 14:10:00"), "Stopped"));
        assert!(!filter.keeps(at("2026-11-01 15:10:00"), "Booting"));
    }

    #[test]
    fn matches_mark_where_the_expression_is_found() {
        let filter = Filter::new("refus|block");
        assert!(filter.expression);
        assert_eq!(filter.matches("Refused x, blocked"), [0..5, 11..16]);
        assert_eq!(
            Filter::new("@14:00").matches("anything"),
            Vec::<std::ops::Range<usize>>::new()
        );
    }

    #[test]
    fn a_bad_at_word_is_reported() {
        let filter = Filter::new("@yesterday");
        assert!(filter.problem.is_some());
        assert!(Filter::new("@..").problem.is_some());
    }
}
