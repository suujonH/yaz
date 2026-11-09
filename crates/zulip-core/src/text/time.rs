//! 界面上各处的时间文字。除 [`from_unix`] 和 [`local_label`] 外都是纯函数，时间由调用方换算成本地时间后传入。

use chrono::{DateTime, Datelike, FixedOffset, Local, NaiveDateTime, TimeDelta, Timelike};

/// 聊天中两条消息间隔达到该秒数（30 分钟）时插入时间分隔条。
pub const SEPARATOR_GAP_SECONDS: i64 = 1800;

const WEEKDAY_NAMES: [&str; 7] = ["日", "一", "二", "三", "四", "五", "六"];

/// 没有时区的时间文字。
const NAIVE_FORMATS: [&str; 4] = [
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%dT%H:%M",
    "%Y-%m-%d %H:%M",
];

/// 时刻。24 小时制：HH:mm；12 小时制：上午/下午h:mm。`seconds` 为 true 时带秒。
pub fn clock_time(time: NaiveDateTime, use_24h: bool, seconds: bool) -> String {
    if use_24h {
        return time
            .format(if seconds { "%H:%M:%S" } else { "%H:%M" })
            .to_string();
    }
    let period = if time.hour() < 12 { "上午" } else { "下午" };
    format!(
        "{period}{}",
        time.format(if seconds { "%-I:%M:%S" } else { "%-I:%M" })
    )
}

/// 会话列表的时间。今天：时刻；昨天：昨天+时刻；今年：MM/dd；往年：yyyy/MM/dd。
pub fn list_time(time: NaiveDateTime, now: NaiveDateTime, use_24h: bool) -> String {
    let days = (now.date() - time.date()).num_days();
    if days == 0 {
        clock_time(time, use_24h, false)
    } else if days == 1 {
        format!("昨天{}", clock_time(time, use_24h, false))
    } else if time.year() == now.year() {
        time.format("%m/%d").to_string()
    } else {
        time.format("%Y/%m/%d").to_string()
    }
}

/// 聊天中的时间分隔条。今天：时刻；昨天：昨天 时刻；7 天以内：星期X 时刻；其它：yyyy/MM/dd 时刻。
pub fn separator_time(time: NaiveDateTime, now: NaiveDateTime, use_24h: bool) -> String {
    let clock = clock_time(time, use_24h, false);
    let days = (now.date() - time.date()).num_days();
    if days <= 0 {
        clock
    } else if days == 1 {
        format!("昨天 {clock}")
    } else if days < 7 {
        let weekday = WEEKDAY_NAMES[time.weekday().num_days_from_sunday() as usize];
        format!("星期{weekday} {clock}")
    } else {
        format!("{} {clock}", time.format("%Y/%m/%d"))
    }
}

/// 点击消息后显示的精确时间。同一年：MM-dd 时刻（带秒）；否则：yyyy-MM-dd 时刻（带秒）。
pub fn precise_time(time: NaiveDateTime, now: NaiveDateTime, use_24h: bool) -> String {
    let date = time.format(if time.year() == now.year() {
        "%m-%d"
    } else {
        "%Y-%m-%d"
    });
    format!("{date} {}", clock_time(time, use_24h, true))
}

/// 文件列表的分组标题。今天：今天；今年：M月；往年：yyyy年M月。
pub fn month_group(time: NaiveDateTime, now: NaiveDateTime) -> String {
    if time.date() == now.date() {
        "今天".to_owned()
    } else if time.year() == now.year() {
        format!("{}月", time.month())
    } else {
        format!("{}年{}月", time.year(), time.month())
    }
}

/// 距离提醒时间的倒计时：还有X天X小时X分钟 / 已过X天X小时X分钟（只精确到分钟，省略为 0 的单位）；
/// 相差不到 1 分钟时为“还有不到 1 分钟”/“刚刚到时间”。
pub fn countdown(target: NaiveDateTime, now: NaiveDateTime) -> String {
    let delta: TimeDelta = target - now;
    let future = delta > TimeDelta::zero();
    let minutes = delta.num_minutes().abs();
    if minutes == 0 {
        return if future {
            "还有不到 1 分钟"
        } else {
            "刚刚到时间"
        }
        .to_owned();
    }
    let mut text = if future { "还有" } else { "已过" }.to_owned();
    for (value, unit) in [
        (minutes / (24 * 60), "天"),
        (minutes / 60 % 24, "小时"),
        (minutes % 60, "分钟"),
    ] {
        if value > 0 {
            text.push_str(&format!("{value}{unit}"));
        }
    }
    text
}

/// 两条相邻消息（Unix 秒）之间是否需要时间分隔条。
pub fn needs_separator(previous_timestamp: i64, current_timestamp: i64) -> bool {
    current_timestamp - previous_timestamp >= SEPARATOR_GAP_SECONDS
}

/// Unix 秒转为本地时间；超出可表示的范围时为 `None`。
pub fn from_unix(seconds: i64) -> Option<NaiveDateTime> {
    DateTime::from_timestamp(seconds, 0).map(|t| t.with_timezone(&Local).naive_local())
}

/// 解析全局时间 `<time:…>` 与 `datetime` 属性：RFC 3339，或没有时区的 ISO 形式（按 UTC）。
pub fn parse_datetime(text: &str) -> Option<DateTime<FixedOffset>> {
    let text = text.trim();
    DateTime::parse_from_rfc3339(text).ok().or_else(|| {
        NAIVE_FORMATS
            .iter()
            .find_map(|format| NaiveDateTime::parse_from_str(text, format).ok())
            .map(|t| t.and_utc().fixed_offset())
    })
}

