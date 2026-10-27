//! Local dates and times for `{date}`, `{time}`, `{datetime}` and `{day}`:
//! shifting them (`offset="+3d"`) and writing them out in a Unicode date
//! pattern (`format="EEEE, MMM d"`), as Raycast's placeholders take them.

/// A wall-clock moment in the user's time zone. Shifting it ignores
/// daylight-saving changes, as a calendar does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    /// 1–12
    pub month: u8,
    /// 1–31
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// From Monday, as [`LocalTime::weekday`] counts.
const DAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// A shift in time: `+3d`, `-1M +2h`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Offset(Vec<(i64, Unit)>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl Offset {
    /// `+3d`, `-2w`, `+1y -2M +3d`, `1h` (a missing sign is `+`). Units: `s`,
    /// `m` (minutes), `h`, `d`, `w`, `M` (months), `y`. `None` when it isn't one.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = Vec::new();
        for part in text.split_whitespace() {
            let (sign, rest) = match part.as_bytes().first()? {
                b'-' => (-1, &part[1..]),
                b'+' => (1, &part[1..]),
                _ => (1, part),
            };
            let digits = rest.find(|ch: char| !ch.is_ascii_digit())?;
            let amount: i64 = rest[..digits].parse().ok()?;
            let unit = match &rest[digits..] {
                "s" => Unit::Second,
                "m" => Unit::Minute,
                "h" => Unit::Hour,
                "d" => Unit::Day,
                "w" => Unit::Week,
                "M" => Unit::Month,
                "y" => Unit::Year,
                _ => return None,
            };
            parts.push((sign * amount, unit));
        }
        Some(Self(parts))
    }
}

impl LocalTime {
    /// 0 for Monday … 6 for Sunday.
    pub fn weekday(&self) -> usize {
        // 1970-01-01 was a Thursday.
        (days_from_civil(self.year, self.month, self.day) + 3).rem_euclid(7) as usize
    }

    /// The moment `offset` away: months and years keep the day of the
    /// month where it exists (Jan 31 + 1M is Feb 28 or 29).
    pub fn shifted(&self, offset: &Offset) -> Self {
        let mut time = *self;
        for &(amount, unit) in &offset.0 {
            let seconds = match unit {
                Unit::Month => {
                    time = time.plus_months(amount);
                    continue;
                }
                Unit::Year => {
                    time = time.plus_months(amount.saturating_mul(12));
                    continue;
                }
                Unit::Second => amount,
                Unit::Minute => amount.saturating_mul(60),
                Unit::Hour => amount.saturating_mul(3600),
                Unit::Day => amount.saturating_mul(86_400),
                Unit::Week => amount.saturating_mul(7 * 86_400),
            };
            time = time.plus_seconds(seconds);
        }
        time
    }

    fn plus_months(&self, months: i64) -> Self {
        let total = i64::from(self.year) * 12 + i64::from(self.month.max(1)) - 1 + months;
        let year = total.div_euclid(12) as i32;
        let month = (total.rem_euclid(12) + 1) as u8;
        Self {
            year,
            month,
            day: self.day.min(days_in_month(year, month)),
            ..*self
        }
    }

