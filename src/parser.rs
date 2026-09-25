use chrono::{FixedOffset, NaiveDate, NaiveDateTime, TimeZone};
use thiserror::Error;

use crate::function_codes;
use crate::model::PunchEvent;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("empty line")]
    Empty,
    #[error("invalid date/time segment: {0}")]
    InvalidTimestamp(String),
    #[error("missing bracketed node info: {0}")]
    MissingNodeInfo(String),
    #[error("invalid node info `{0}`")]
    InvalidNodeInfo(String),
    #[error("invalid uid segment: {0}")]
    InvalidUid(String),
}

pub fn parse_text_line(line: &str, tz_offset_seconds: i32) -> Result<PunchEvent, ParseError> {
    let raw = line.trim_end_matches(['\r', '\n']);
    if raw.trim().is_empty() {
        return Err(ParseError::Empty);
    }

    let bytes = raw.as_bytes();
    if bytes.len() < 17 {
        return Err(ParseError::InvalidTimestamp(raw.to_string()));
    }
    let digit = |b: u8| b.is_ascii_digit();
    if !(digit(bytes[0])
        && digit(bytes[1])
        && bytes[2] == b'\''
        && digit(bytes[3])
        && digit(bytes[4])
        && bytes[5] == b'/'
        && digit(bytes[6])
        && digit(bytes[7])
        && bytes[8] == b' '
        && digit(bytes[9])
        && digit(bytes[10])
        && bytes[11] == b':'
        && digit(bytes[12])
        && digit(bytes[13])
        && bytes[14] == b':'
        && digit(bytes[15])
        && digit(bytes[16]))
    {
        return Err(ParseError::InvalidTimestamp(raw.to_string()));
    }

    let yy = (bytes[0] - b'0') as i32 * 10 + (bytes[1] - b'0') as i32;
    let mm = (bytes[3] - b'0') as u32 * 10 + (bytes[4] - b'0') as u32;
    let dd = (bytes[6] - b'0') as u32 * 10 + (bytes[7] - b'0') as u32;
    let hh = (bytes[9] - b'0') as u32 * 10 + (bytes[10] - b'0') as u32;
    let mins = (bytes[12] - b'0') as u32 * 10 + (bytes[13] - b'0') as u32;
    let ss = (bytes[15] - b'0') as u32 * 10 + (bytes[16] - b'0') as u32;

    let naive_date =
        NaiveDate::from_ymd_opt(2000 + yy, mm, dd).ok_or(ParseError::InvalidTimestamp(
            raw.to_string(),
        ))?;
    let naive_time =
        chrono::NaiveTime::from_hms_opt(hh, mins, ss).ok_or(ParseError::InvalidTimestamp(
            raw.to_string(),
        ))?;
    let naive = NaiveDateTime::new(naive_date, naive_time);
    let offset = FixedOffset::east_opt(tz_offset_seconds)
        .ok_or_else(|| ParseError::InvalidTimestamp(raw.to_string()))?;
    let occurred_at = offset
        .from_local_datetime(&naive)
        .earliest()
        .unwrap_or_else(|| {
            let utc = chrono::Utc.from_utc_datetime(&naive);
            utc.with_timezone(&offset)
        });

    let after_bracket = find_bracket(raw).ok_or(ParseError::MissingNodeInfo(raw.to_string()))?;
    let (node_id, sub_code, function_code) = parse_node_info(&after_bracket.0)?;

    let rest = &raw[after_bracket.1..];
    let (door_no, rest) = take_paren(rest)?;
    let (uid_hex, uid_decimal, rest) = take_uid(rest)?;

    let marker = find_event_marker(rest);
    let (alias_raw, description) = match marker {
        Some((alias_end, _, _, desc_start)) => {
            let alias_raw = rest[..alias_end].trim().to_string();
            let desc = rest[desc_start..].trim().to_string();
            (alias_raw, desc)
        }
        None => (rest.trim().to_string(), "".to_string()),
    };

    let username = alias_raw.trim().to_string();

    let description = if description.is_empty() {
        function_codes::lookup(function_code)
            .map(|i| i.en.to_string())
            .unwrap_or_else(|| "Unknown".to_string())
    } else {
        description
    };

    Ok(PunchEvent {
        node_id,
        sub_code,
        function_code,
        event_code: function_codes::event_code(function_code),
        description,
        door_no,
        uid_hex,
        uid_decimal,
        username_raw: alias_raw,
        username,
        occurred_at,
        punch_type: "unknown".to_string(),
        duty_code: None,
        duty_label: None,
        raw: raw.to_string(),
    })
}

