//! Turning Unix time into calendar dates and clock times, and reading and
//! writing time zone offsets.

use alloc::format;
use alloc::string::String;

/// A calendar date and time of day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
	pub year: i64,
	pub month: u8,
	pub day: u8,
	pub hour: u8,
	pub minute: u8,
	pub second: u8,
}

impl DateTime {
	/// The date and time `offset_minutes` east of UTC at `unix_seconds`.
	pub fn from_unix(unix_seconds: i64, offset_minutes: i16) -> Self {
		let local = unix_seconds + i64::from(offset_minutes) * 60;
		let (days, seconds) = (local.div_euclid(86_400), local.rem_euclid(86_400));
		let (year, month, day) = civil_from_days(days);
		DateTime {
			year,
			month,
			day,
			hour: (seconds / 3600) as u8,
			minute: (seconds / 60 % 60) as u8,
			second: (seconds % 60) as u8,
		}
	}

	pub fn clock(&self) -> String {
		format!("{:02}:{:02}", self.hour, self.minute)
	}

	pub fn date_and_clock(&self) -> String {
		format!(
			"{}-{:02}-{:02} {:02}:{:02}",
			self.year, self.month, self.day, self.hour, self.minute
		)
	}
}

/// The proleptic Gregorian date `days` after 1970-01-01, by Howard
/// Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i64, u8, u8) {
	let days = days + 719_468;
	let era = days.div_euclid(146_097);
	let day_of_era = days.rem_euclid(146_097);
	let year_of_era =
		(day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
	let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
	let month_index = (5 * day_of_year + 2) / 153;
	let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u8;
	let month = if month_index < 10 {
		month_index + 3
	} else {
		month_index - 9
	} as u8;
	let year = year_of_era + era * 400 + i64::from(month <= 2);
	(year, month, day)
}

/// Reads an offset from UTC like `+2`, `-05:00` or `+0530`, in minutes.
pub fn parse_offset(text: &str) -> Option<i16> {
	let text = text.trim();
	let (sign, rest) = match text.as_bytes().first()? {
		b'+' => (1, &text[1..]),
		b'-' => (-1, &text[1..]),
		_ => (1, text),
	};
	let digits: String = rest.chars().filter(|&character| character != ':').collect();
	if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
		return None;
	}
	let (hours, minutes) = if digits.len() <= 2 {
		(digits.parse::<i16>().ok()?, 0)
	} else {
		let split = digits.len() - 2;
		(
			digits[..split].parse::<i16>().ok()?,
			digits[split..].parse::<i16>().ok()?,
		)
	};
	if hours > 14 || minutes >= 60 {
		return None;
	}
	Some(sign * (hours * 60 + minutes))
}

pub fn format_offset(minutes: i16) -> String {
	let sign = if minutes < 0 { '-' } else { '+' };
	let minutes = minutes.unsigned_abs();
	format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}


/// How long something has been up, to the nearest unit worth showing.
pub fn short_uptime(seconds: u64) -> String {
	match seconds {
		0..60 => format!("{seconds} s"),
		60..3600 => format!("{} min", seconds / 60),
		3600..86_400 => format!("{} h {} min", seconds / 3600, seconds / 60 % 60),
		_ => format!("{} d {} h", seconds / 86_400, seconds / 3600 % 24),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn converts_unix_time_to_dates() {
		assert_eq!(
			DateTime::from_unix(0, 0).date_and_clock(),
			"1970-01-01 00:00"
		);
		assert_eq!(
			DateTime::from_unix(951_782_400, 0).date_and_clock(),
			"2000-02-29 00:00"
		);
		assert_eq!(
			DateTime::from_unix(1_794_182_400, 0).date_and_clock(),
			"2026-11-09 00:00"
		);
		// Two hours west of UTC is still the previous day.
		assert_eq!(
			DateTime::from_unix(1_794_182_400, -120).date_and_clock(),
			"2026-11-08 22:00"
		);
	}

	#[test]
	fn reads_and_writes_offsets() {
		assert_eq!(parse_offset("+2"), Some(120));
		assert_eq!(parse_offset("-05:00"), Some(-300));
		assert_eq!(parse_offset("+0530"), Some(330));
		assert_eq!(parse_offset("5:45"), Some(345));
		assert_eq!(parse_offset("+15"), None);
		assert_eq!(parse_offset("+2:60"), None);
		assert_eq!(parse_offset("east"), None);
		assert_eq!(format_offset(-300), "-05:00");
		assert_eq!(format_offset(330), "+05:30");
	}
}
