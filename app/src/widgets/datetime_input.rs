//! Date and time typed by the user: text for filtering by date, and the dialog for picking a future
//! time.

use crate::ui::*;
use chrono::{Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use dioxus::prelude::*;
use zulip_core::text::time::from_unix;

/// Format of the value of a `datetime-local` control.
const CONTROL_FORMAT: &str = "%Y-%m-%dT%H:%M";

/// The numbers appearing in the text in order; all other characters are treated as separators:
///
/// a Chinese-style date such as `2026<year>11<month>5<day> 18:30` becomes `[2026, 11, 5, 18, 30]`.
fn numbers(text: &str) -> Vec<u32> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

/// Year-month-day, such as `2026-11-05`, `2026/1/5`, or a Chinese-style date with year/month/day
/// characters.
pub fn parse_date(text: &str) -> Option<NaiveDate> {
    let n = numbers(text);
    if n.len() != 3 {
        return None;
    }
    NaiveDate::from_ymd_opt(i32::try_from(n[0]).ok()?, n[1], n[2])
}

/// Year-month-day plus hour-minute (seconds optional), such as `2026-11-05 18:30`.
pub fn parse_datetime(text: &str) -> Option<NaiveDateTime> {
    let n = numbers(text);
    if !(5..=6).contains(&n.len()) {
        return None;
    }
    NaiveDate::from_ymd_opt(i32::try_from(n[0]).ok()?, n[1], n[2])?.and_hms_opt(
        n[3],
        n[4],
        n.get(5).copied().unwrap_or(0),
    )
}

/// Unix seconds for a local time.
pub fn local_timestamp(time: NaiveDateTime) -> Option<i64> {
    Some(Local.from_local_datetime(&time).earliest()?.timestamp())
}

/// ISO 8601 text for the end of this day (local 23:59:59), used as the cutoff time for chat-history
/// filtering.
pub fn end_of_day(date: NaiveDate) -> Option<String> {
    let time = Local
        .from_local_datetime(&date.and_hms_opt(23, 59, 59)?)
        .earliest()?;
    Some(time.format("%Y-%m-%dT%H:%M:%S%:z").to_string())
}

// When `initial` is empty, default to the next whole hour
fn control_value(initial: Option<i64>) -> String {
    match initial.and_then(from_unix) {
        Some(time) => time.format(CONTROL_FORMAT).to_string(),
        None => {
            let next = Local::now() + Duration::hours(1);
            next.with_minute(0)
                .unwrap_or(next)
                .format(CONTROL_FORMAT)
                .to_string()
        }
    }
}

/// Pick a future time (Unix seconds); must be later than now; `None` on cancel. `initial` is the
/// initial value; when empty it defaults to the next whole hour.
pub async fn ask_future_time(
    overlays: Overlays,
    title: &'static str,
    initial: Option<i64>,
) -> Option<i64> {
    overlays
        .ask_dialog(move |answer| rsx! { FutureTimeDialog { title, initial, answer } })
        .await
}

#[component]
fn FutureTimeDialog(title: &'static str, initial: Option<i64>, answer: Answer<i64>) -> Element {
    let mut text = use_signal(|| control_value(initial));
    let at = parse_datetime(&text.read()).and_then(local_timestamp);
    let future = at.is_some_and(|at| at > Local::now().timestamp());
    let close = answer.clone();
    let cancel = answer.clone();
    rsx! {
        DialogFrame {
            title,
            width: 320.0,
            on_close: move |_| close.cancel(),
            footer: rsx! {
                DialogButtons {
                    disabled: !future,
                    on_confirm: move |_| {
                        if let Some(at) = at {
                            answer.send(at);
                        }
                    },
                    on_cancel: move |_| cancel.cancel(),
                }
            },
            div { class: "dialog-form",
                div { class: "input",
                    input {
                        r#type: "datetime-local",
                        value: "{text}",
                        oninput: move |e| text.set(e.value()),
                    }
                }
                if !future {
                    div { class: "dialog-error", "请选择将来的时间" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_accept_common_separators() {
        let expected = NaiveDate::from_ymd_opt(2026, 11, 5);
        assert_eq!(parse_date("2026-11-05"), expected);
        assert_eq!(parse_date(" 2026/11/5 "), expected);
        assert_eq!(parse_date("2026年11月5日"), expected);
    }

    #[test]
    fn invalid_dates_are_rejected() {
        assert_eq!(parse_date("abc"), None);
        assert_eq!(parse_date("2026-13-01"), None);
        assert_eq!(parse_date("2026-11"), None);
        assert_eq!(parse_date("2026-11-05 18:30"), None);
    }

    #[test]
    fn datetimes_have_optional_seconds() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        assert_eq!(
            parse_datetime("2026-11-05 18:30"),
            day.and_hms_opt(18, 30, 0)
        );
        assert_eq!(
            parse_datetime("2026-11-05T18:30:15"),
            day.and_hms_opt(18, 30, 15)
        );
        assert_eq!(parse_datetime("2026-11-05"), None);
        assert_eq!(parse_datetime("2026-11-05 25:00"), None);
    }

    #[test]
    fn end_of_day_is_the_last_second_with_offset() {
        let text = end_of_day(NaiveDate::from_ymd_opt(2026, 11, 5).unwrap()).unwrap();
        assert!(text.starts_with("2026-11-05T23:59:59"));
        assert!(text.len() > "2026-11-05T23:59:59".len());
    }

    #[test]
    fn control_value_round_trips_to_the_minute() {
        let time = parse_datetime("2026-11-05 18:30").unwrap();
        let timestamp = local_timestamp(time).unwrap();
        let value = control_value(Some(timestamp));
        assert_eq!(value, "2026-11-05T18:30");
        assert_eq!(parse_datetime(&value), Some(time));
    }

    #[test]
    fn control_value_defaults_to_the_next_whole_hour() {
        let value = parse_datetime(&control_value(None)).unwrap();
        assert_eq!(value.minute(), 0);
        assert!(local_timestamp(value).unwrap() > Local::now().timestamp() - 3600);
    }
}
