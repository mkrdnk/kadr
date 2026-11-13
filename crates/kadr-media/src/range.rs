#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Selection {
    Full,
    Partial { start: u64, end: u64 },
    Unsatisfiable,
}

pub(crate) fn select_range(value: &str, length: u64) -> Selection {
    let value = value.trim_matches([' ', '\t']);
    let Some((unit, range)) = value.split_once('=') else {
        return Selection::Full;
    };
    if !unit.eq_ignore_ascii_case("bytes") {
        return Selection::Full;
    }
    let Some((first, last)) = range.trim_matches([' ', '\t']).split_once('-') else {
        return Selection::Full;
    };

    if first.is_empty() {
        let Some(suffix) = decimal(last) else {
            return Selection::Full;
        };
        let suffix = bounded(suffix);
        return if length == 0 || suffix == 0 {
            Selection::Unsatisfiable
        } else {
            Selection::Partial {
                start: length.saturating_sub(suffix),
                end: length - 1,
            }
        };
    }

    let Some(first) = decimal(first) else {
        return Selection::Full;
    };
    let last = if last.is_empty() {
        None
    } else {
        let Some(last) = decimal(last) else {
            return Selection::Full;
        };
        // Compare normalized decimal strings before bounding to preserve ordering
        // even when both endpoints exceed u64::MAX.
        if (first.len(), first) > (last.len(), last) {
            return Selection::Full;
        }
        Some(bounded(last))
    };
    let start = bounded(first);
    if start >= length {
        return Selection::Unsatisfiable;
    }
    Selection::Partial {
        start,
        end: last.unwrap_or(length - 1).min(length - 1),
    }
}

fn decimal(value: &str) -> Option<&str> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let normalized = value.trim_start_matches('0');
    Some(if normalized.is_empty() {
        "0"
    } else {
        normalized
    })
}

fn bounded(value: &str) -> u64 {
    value.parse().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{Selection, select_range};
    use Selection::{Full, Partial, Unsatisfiable};

    #[test]
    fn selects_closed_open_and_suffix_ranges() {
        for (value, expected) in [
            ("bytes=0-0", Partial { start: 0, end: 0 }),
            ("bytes=2-5", Partial { start: 2, end: 5 }),
            ("bytes=2-99", Partial { start: 2, end: 9 }),
            ("bytes=0-9", Partial { start: 0, end: 9 }),
            ("bytes=2-", Partial { start: 2, end: 9 }),
            ("bytes=9-", Partial { start: 9, end: 9 }),
            ("bytes=-1", Partial { start: 9, end: 9 }),
            ("bytes=-4", Partial { start: 6, end: 9 }),
            ("bytes=-10", Partial { start: 0, end: 9 }),
            ("bytes=-99", Partial { start: 0, end: 9 }),
            ("bytes=0002-0005", Partial { start: 2, end: 5 }),
            ("bytes=-0004", Partial { start: 6, end: 9 }),
            ("BYTES=2-5", Partial { start: 2, end: 5 }),
            (" \tbytes= \t2-5\t ", Partial { start: 2, end: 5 }),
        ] {
            assert_eq!(select_range(value, 10), expected, "{value:?}");
        }
    }

    #[test]
    fn rejects_valid_but_unsatisfiable_ranges() {
        for value in [
            "bytes=10-",
            "bytes=10-10",
            "bytes=11-99",
            "bytes=-0",
            "bytes=-00000",
            "bytes=18446744073709551615-",
            "bytes=9999999999999999999999999999999999999999-",
        ] {
            assert_eq!(select_range(value, 10), Unsatisfiable, "{value:?}");
        }
    }

    #[test]
    fn empty_sources_have_no_satisfiable_range() {
        for value in [
            "bytes=0-0",
            "bytes=0-",
            "bytes=1-9",
            "bytes=-0",
            "bytes=-1",
            "bytes=-9999999999999999999999999999999999999999",
        ] {
            assert_eq!(select_range(value, 0), Unsatisfiable, "{value:?}");
        }
    }

    #[test]
    fn ignores_malformed_unsupported_and_multipart_ranges() {
        for value in [
            "",
            "bytes",
            "bytes=",
            "bytes=-",
            "bytes=1",
            "items=0-1",
            "bytes =0-1",
            "bytes=+1-2",
            "bytes=1-+2",
            "bytes=--1",
            "bytes=1--2",
            "bytes=1-2-3",
            "bytes=1-2=3",
            "bytes=1 -2",
            "bytes=1- 2",
            "bytes=1-\t2",
            "bytes=1.0-2",
            "bytes=0x1-2",
            "bytes=١-٢",
            "bytes=1-2\n",
            "\r\nbytes=1-2",
            "bytes=\u{a0}1-2",
            "bytes=a-2",
            "bytes=1-a",
            "bytes=-a",
            "bytes=0-1,2-3",
            "bytes=0-1,",
            "bytes=,0-1",
            "bytes=2-1",
            "bytes=10-9",
            "bytes=0002-0001",
        ] {
            for length in [0, 10, u64::MAX] {
                assert_eq!(select_range(value, length), Full, "{value:?}, {length}");
            }
        }
    }

    #[test]
    fn handles_maximum_source_length() {
        let end = u64::MAX - 1;
        for (value, expected) in [
            ("bytes=0-", Partial { start: 0, end }),
            ("bytes=0-18446744073709551615", Partial { start: 0, end }),
            ("bytes=18446744073709551614-", Partial { start: end, end }),
            ("bytes=-1", Partial { start: end, end }),
            ("bytes=-18446744073709551615", Partial { start: 0, end }),
            ("bytes=18446744073709551615-", Unsatisfiable),
            (
                "bytes=18446744073709551615-18446744073709551615",
                Unsatisfiable,
            ),
            ("bytes=18446744073709551616-", Unsatisfiable),
        ] {
            assert_eq!(select_range(value, u64::MAX), expected, "{value:?}");
        }
    }

    #[test]
    fn handles_arbitrarily_large_decimal_values() {
        let huge = "9999999999999999999999999999999999999999";
        for length in [1, 10, u64::MAX] {
            let whole = Partial {
                start: 0,
                end: length - 1,
            };
            assert_eq!(select_range(&format!("bytes=0-{huge}"), length), whole);
            assert_eq!(select_range(&format!("bytes=-{huge}"), length), whole);
            assert_eq!(
                select_range(&format!("bytes={huge}-{huge}"), length),
                Unsatisfiable,
            );
            assert_eq!(select_range(&format!("bytes={huge}0-{huge}"), length), Full,);
            assert_eq!(
                select_range(&format!("bytes={huge}-{huge}0"), length),
                Unsatisfiable,
            );
            assert_eq!(
                select_range(
                    &format!("bytes={huge}-8888888888888888888888888888888888888888"),
                    length
                ),
                Full,
            );
            assert_eq!(
                select_range(
                    "bytes=0000000000000000000000000000000000000000-0000000000000000000000000000000000000000",
                    length
                ),
                Partial { start: 0, end: 0 },
            );
        }
    }
}
