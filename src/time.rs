//! Court time. Tuvalu (Pacific/Funafuti) is UTC+12 all year, no DST, so a fixed offset is exact and
//! avoids shipping a timezone database. Instants are stored as UTC `YYYY-MM-DDTHH:MM:SSZ`;
//! court calendar dates as `YYYY-MM-DD`. Browser timezone never affects court dates.

use crate::error::{AppError, AppResult};
use time::macros::format_description;
use time::{Date, Duration, OffsetDateTime, PrimitiveDateTime, UtcOffset};

pub const COURT_TZ_NAME: &str = "Pacific/Funafuti";
pub const COURT_OFFSET_HOURS: i8 = 12;

fn court_offset() -> UtcOffset {
    UtcOffset::from_hms(COURT_OFFSET_HOURS, 0, 0).expect("valid offset")
}

const UTC_FMT: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
const LOCAL_FMT: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]");
const DATE_FMT: &[time::format_description::FormatItem<'static>] = format_description!("[year]-[month]-[day]");

pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

pub fn fmt_utc(t: OffsetDateTime) -> String {
    t.to_offset(UtcOffset::UTC).format(UTC_FMT).expect("format utc")
}

/// Current instant, UTC text.
pub fn now_utc() -> String {
    fmt_utc(now())
}

/// Instant `hours` from now, UTC text.
pub fn utc_in_hours(hours: i64) -> String {
    fmt_utc(now() + Duration::hours(hours))
}

pub fn parse_utc(s: &str) -> AppResult<OffsetDateTime> {
    PrimitiveDateTime::parse(s, UTC_FMT)
        .map(|p| p.assume_utc())
        .map_err(|_| AppError::validation(format!("Invalid UTC timestamp '{s}'")))
}

/// Today's date at the court.
pub fn today_local() -> String {
    now().to_offset(court_offset()).date().format(DATE_FMT).expect("format date")
}

/// Validate a `YYYY-MM-DD` calendar date and return it normalised.
pub fn parse_date(s: &str) -> AppResult<String> {
    Date::parse(s.trim(), DATE_FMT)
        .map(|d| d.format(DATE_FMT).expect("format date"))
        .map_err(|_| AppError::validation(format!("Invalid date '{s}', expected YYYY-MM-DD")))
}

/// Optional date field helper: empty → None.
pub fn parse_opt_date(s: Option<&str>) -> AppResult<Option<String>> {
    match s.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) => parse_date(v).map(Some),
    }
}

/// Year of a `YYYY-MM-DD` date.
pub fn year_of(date: &str) -> AppResult<i32> {
    Date::parse(date, DATE_FMT)
        .map(|d| d.year())
        .map_err(|_| AppError::validation(format!("Invalid date '{date}'")))
}

/// Court-local `YYYY-MM-DDTHH:MM` → UTC text.
pub fn local_to_utc(local: &str) -> AppResult<String> {
    let p = PrimitiveDateTime::parse(local.trim(), LOCAL_FMT)
        .map_err(|_| AppError::validation(format!("Invalid local time '{local}', expected YYYY-MM-DDTHH:MM")))?;
    Ok(fmt_utc(p.assume_offset(court_offset())))
}

/// UTC text → court-local `YYYY-MM-DDTHH:MM`.
pub fn utc_to_local(utc: &str) -> String {
    match parse_utc(utc) {
        Ok(t) => t.to_offset(court_offset()).format(LOCAL_FMT).expect("format local"),
        Err(_) => utc.to_string(),
    }
}

/// Court wall-clock text in English, for correspondence or a short next-action date.
pub fn human_court_local(local: &str, short_date: bool) -> String {
    let Ok(t) = PrimitiveDateTime::parse(local, LOCAL_FMT) else {
        return local.to_string();
    };
    let weekday = t.weekday().to_string();
    let month = t.month().to_string();
    if short_date {
        format!("{} {} {} {}", &weekday[..3], t.day(), &month[..3], t.year())
    } else {
        format!("{weekday} {} {month} {} at {:02}:{:02}", t.day(), t.year(), t.hour(), t.minute())
    }
}

/// UTC text → court-local date `YYYY-MM-DD`.
pub fn utc_to_local_date(utc: &str) -> String {
    match parse_utc(utc) {
        Ok(t) => t.to_offset(court_offset()).date().format(DATE_FMT).expect("format date"),
        Err(_) => utc.get(..10).unwrap_or(utc).to_string(),
    }
}

/// Add minutes to a UTC text instant.
pub fn add_minutes(utc: &str, minutes: i64) -> AppResult<String> {
    Ok(fmt_utc(parse_utc(utc)? + Duration::minutes(minutes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_roundtrip_is_utc_plus_12() {
        let utc = local_to_utc("2026-11-17T09:00").unwrap();
        assert_eq!(utc, "2026-11-16T21:00:00Z");
        assert_eq!(utc_to_local(&utc), "2026-11-17T09:00");
        assert_eq!(utc_to_local_date(&utc), "2026-11-17");
    }

    #[test]
    fn rejects_bad_dates() {
        assert!(parse_date("2026-02-30").is_err());
        assert!(local_to_utc("17/11/2026 09:00").is_err());
    }
}
