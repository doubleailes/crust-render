//! Logging: the `-l` filter, the `--stats` report's exemption from it, and
//! the `--log-file` sink.

use std::path::Path;
use std::time::SystemTime;
use tracing::Level;

use crate::cli::LoggerLevel;

/// Target the `--stats` report is emitted under.
///
/// It exists so the report can be exempted from `-l`: `--stats` is an
/// explicit request for the report, and honouring it only at `-l info` or
/// below would mean `--stats -l warn` silently produced nothing. The filter
/// in `main` admits this target at any level and applies `-l` to everything
/// else, which is what keeps the report a log event — reaching `--log-file`
/// like any other — without letting the log level decide whether it appears.
pub(super) const STATS_TARGET: &str = "crust_render::stats";

/// Whether an event at `level` on `target` survives a `-l max` filter.
///
/// Named rather than inlined into the closure so it can be tested: the whole
/// point of it is the one case that is easy to regress into silence —
/// [`STATS_TARGET`] passing at a level that rejects everything else.
pub(super) fn event_enabled(target: &str, level: &Level, max: Level) -> bool {
    target == STATS_TARGET || effective_level(target, level) <= max
}

/// The level an event is filtered at, which for a few dependencies is not
/// the level it was emitted at.
///
/// `cranelift_jit` logs the whole IR of every function it defines at INFO —
/// one multi-hundred-line dump per MaterialX program, so a default render's
/// INFO output grew with the number of materials, against the rule that INFO
/// lines do not scale with the scene. `tracing` cannot rewrite an event's
/// level, so it is *filtered* as DEBUG (shown from `-l debug` on) while still
/// printing its own `INFO` stamp. WARN and ERROR from cranelift are untouched.
fn effective_level(target: &str, level: &Level) -> Level {
    if *level == Level::INFO && target.starts_with("cranelift") {
        Level::DEBUG
    } else {
        *level
    }
}

pub(super) fn get_logger_level(level: LoggerLevel) -> Level {
    match level {
        LoggerLevel::Debug => Level::DEBUG,
        LoggerLevel::Info => Level::INFO,
        LoggerLevel::Warn => Level::WARN,
        LoggerLevel::Error => Level::ERROR,
        LoggerLevel::Trace => Level::TRACE,
    }
}

/// A filename-safe UTC timestamp, `YYYYMMDDTHHMMSSZ`.
///
/// Hand-rolled rather than pulled from `chrono` or `time`: neither is in the
/// dependency graph, and adding one to name a file would be the largest
/// dependency in this binary. `tracing-subscriber` formats its own line
/// timestamps the same way and for the same reason, so the `Z` suffix here
/// matches what the log lines themselves carry.
///
/// The civil-from-days conversion is Howard Hinnant's, shifting the era to
/// start on 0000-03-01 so a leap day lands at the end of a 400-year cycle and
/// the month arithmetic needs no table. Valid for any date this can be handed.
fn utc_stamp(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        // A clock before 1970 is not worth a failure path; it only names a file.
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Days since 1970-01-01 -> civil date.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    // `mp` counts from March; roll it back to a calendar month, and with it
    // the year, which only advances once January is reached.
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = era * 400 + yoe + i64::from(month <= 2);

    format!("{year:04}{month:02}{day:02}T{hour:02}{min:02}{sec:02}Z")
}