    fn plus_seconds(&self, seconds: i64) -> Self {
        let day_seconds = i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
            + seconds;
        let days =
            days_from_civil(self.year, self.month, self.day) + day_seconds.div_euclid(86_400);
        let rest = day_seconds.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: (rest / 3600) as u8,
            minute: (rest % 3600 / 60) as u8,
            second: (rest % 60) as u8,
        }
    }

    /// Writes it out in a Unicode date pattern: `yyyy` `yy` (year), `M` `MM`
    /// `MMM` `MMMM` (month), `d` `dd`, `D` (day of the year), `E`…`EEE`
    /// `EEEE` (weekday), `H` `HH` (0–23), `h` `hh` (1–12), `a` (AM/PM), `m`
    /// `mm`, `s` `ss`. Text in single quotes is written as it is (`''` is a
    /// quote); other letters and characters too.
    pub fn format(&self, pattern: &str) -> String {
        let mut out = String::with_capacity(pattern.len() + 8);
        let chars: Vec<char> = pattern.chars().collect();
        let mut ix = 0;
        while ix < chars.len() {
            let ch = chars[ix];
            if ch == '\'' {
                // `''` is a quote; otherwise text up to the closing one.
                if chars.get(ix + 1) == Some(&'\'') {
                    out.push('\'');
                    ix += 2;
                    continue;
                }
                ix += 1;
                while ix < chars.len() {
                    if chars[ix] == '\'' {
                        if chars.get(ix + 1) == Some(&'\'') {
                            out.push('\'');
                            ix += 2;
                            continue;
                        }
                        break;
                    }
                    out.push(chars[ix]);
                    ix += 1;
                }
                ix += 1;
                continue;
            }
            let run = chars[ix..].iter().take_while(|&&next| next == ch).count();
            ix += run;
            let month = MONTHS[usize::from(self.month.clamp(1, 12)) - 1];
            let hour12 = match self.hour % 12 {
                0 => 12,
                hour => hour,
            };
            match (ch, run) {
                ('y', 2) => out.push_str(&format!("{:02}", self.year.rem_euclid(100))),
                ('y', _) => out.push_str(&format!("{:0run$}", self.year)),
                ('M' | 'L', 1 | 2) => out.push_str(&format!("{:0run$}", self.month)),
                ('M' | 'L', 3) => out.push_str(&month[..3]),
                ('M' | 'L', _) => out.push_str(month),
                ('d', _) => out.push_str(&format!("{:0run$}", self.day)),
                ('D', _) => out.push_str(&format!("{:0run$}", self.day_of_year())),
                ('E', 1..=3) => out.push_str(&DAYS[self.weekday()][..3]),
                ('E', _) => out.push_str(DAYS[self.weekday()]),
                ('H', _) => out.push_str(&format!("{:0run$}", self.hour)),
                ('h', _) => out.push_str(&format!("{:0run$}", hour12)),
                ('a', _) => out.push_str(if self.hour < 12 { "AM" } else { "PM" }),
                ('m', _) => out.push_str(&format!("{:0run$}", self.minute)),
                ('s', _) => out.push_str(&format!("{:0run$}", self.second)),
                _ => (0..run).for_each(|_| out.push(ch)),
            }
        }
        out
    }

    fn day_of_year(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day) - days_from_civil(self.year, 1, 1) + 1
    }
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let (month, day) = (i64::from(month.clamp(1, 12)), i64::from(day.max(1)));
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year as i32, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> LocalTime {
        LocalTime {
            year,
            month,
            day,
            hour,
            minute,
            second: 7,
        }
    }

    #[test]
    fn knows_the_weekday() {
        assert_eq!(DAYS[at(2026, 10, 23, 0, 0).weekday()], "Friday");
        assert_eq!(DAYS[at(2000, 2, 29, 0, 0).weekday()], "Tuesday");
        assert_eq!(DAYS[at(1969, 12, 31, 0, 0).weekday()], "Wednesday");
    }

    #[test]
    fn formats_unicode_patterns() {
        let time = at(2026, 3, 5, 14, 9);
        assert_eq!(time.format("yyyy-MM-dd HH:mm:ss"), "2026-03-05 14:09:07");
        assert_eq!(time.format("EEEE, MMMM d, y"), "Thursday, March 5, 2026");
        assert_eq!(time.format("EEE d MMM yy"), "Thu 5 Mar 26");
        assert_eq!(time.format("h:mm a"), "2:09 PM");
        assert_eq!(at(2026, 1, 1, 0, 5).format("hh:mm a, D"), "12:05 AM, 1");
        assert_eq!(
            time.format("'Week of' d 'o''clock' ''"),
            "Week of 5 o'clock '"
        );
        assert_eq!(time.format("dd.MM — Q!"), "05.03 — Q!");
    }

    #[test]
    fn shifts_by_offsets() {
        let time = at(2026, 1, 31, 23, 30);
        let shift = |text: &str| time.shifted(&Offset::parse(text).unwrap());
        assert_eq!(shift("+1M"), at(2026, 2, 28, 23, 30));
        assert_eq!(shift("+1y +1M"), at(2027, 2, 28, 23, 30));
        assert_eq!(shift("+1h"), at(2026, 2, 1, 0, 30));
        assert_eq!(shift("-31d"), at(2025, 12, 31, 23, 30));
        assert_eq!(shift("2w -45m"), at(2026, 2, 14, 22, 45));
        assert_eq!(shift("-2M"), at(2025, 11, 30, 23, 30));
        assert_eq!(
            at(2024, 2, 29, 0, 0)
                .shifted(&Offset::parse("+1y").unwrap())
                .day,
            28
        );
        assert_eq!(shift(""), time);
        assert_eq!(Offset::parse("+3x"), None);
        assert_eq!(Offset::parse("+d"), None);
        assert_eq!(Offset::parse("tomorrow"), None);
    }
}