fn find_bracket(line: &str) -> Option<(String, usize)> {
    let start = line.find('[')?;
    let end = line[start + 1..].find(']')? + start + 1;
    Some((line[start + 1..end].to_string(), end + 1))
}

fn parse_node_info(inner: &str) -> Result<(u32, u32, u32), ParseError> {
    let mut parts = inner.split(':');
    let left = parts.next().ok_or(ParseError::InvalidNodeInfo(inner.to_string()))?;
    let right = parts
        .next()
        .ok_or(ParseError::InvalidNodeInfo(inner.to_string()))?
        .trim();
    let func = u32::from_str_radix(right, 16)
        .map_err(|_| ParseError::InvalidNodeInfo(inner.to_string()))?;
    let mut side = left.split('.');
    let node: u32 = side
        .next()
        .unwrap_or_default()
        .parse()
        .map_err(|_| ParseError::InvalidNodeInfo(inner.to_string()))?;
    let sub: u32 = side
        .next()
        .unwrap_or_default()
        .parse()
        .map_err(|_| ParseError::InvalidNodeInfo(inner.to_string()))?;
    Ok((node, sub, func))
}

fn take_paren(s: &str) -> Result<(Option<u32>, &str), ParseError> {
    if let Some(stripped) = s.strip_prefix('(') {
        if let Some(idx) = stripped.find(')') {
            let val: u32 = stripped[..idx]
                .trim()
                .parse()
                .map_err(|_| ParseError::InvalidUid(s.to_string()))?;
            return Ok((Some(val), &stripped[idx + 1..]));
        }
    }
    Ok((None, s))
}

fn take_uid(s: &str) -> Result<(String, Option<u64>, &str), ParseError> {
    let end = s.find(' ').unwrap_or(s.len());
    let seg = &s[..end];
    let rest = &s[end..];
    let hex = seg.trim();
    let decimal = if hex.len() <= 16 && !hex.is_empty() {
        u64::from_str_radix(hex, 16).ok()
    } else {
        None
    };
    Ok((hex.to_string(), decimal, rest))
}

fn find_event_marker(s: &str) -> Option<(usize, usize, usize, usize)> {
    let marker = s.rfind(" (M").or_else(|| s.rfind("(M"))?;
    let digits_start = if s.as_bytes()[marker] == b' ' {
        marker + 3
    } else {
        marker + 2
    };
    let bytes = s.as_bytes();
    let mut idx = digits_start;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx == digits_start || idx >= bytes.len() || bytes[idx] != b')' {
        return None;
    }
    let desc_start = idx + 1;
    Some((marker, digits_start, idx, desc_start))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str =
        "21'05/12 13:38:54 [001.17:0B](0)00000000D4B81403 rSammi                (M11)Normal Access\n";

    #[test]
    fn parses_official_sample() {
        let ev = parse_text_line(SAMPLE, 28800).unwrap();
        assert_eq!(ev.node_id, 1);
        assert_eq!(ev.sub_code, 17);
        assert_eq!(ev.function_code, 11);
        assert_eq!(ev.event_code, "M11");
        assert_eq!(ev.door_no, Some(0));
        assert_eq!(ev.uid_hex, "00000000D4B81403");
        assert_eq!(ev.uid_decimal, Some(0x0000_0000_D4B8_1403));
        assert_eq!(ev.username, "rSammi");
        assert_eq!(ev.description, "Normal Access");
        assert_eq!(ev.occurred_at.format("%Y-%m-%d %H:%M:%S").to_string(), "2021-05-12 13:38:54");
    }

    #[test]
    fn parses_crlf_and_padded_fields() {
        let line = "22'01/03 08:05:01 [010.18:1C](1)12345678ABCDEF00 王小明       (M28)Access by PIN\r\n";
        let ev = parse_text_line(line, 28800).unwrap();
        assert_eq!(ev.node_id, 10);
        assert_eq!(ev.sub_code, 18);
        assert_eq!(ev.function_code, 28);
        assert_eq!(ev.event_code, "M28");
        assert_eq!(ev.uid_decimal, Some(0x12345678ABCDEF00));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_text_line("", 28800).is_err());
        assert!(parse_text_line("not a record\n", 28800).is_err());
    }
}