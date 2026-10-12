//! Parser for the subset of moment.js date formats Obsidian uses for daily
//! note file names (e.g. `YYYY-MM-DD`, `YYYY/MM/DD`, `dddd, MMMM Do YYYY`).

use anyhow::{Result, bail};
use chrono::NaiveDate;

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Literal(String),
    Year4,
    Year2,
    MonthName,
    MonthShort,
    Month2,
    Month,
    DayOrdinal,
    Day2,
    Day,
    /// Weekday names (`dddd`, `ddd`, `dd`); validated loosely.
    Weekday,
}

#[derive(Debug, Clone)]
pub struct DateFormat {
    tokens: Vec<Token>,
}

impl DateFormat {
    pub fn parse(format: &str) -> Result<Self> {
        let chars: Vec<char> = format.chars().collect();
        let mut tokens = Vec::new();
        let mut i = 0;
        let run = |i: usize, c: char| chars[i..].iter().take_while(|&&x| x == c).count();
        while i < chars.len() {
            let c = chars[i];
            match c {
                '[' => {
                    let end = chars[i + 1..]
                        .iter()
                        .position(|&x| x == ']')
                        .map(|p| i + 1 + p);
                    let Some(end) = end else {
                        bail!("unterminated [literal] in date format {format:?}");
                    };
                    push_literal(&mut tokens, chars[i + 1..end].iter().collect());
                    i = end + 1;
                }
                'Y' => {
                    let n = run(i, 'Y');
                    tokens.push(match n {
                        4 => Token::Year4,
                        2 => Token::Year2,
                        _ => bail!("unsupported year token in date format {format:?}"),
                    });
                    i += n;
                }
                'M' => {
                    let n = run(i, 'M');
                    tokens.push(match n {
                        1 => Token::Month,
                        2 => Token::Month2,
                        3 => Token::MonthShort,
                        4 => Token::MonthName,
                        _ => bail!("unsupported month token in date format {format:?}"),
                    });
                    i += n;
                }
                'D' => {
                    let n = run(i, 'D');
                    if n == 1 && chars.get(i + 1) == Some(&'o') {
                        tokens.push(Token::DayOrdinal);
                        i += 2;
                        continue;
                    }
                    tokens.push(match n {
                        1 => Token::Day,
                        2 => Token::Day2,
                        _ => {
                            bail!("day-of-year tokens are not supported in date format {format:?}")
                        }
                    });
                    i += n;
                }
                'd' => {
                    let n = run(i, 'd');
                    if n < 2 {
                        bail!("numeric weekday tokens are not supported in date format {format:?}");
                    }
                    tokens.push(Token::Weekday);
                    i += n;
                }
                'Q' | 'W' | 'w' | 'G' | 'g' | 'X' | 'x' | 'H' | 'h' | 'm' | 's' | 'A' | 'a' => {
                    bail!("token {c:?} is not supported in daily note format {format:?}")
                }
                _ => {
                    push_literal(&mut tokens, c.to_string());
                    i += 1;
                }
            }
        }
        let has = |t: &[Token]| tokens.iter().any(|x| t.contains(x));
        if !has(&[Token::Year4, Token::Year2])
            || !has(&[
                Token::Month,
                Token::Month2,
                Token::MonthShort,
                Token::MonthName,
            ])
            || !has(&[Token::Day, Token::Day2, Token::DayOrdinal])
        {
            bail!("daily note format {format:?} must contain a year, month and day");
        }
        Ok(DateFormat { tokens })
    }

    pub fn contains_path_separator(&self) -> bool {
        self.tokens
            .iter()
            .any(|t| matches!(t, Token::Literal(s) if s.contains('/')))
    }