/// 全局时间显示的文字：本地时间 yyyy/MM/dd HH:mm。
pub fn local_label(time: &DateTime<FixedOffset>) -> String {
    time.with_timezone(&Local)
        .format("%Y/%m/%d %H:%M")
        .to_string()
}

/// 本地日期 yyyy/MM/dd；时间超出可表示的范围时为空。
pub fn date_label(seconds: i64) -> String {
    from_unix(seconds).map_or_else(String::new, |t| t.format("%Y/%m/%d").to_string())
}

/// 本地时间 yyyy/MM/dd HH:mm；时间超出可表示的范围时为空。
pub fn datetime_label(seconds: i64) -> String {
    from_unix(seconds).map_or_else(String::new, |t| t.format("%Y/%m/%d %H:%M").to_string())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn t(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
            .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M"))
            .unwrap()
    }

    // 2026-10-22 是星期四
    fn now() -> NaiveDateTime {
        t("2026-10-22 15:30:00")
    }

    #[test]
    fn labels_use_local_time() {
        let seconds = Local
            .with_ymd_and_hms(2026, 3, 4, 5, 6, 7)
            .earliest()
            .unwrap()
            .timestamp();
        assert_eq!(date_label(seconds), "2026/03/04");
        assert_eq!(datetime_label(seconds), "2026/03/04 05:06");
        assert_eq!(date_label(i64::MAX), "");
        assert_eq!(datetime_label(i64::MAX), "");
    }

    #[test]
    fn list() {
        for (time, expected) in [
            ("2026-10-22 09:05", "09:05"),
            ("2026-10-21 23:59", "昨天23:59"),
            ("2026-01-02 08:00", "01/02"),
            ("2025-12-31 08:00", "2025/12/31"),
        ] {
            assert_eq!(list_time(t(time), now(), true), expected);
        }
    }

    #[test]
    fn separator() {
        for (time, expected) in [
            ("2026-10-22 09:05", "09:05"),
            ("2026-10-21 23:59", "昨天 23:59"),
            ("2026-10-20 08:00", "星期二 08:00"),
            ("2026-10-18 08:00", "星期日 08:00"),
            ("2026-10-16 08:00", "星期五 08:00"),
            ("2026-10-15 08:00", "2026/10/15 08:00"),
        ] {
            assert_eq!(separator_time(t(time), now(), true), expected);
        }
    }

    #[test]
    fn precise() {
        for (time, expected) in [
            ("2026-03-04 05:06:07", "03-04 05:06:07"),
            ("2025-03-04 05:06:07", "2025-03-04 05:06:07"),
        ] {
            assert_eq!(precise_time(t(time), now(), true), expected);
        }
    }

    #[test]
    fn clock_12_hour() {
        for (time, expected) in [
            ("2026-10-22 00:05", "上午12:05"),
            ("2026-10-22 09:05", "上午9:05"),
            ("2026-10-22 12:00", "下午12:00"),
            ("2026-10-22 23:59", "下午11:59"),
        ] {
            assert_eq!(clock_time(t(time), false, false), expected);
        }
    }

    #[test]
    fn twelve_hour_variants() {
        assert_eq!(
            list_time(t("2026-10-21 23:59"), now(), false),
            "昨天下午11:59"
        );
        assert_eq!(
            separator_time(t("2026-10-20 08:00"), now(), false),
            "星期二 上午8:00"
        );
        assert_eq!(
            separator_time(t("2026-10-15 14:00"), now(), false),
            "2026/10/15 下午2:00"
        );
        assert_eq!(
            precise_time(t("2026-03-04 05:06:07"), now(), false),
            "03-04 上午5:06:07"
        );
    }

    #[test]
    fn month() {
        for (time, expected) in [
            ("2026-10-22 00:01", "今天"),
            ("2026-10-21 23:59", "10月"),
            ("2026-06-01 08:00", "6月"),
            ("2025-12-31 08:00", "2025年12月"),
        ] {
            assert_eq!(month_group(t(time), now()), expected);
        }
    }

    #[test]
    fn countdown_text() {
        for (target, expected) in [
            ("2026-10-22 15:30:30", "还有不到 1 分钟"),
            ("2026-10-22 15:35:59", "还有5分钟"),
            ("2026-10-22 17:30:00", "还有2小时"),
            ("2026-10-22 17:45:00", "还有2小时15分钟"),
            ("2026-10-24 15:31:00", "还有2天1分钟"),
            ("2026-10-25 18:00:00", "还有3天2小时30分钟"),
            ("2026-10-22 15:29:40", "刚刚到时间"),
            ("2026-10-22 15:20:00", "已过10分钟"),
            ("2026-10-22 14:00:00", "已过1小时30分钟"),
            ("2026-10-21 15:30:00", "已过1天"),
        ] {
            assert_eq!(countdown(t(target), now()), expected);
        }
    }

    #[test]
    fn separator_at_30_minutes() {
        assert!(!needs_separator(1000, 1000 + 1799));
        assert!(needs_separator(1000, 1000 + 1800));
    }

    #[test]
    fn parses_datetime_forms() {
        let expected = parse_datetime("2026-10-24T07:00:00Z").unwrap();
        for text in [
            "2026-10-24T15:00:00+08:00",
            "2026-10-24T07:00:00",
            "2026-10-24 07:00",
        ] {
            assert_eq!(parse_datetime(text), Some(expected), "{text}");
        }
        assert_eq!(parse_datetime("abc"), None);
    }
}
