use crate::direntry::FileRecord;
use chrono::{NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use prettytable::{Cell, Row, Table};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A fake-inode wrapper to offer an ext-like API on exFAT.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExInode {
    pub i_num: u64,
    pub attributes: u16,
    pub first_cluster: u32,
    pub size: u64,
    pub name: String,
    pub create_time: i64,
    pub last_mod_time: i64,
    pub last_access_time: i64,
}

/// Convert a 32-bit FAT/exFAT timestamp (date<<16 | time) to UNIX epoch seconds (UTC).
/// Assumptions:
/// - Timestamp is interpreted as a naive UTC time (exFAT stores local time; TZ info is not in this 32-bit value).
/// - 2-second granularity for seconds field; clamp 60 → 59.
fn exfat_ts_to_unix(ts: u32) -> i64 {
    let time = (ts & 0xFFFF) as u16;
    let date = (ts >> 16) as u16;

    let day = (date & 0b1_1111) as u32;
    let month = ((date >> 5) & 0b1111) as u32;
    let year = 1980 + ((date >> 9) as u32);
    let secs = ((time & 0b1_1111) as u32) * 2;
    let mins = ((time >> 5) & 0b11_1111) as u32;
    let hours = ((time >> 11) & 0b1_1111) as u32;

    // Basic sanity checks, mirror your original guard.
    if month == 0 || month > 12 || day == 0 || day > 31 || hours > 23 || mins > 59 {
        return -1; // sentinel for "invalid"
    }

    // Build a NaiveDateTime and convert to epoch seconds.
    let date = match NaiveDate::from_ymd_opt(year as i32, month, day) {
        Some(d) => d,
        None => return -1,
    };
    let ndt = date
        .and_hms_opt(hours, mins, secs.min(59))
        .unwrap_or_else(|| date.and_hms_opt(hours, mins, 59).unwrap());

    ndt.and_utc().timestamp()
}

/// Render UNIX seconds (UTC) as ISO-8601. If invalid (<0), show the raw sentinel.
fn unix_to_iso(secs: i64) -> String {
    if secs < 0 {
        return format!("{}", secs);
    }
    match NaiveDateTime::from_timestamp_opt(secs, 0) {
        Some(ndt) => ndt.and_utc().to_rfc3339_opts(SecondsFormat::Secs, true),
        None => format!("{}", secs),
    }
}

impl ExInode {
    pub fn from_record(i_num: u64, fr: &FileRecord) -> Self {
        Self {
            i_num,
            attributes: fr.attributes,
            first_cluster: fr.first_cluster,
            size: fr.size,
            name: fr.name.clone(),
            // Convert exFAT raw timestamps to UNIX seconds here:
            create_time: exfat_ts_to_unix(fr.create_time),
            last_mod_time: exfat_ts_to_unix(fr.last_mod_time),
            last_access_time: exfat_ts_to_unix(fr.last_access_time),
        }
    }

    #[inline]
    pub fn size(&self) -> u64 {
        self.size
    }
    #[inline]
    pub fn is_dir(&self) -> bool {
        (self.attributes & 0x0010) != 0
    }
    #[inline]
    pub fn is_regular_file(&self) -> bool {
        !self.is_dir()
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }

    pub fn to_string(&self) -> String {
        let mut t = Table::new();
        t.add_row(Row::new(vec![
            Cell::new("Identifier"),
            Cell::new(&format!("0x{:016x}", self.i_num)),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Attributes"),
            Cell::new(&format!("0x{:04x}", self.attributes)),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("First cluster"),
            Cell::new(&format!("{}", self.first_cluster)),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Size"),
            Cell::new(&format!("{}", self.size)),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Dir?"),
            Cell::new(&format!("{}", self.is_dir())),
        ]));
        t.add_row(Row::new(vec![Cell::new("Name"), Cell::new(&self.name)]));
        t.add_row(Row::new(vec![
            Cell::new("Created"),
            Cell::new(&unix_to_iso(self.create_time)),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Modified"),
            Cell::new(&unix_to_iso(self.last_mod_time)),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Accessed"),
            Cell::new(&unix_to_iso(self.last_access_time)),
        ]));
        t.to_string()
    }
}
