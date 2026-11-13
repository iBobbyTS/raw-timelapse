//! Capture-time continuity: the solver's time-gap boundaries.
//!
//! Only capture-time information contributes boundaries. Filename digit jumps
//! are not evidence of a capture gap and are not invented into the plan.

/// Zero-based frame indices that start a new continuity segment.
///
/// The median positive capture interval is the nominal interval; a gap needs at
/// least three samples and is either non-positive or longer than 1.5 nominal
/// intervals. Missing capture times degrade coverage to filename-only, which
/// adds no boundaries.
pub fn time_gaps(capture_times: &[Option<String>]) -> Vec<usize> {
    let times: Vec<Option<f64>> = capture_times
        .iter()
        .map(|time| time.as_deref().and_then(parse_capture_time))
        .collect();
    let mut deltas: Vec<Option<f64>> = Vec::with_capacity(times.len().saturating_sub(1));
    for index in 1..times.len() {
        deltas.push(match (times[index], times[index - 1]) {
            (Some(current), Some(previous)) => Some(current - previous),
            _ => None,
        });
    }
    let mut positive: Vec<f64> = deltas
        .iter()
        .flatten()
        .copied()
        .filter(|delta| *delta > 0.0)
        .collect();
    positive.sort_by(f64::total_cmp);
    if positive.is_empty() || positive.len() < 3 {
        return vec![];
    }
    let nominal = positive[positive.len() / 2];
    let mut gaps = vec![];
    for (offset, delta) in deltas.iter().enumerate() {
        let index = offset + 1;
        let is_gap = match delta {
            Some(value) => *value <= 0.0 || *value > 1.5 * nominal,
            None => true,
        };
        if is_gap {
            gaps.push(index);
        }
    }
    gaps
}

fn parse_capture_time(text: &str) -> Option<f64> {
    let text = text.trim();
    let (date, time) = if let Some((date, time)) = text.split_once(' ') {
        (date, time)
    } else if let Some((date, time)) = text.split_once('T') {
        (date, time)
    } else {
        return None;
    };
    let date_parts: Vec<&str> = date
        .split(|ch| ch == ':' || ch == '-' || ch == '/')
        .collect();
    if date_parts.len() != 3 {
        return None;
    }
    let year: i64 = date_parts[0].parse().ok()?;
    let month: i64 = date_parts[1].parse().ok()?;
    let day: i64 = date_parts[2].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let time = time.trim_end_matches('Z');
    let (clock, fraction) = match time.split_once(['.', ',']) {
        Some((clock, fraction)) => (clock, fraction),
        None => (time, ""),
    };
    let time_parts: Vec<&str> = clock.split(':').collect();
    if time_parts.len() < 2 || time_parts.len() > 3 {
        return None;
    }
    let hour: f64 = time_parts[0].parse().ok()?;
    let minute: f64 = time_parts[1].parse().ok()?;
    let second: f64 = if time_parts.len() == 3 {
        time_parts[2].parse().ok()?
    } else {
        0.0
    };
    if !(0.0..60.0).contains(&minute) || !(0.0..61.0).contains(&second) {
        return None;
    }
    let mut seconds = second;
    if !fraction.is_empty() {
        let mut scale = 0.1;
        for ch in fraction.chars() {
            let digit = ch.to_digit(10)? as f64;
            seconds += digit * scale;
            scale *= 0.1;
        }
    }
    let days = days_from_civil(year, month, day);
    Some(days as f64 * 86400.0 + hour * 3600.0 + minute * 60.0 + seconds)
}

/// Howard Hinnant's days_from_civil (proleptic Gregorian, no timezone).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month_index = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * month_index + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn times(values: &[&str]) -> Vec<Option<String>> {
        values.iter().map(|value| Some((*value).to_string())).collect()
    }

    #[test]
    fn capture_time_parses_exif_and_iso() {
        let exif = parse_capture_time("2026:11:08 20:09:53").expect("exif");
        let iso = parse_capture_time("2026-11-08T20:09:53").expect("iso");
        assert_eq!(exif, iso);
        assert!(parse_capture_time("not a time").is_none());
        let fractional = parse_capture_time("2026:11:08 20:09:53.5").expect("fraction");
        assert!((fractional - exif - 0.5).abs() < 1e-9);
    }

    #[test]
    fn repeated_interval_adds_no_boundary() {
        let gaps = time_gaps(&times(&[
            "2026:11:08 20:00:00",
            "2026:11:08 20:00:10",
            "2026:11:08 20:00:20",
            "2026:11:08 20:00:30",
        ]));
        assert!(gaps.is_empty(), "{gaps:?}");
    }

    #[test]
    fn long_gap_and_missing_time_start_segments() {
        let gaps = time_gaps(&times(&[
            "2026:11:08 20:00:00",
            "2026:11:08 20:00:10",
            "2026:11:08 20:00:20",
            "2026:11:08 20:00:30",
            "2026:11:08 20:01:30",
            "2026:11:08 20:01:40",
        ]));
        assert_eq!(gaps, vec![4]);

        let mut with_missing = times(&[
            "2026:11:08 20:00:00",
            "2026:11:08 20:00:10",
            "2026:11:08 20:00:20",
            "2026:11:08 20:00:30",
            "2026:11:08 20:00:40",
            "2026:11:08 20:00:50",
        ]);
        with_missing[3] = None;
        assert_eq!(time_gaps(&with_missing), vec![3, 4]);
    }

    #[test]
    fn filename_only_degrades_to_no_boundary() {
        assert!(time_gaps(&[None, None, None, None]).is_empty());
        assert!(time_gaps(&[]).is_empty());
    }
}
