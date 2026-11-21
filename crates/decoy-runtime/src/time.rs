use std::time::Duration;

use jiff::Timestamp;
use jiff::fmt::friendly::{Designator, SpanParser, SpanPrinter};

const DAY_SECONDS: u64 = 86_400;
const MONTH_SECONDS: u64 = 2_630_016;
const YEAR_SECONDS: u64 = 31_557_600;

const DURATION_PARSER: SpanParser = SpanParser::new();
const DURATION_PRINTER: SpanPrinter = SpanPrinter::new().designator(Designator::HumanTime);

/// Parses a duration expressed in `ms` or `s`.
pub(crate) fn parse_duration(input: &str) -> Option<Duration> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    input
        .split_inclusive('s')
        .try_fold(Duration::ZERO, |total, term| {
            let term = term.trim_start();
            let number = term
                .strip_suffix("ms")
                .or_else(|| term.strip_suffix('s'))?
                .trim_end();
            let starts_with_digit = number.starts_with(|c: char| c.is_ascii_digit());
            let ends_with_digit = number.ends_with(|c: char| c.is_ascii_digit());
            if !starts_with_digit
                || !ends_with_digit
                || !number.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            {
                return None;
            }
            total.checked_add(DURATION_PARSER.parse_unsigned_duration(term).ok()?)
        })
}

/// Formats the duration for a timeout error.
pub(crate) fn format_duration(duration: Duration) -> String {
    let mut seconds = duration.as_secs();
    let mut parts = Vec::new();
    for (name, unit) in [
        ("year", YEAR_SECONDS),
        ("month", MONTH_SECONDS),
        ("day", DAY_SECONDS),
    ] {
        let count = seconds / unit;
        seconds %= unit;
        if count > 0 {
            let plural = if count == 1 { "" } else { "s" };
            parts.push(format!("{count}{name}{plural}"));
        }
    }
    let remainder = Duration::new(seconds, duration.subsec_nanos());
    if parts.is_empty() || !remainder.is_zero() {
        parts.push(DURATION_PRINTER.unsigned_duration_to_string(&remainder));
    }
    parts.join(" ")
}

/// Current UTC time in RFC 3339 format with milliseconds.
pub(crate) fn now_rfc3339() -> String {
    format!("{:.3}", Timestamp::now())
}

/// Current Unix time in whole seconds.
pub(crate) fn now_unix_seconds() -> i64 {
    Timestamp::now().as_second().max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsupported_duration_units_and_syntax() {
        for input in [
            "", "2", "1m", "1ns", "2sec", "2s 1m", "1s 2", "-1s", "+1s", "1.ms", "1.2.3s",
        ] {
            assert_eq!(parse_duration(input), None, "{input}");
        }
    }

    #[test]
    fn formats_timeout_components() {
        let duration = Duration::new(YEAR_SECONDS + 2 * DAY_SECONDS + 3723, 400_005_006);
        assert_eq!(
            format_duration(duration),
            "1year 2days 1h 2m 3s 400ms 5us 6ns"
        );
        assert_eq!(format_duration(Duration::ZERO), "0s");
    }
}