/// Opens the run's log file, creating any missing directories in `dir`.
///
/// A failure fails the run rather than warning: nothing has been rendered yet
/// when this runs, so stopping costs no work, and a `--log-file` that quietly
/// produced no file would be discovered only after the render it was meant to
/// record. The error is the message to print.
pub(super) fn open_log_file(dir: &Path) -> std::result::Result<std::fs::File, String> {
    let path = dir.join(format!("crust-render-{}.log", utc_stamp(SystemTime::now())));
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return Err(format!(
            "could not create log directory {}: {e}",
            parent.display()
        ));
    }
    let f = std::fs::File::create(&path)
        .map_err(|e| format!("could not create log file {}: {e}", path.display()))?;
    // Said on stderr rather than through `tracing`: the subscriber this file
    // belongs to does not exist yet.
    eprintln!("Logging to {}", path.display());
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SystemTime` at a given Unix second, for pinning `utc_stamp` against
    /// dates whose answers are known independently.
    fn at(unix_secs: u64) -> SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix_secs)
    }

    #[test]
    fn utc_stamp_names_known_instants() {
        assert_eq!(utc_stamp(at(0)), "19700101T000000Z");
        assert_eq!(utc_stamp(at(1_774_267_884)), "20260323T121124Z");
        // Last second of a year, and the first of the next.
        assert_eq!(utc_stamp(at(1_767_225_599)), "20251231T235959Z");
        assert_eq!(utc_stamp(at(1_767_225_600)), "20260101T000000Z");
    }

    #[test]
    fn utc_stamp_handles_leap_years() {
        // 2024 is a leap year: Feb 29 exists.
        assert_eq!(utc_stamp(at(1_709_164_800)), "20240229T000000Z");
        // 2000 is a leap year (divisible by 400) — the case a naive
        // "divisible by 4, except by 100" rule gets wrong.
        assert_eq!(utc_stamp(at(951_782_400)), "20000229T000000Z");
        // 1900 was NOT a leap year, but it predates the epoch, so check the
        // other end of the same rule: 2100 is not one either, and March 1
        // must follow February 28.
        assert_eq!(utc_stamp(at(4_107_456_000)), "21000228T000000Z");
        assert_eq!(utc_stamp(at(4_107_542_400)), "21000301T000000Z");
    }

    #[test]
    fn utc_stamp_is_filename_safe_and_sorts_chronologically() {
        let mut prev = utc_stamp(at(0));
        for day in 1..4000u64 {
            // Every 37 days, so the walk crosses month and year boundaries
            // at varied offsets rather than landing on the same day each time.
            let t = utc_stamp(at(day * 37 * 86_400 + 3661));
            assert!(
                t.chars().all(|c| c.is_ascii_alphanumeric()),
                "{t} is not filename-safe"
            );
            assert_eq!(t.len(), 16, "{t} is not a fixed-width stamp");
            // Fixed width and zero-padded, so lexical order is chronological
            // — which is the whole reason for this format over a locale one.
            assert!(t > prev, "{t} does not sort after {prev}");
            prev = t;
        }
    }

    #[test]
    fn the_stats_report_survives_every_log_level() {
        // `--stats` is an explicit request, so no `-l` may suppress it —
        // including the quietest, which is the regression this guards.
        for max in [
            Level::ERROR,
            Level::WARN,
            Level::INFO,
            Level::DEBUG,
            Level::TRACE,
        ] {
            assert!(
                event_enabled(STATS_TARGET, &Level::INFO, max),
                "the stats report was filtered out at -l {max}"
            );
        }
    }

    #[test]
    fn every_other_target_still_obeys_the_level() {
        // The exemption is for one target, not a hole in the filter.
        assert!(!event_enabled("crust_render", &Level::INFO, Level::ERROR));
        assert!(!event_enabled(
            "crust_core::scene::usd_import",
            &Level::DEBUG,
            Level::INFO
        ));
        assert!(event_enabled("crust_render", &Level::ERROR, Level::ERROR));
        assert!(event_enabled(
            "crust_core::tracer",
            &Level::DEBUG,
            Level::DEBUG
        ));
        assert!(event_enabled("crust_assets", &Level::WARN, Level::INFO));
        // A near-miss on the target name is not the stats target.
        assert!(!event_enabled("stats", &Level::INFO, Level::ERROR));
        // Cranelift's INFO IR dumps are filtered as DEBUG, and only those.
        let jit = "cranelift_jit::backend";
        assert!(!event_enabled(jit, &Level::INFO, Level::INFO));
        assert!(event_enabled(jit, &Level::INFO, Level::DEBUG));
        assert!(event_enabled(jit, &Level::WARN, Level::INFO));
        assert!(event_enabled("crust_render", &Level::INFO, Level::INFO));
        assert!(!event_enabled(
            "crust_render::stats_extra",
            &Level::INFO,
            Level::ERROR
        ));
    }

    #[test]
    fn log_levels_map_one_to_one() {
        assert_eq!(get_logger_level(LoggerLevel::Trace), Level::TRACE);
        assert_eq!(get_logger_level(LoggerLevel::Debug), Level::DEBUG);
        assert_eq!(get_logger_level(LoggerLevel::Info), Level::INFO);
        assert_eq!(get_logger_level(LoggerLevel::Warn), Level::WARN);
        assert_eq!(get_logger_level(LoggerLevel::Error), Level::ERROR);
    }
}