    /// Parses `input` in full; returns `None` if it doesn't match the format.
    pub fn parse_date(&self, input: &str) -> Option<NaiveDate> {
        let mut rest = input;
        let (mut year, mut month, mut day) = (None, None, None);
        for token in &self.tokens {
            match token {
                Token::Literal(lit) => {
                    rest = strip_prefix_ci(rest, lit)?;
                }
                Token::Year4 => {
                    let (n, r) = take_digits(rest, 4, 4)?;
                    year = Some(n as i32);
                    rest = r;
                }
                Token::Year2 => {
                    let (n, r) = take_digits(rest, 2, 2)?;
                    year = Some(2000 + n as i32);
                    rest = r;
                }
                Token::Month2 => {
                    let (n, r) = take_digits(rest, 2, 2)?;
                    month = Some(n);
                    rest = r;
                }
                Token::Month => {
                    let (n, r) = take_digits(rest, 1, 2)?;
                    month = Some(n);
                    rest = r;
                }
                Token::MonthName | Token::MonthShort => {
                    let (n, r) = take_month_name(rest, matches!(token, Token::MonthShort))?;
                    month = Some(n);
                    rest = r;
                }
                Token::Day2 => {
                    let (n, r) = take_digits(rest, 2, 2)?;
                    day = Some(n);
                    rest = r;
                }
                Token::Day => {
                    let (n, r) = take_digits(rest, 1, 2)?;
                    day = Some(n);
                    rest = r;
                }
                Token::DayOrdinal => {
                    let (n, r) = take_digits(rest, 1, 2)?;
                    let r = ["st", "nd", "rd", "th"]
                        .iter()
                        .find_map(|s| strip_prefix_ci(r, s))?;
                    day = Some(n);
                    rest = r;
                }
                Token::Weekday => {
                    let len = rest
                        .char_indices()
                        .find(|(_, c)| !c.is_alphabetic())
                        .map(|(i, _)| i)
                        .unwrap_or(rest.len());
                    if len < 2 {
                        return None;
                    }
                    rest = &rest[len..];
                }
            }
        }
        if !rest.is_empty() {
            return None;
        }
        NaiveDate::from_ymd_opt(year?, month?, day?)
    }
}

fn push_literal(tokens: &mut Vec<Token>, s: String) {
    if let Some(Token::Literal(prev)) = tokens.last_mut() {
        prev.push_str(&s);
    } else {
        tokens.push(Token::Literal(s));
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let n = prefix.len();
    if s.len() >= n && s.is_char_boundary(n) && s[..n].eq_ignore_ascii_case(prefix) {
        Some(&s[n..])
    } else {
        None
    }
}

fn take_digits(s: &str, min: usize, max: usize) -> Option<(u32, &str)> {
    let len = s.bytes().take(max).take_while(u8::is_ascii_digit).count();
    if len < min {
        return None;
    }
    Some((s[..len].parse().ok()?, &s[len..]))
}

fn take_month_name(s: &str, short: bool) -> Option<(u32, &str)> {
    for (i, name) in MONTHS.iter().enumerate() {
        let candidate = if short { &name[..3] } else { name };
        if let Some(rest) = strip_prefix_ci(s, candidate) {
            return Some((i as u32 + 1, rest));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(y, m, day)
    }

    #[test]
    fn iso_format() {
        let f = DateFormat::parse("YYYY-MM-DD").unwrap();
        assert_eq!(f.parse_date("2024-01-31"), d(2024, 1, 31));
        assert_eq!(f.parse_date("2024-1-31"), None);
        assert_eq!(f.parse_date("2024-02-30"), None);
        assert_eq!(f.parse_date("2024-01-31 notes"), None);
    }

    #[test]
    fn named_format() {
        let f = DateFormat::parse("dddd, MMMM Do YYYY").unwrap();
        assert_eq!(f.parse_date("Monday, January 1st 2024"), d(2024, 1, 1));
        let f = DateFormat::parse("[Journal] YYYY.MM.DD").unwrap();
        assert_eq!(f.parse_date("Journal 2023.12.05"), d(2023, 12, 5));
    }

    #[test]
    fn path_format() {
        let f = DateFormat::parse("YYYY/MM/YYYY-MM-DD").unwrap();
        assert!(f.contains_path_separator());
        assert_eq!(f.parse_date("2024/03/2024-03-09"), d(2024, 3, 9));
    }

    #[test]
    fn rejects_unsupported() {
        assert!(DateFormat::parse("gggg-[W]ww").is_err());
        assert!(DateFormat::parse("MM-DD").is_err());
    }
}
