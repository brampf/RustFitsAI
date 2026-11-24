// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

/// A complete, owned FITS file.
///
/// This model corresponds to the ordered primary HDU, conforming extensions,
/// and optional special records described in FITS 4.0 Sections 3.1, 3.4, and
/// 3.5. It owns all HDU data and is therefore intentionally not used by the
/// streaming CLI for large files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FitsFile {
    /// The primary HDU followed by extensions in their on-disk order.
    pub hdus: Vec<Hdu>,
    /// Complete 2880-byte special-record blocks after the final HDU.
    pub special_records: Vec<Vec<u8>>,
}

/// An owned Header and Data Unit as defined by FITS 4.0 Section 3.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hdu {
    /// Header cards describing this HDU.
    pub header: Header,
    /// Unpadded Data Unit bytes; FITS block padding is not included.
    pub data: Vec<u8>,
}

/// Header-only index returned before a Data Unit is consumed.
///
/// This is the memory-efficient representation used by [`crate::FitsReader`]. The
/// `data_size` value is derived from the FITS size keywords specified in
/// Section 4.4.1; the Data Unit remains in the input stream until requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HduIndex {
    /// Parsed header and original header bytes for this HDU.
    pub header: Header,
    /// Number of meaningful Data Unit bytes, excluding FITS block padding.
    pub data_size: usize,
}

/// The random-groups layout described by FITS 4.0 Section 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RandomGroupsDescriptor {
    /// Number of parameter values preceding each group array (`PCOUNT`).
    pub parameter_count: usize,
    /// Number of groups in the Data Unit (`GCOUNT`).
    pub group_count: usize,
    /// Number of array elements following each group's parameters.
    pub elements_per_group: usize,
    /// Binary representation shared by parameters and array elements.
    pub representation: DataRepresentation,
}

/// The binary representation selected by `BITPIX` in FITS 5.2 and 5.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataRepresentation {
    /// Unsigned byte data; callers may interpret values below 128 as ASCII.
    Byte,
    /// Big-endian two's-complement 16-bit integer.
    Integer16,
    /// Big-endian two's-complement 32-bit integer.
    Integer32,
    /// Big-endian two's-complement 64-bit integer.
    Integer64,
    /// Big-endian IEEE-754 single-precision value.
    Float32,
    /// Big-endian IEEE-754 double-precision value.
    Float64,
}

impl DataRepresentation {
    /// Maps a validated FITS `BITPIX` value to its binary data representation.
    pub fn from_bitpix(bitpix: &str) -> Option<Self> {
        match bitpix.parse::<i16>().ok()? {
            8 => Some(Self::Byte),
            16 => Some(Self::Integer16),
            32 => Some(Self::Integer32),
            64 => Some(Self::Integer64),
            -32 => Some(Self::Float32),
            -64 => Some(Self::Float64),
            _ => None,
        }
    }
}

/// One decoded scalar from a FITS Data Unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DataValue {
    /// An unsigned 8-bit value; ASCII interpretation is caller-controlled.
    Byte(u8),
    /// A signed big-endian 16-bit value decoded from the Data Unit.
    Integer16(i16),
    /// A signed big-endian 32-bit value decoded from the Data Unit.
    Integer32(i32),
    /// A signed big-endian 64-bit value decoded from the Data Unit.
    Integer64(i64),
    /// An IEEE-754 single-precision value decoded from the Data Unit.
    Float32(f32),
    /// An IEEE-754 double-precision value decoded from the Data Unit.
    Float64(f64),
}

impl HduIndex {
    /// Returns the scalar representation implied by this HDU's `BITPIX`.
    pub fn data_representation(&self) -> Option<DataRepresentation> {
        DataRepresentation::from_bitpix(self.header.value("BITPIX")?)
    }

    /// Returns the random-groups descriptor when this is a random-groups HDU.
    pub fn random_groups(&self) -> Option<RandomGroupsDescriptor> {
        if self.header.value("GROUPS") != Some("T") {
            return None;
        }
        let axes = self.header.value("NAXIS")?.parse::<usize>().ok()?;
        let elements_per_group = (2..=axes)
            .map(|axis| self.header.value(&format!("NAXIS{axis}"))?.parse().ok())
            .collect::<Option<Vec<usize>>>()?
            .into_iter()
            .product();
        Some(RandomGroupsDescriptor {
            parameter_count: self.header.value("PCOUNT")?.parse().ok()?,
            group_count: self.header.value("GCOUNT")?.parse().ok()?,
            elements_per_group,
            representation: self.data_representation()?,
        })
    }
}

impl Hdu {
    /// Returns the extension type from `XTENSION`, if this HDU is an extension.
    ///
    /// FITS 4.0 Section 3.4.1 requires every conforming extension to identify
    /// its type with this keyword; the primary HDU has no `XTENSION` card.
    pub fn extension_type(&self) -> Option<&str> {
        self.header
            .value("XTENSION")
            .map(|value| value.trim_matches('\'').trim())
            .filter(|value| !value.is_empty())
    }
}

/// The header portion of an HDU.
///
/// FITS 4.0 Sections 3.2 and 3.3.1 define headers as one or more 2880-byte
/// blocks containing 80-character keyword records and an `END` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Parsed keyword records, excluding the `END` marker and blank cards.
    pub cards: Vec<Card>,
    /// Exact printable 80-character cards through and including `END`.
    /// This convenience representation is retained for verbose output.
    pub raw_cards: Vec<String>,
    /// Exact header bytes, including header padding.
    pub raw_bytes: Vec<u8>,
    /// Header length in bytes, including padding to a complete FITS block.
    pub header_size: usize,
}

impl Header {
    /// Finds the first parsed card with the requested FITS keyword.
    ///
    /// Keyword/value syntax follows FITS 4.0 Section 4.4.1.
    pub fn value(&self, keyword: &str) -> Option<&str> {
        self.cards
            .iter()
            .find(|card| card.keyword == keyword)
            .and_then(|card| card.value.as_ref())
            .and_then(Value::as_str)
    }

    /// Reconstructs a single-record or `CONTINUE`-extended string value.
    ///
    /// A trailing `&` marks a continuation only when the next physical card
    /// is a `CONTINUE` string record (FITS 4.0 Section 4.2.1.2).
    pub fn string_value(&self, keyword: &str) -> Option<String> {
        let mut cards = self.cards.iter();
        let first = cards.find(|card| card.keyword == keyword)?;
        let Some(Value::String(first_part)) = &first.value else {
            return None;
        };
        let mut part = first_part.as_str();
        let mut value = String::new();
        // Parsed cards omit blank records, which interrupt a continuation.
        let mut raw_continuations = self
            .raw_cards_from(keyword)
            .skip(1)
            .take_while(|record| record.starts_with("CONTINUE  "));
        for card in cards.take_while(|card| card.keyword == "CONTINUE") {
            let Some(prefix) = part.trim_end_matches(' ').strip_suffix('&') else {
                break;
            };
            let Some(Value::String(next)) = &card.value else {
                break;
            };
            if !self.raw_cards.is_empty() && raw_continuations.next().is_none() {
                break;
            }
            value.push_str(prefix);
            part = next;
        }
        value.push_str(part);
        Some(value)
    }

    pub(super) fn raw_cards_from<'a>(&'a self, keyword: &str) -> impl Iterator<Item = &'a str> {
        self.raw_cards
            .iter()
            .map(String::as_str)
            .skip_while(move |card| card.get(..8).map(str::trim_end) != Some(keyword))
    }
}

/// A parsed FITS value from FITS 4.0 Sections 4.2 and 4.2.2-4.2.6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A quoted character string, with doubled single quotes decoded.
    String(String),
    /// An upper-case `T` or `F` logical constant.
    Logical(String),
    /// A signed decimal integer, retained lexically to avoid range loss.
    Integer(String),
    /// A decimal floating-point value with optional upper-case `E`/`D` exponent.
    Real(String),
    /// A parenthesized pair of integer values.
    ComplexInteger(String),
    /// A parenthesized pair of real floating-point values.
    ComplexReal(String),
    /// A present value indicator with an entirely blank value field.
    Undefined,
}

impl Value {
    /// Returns the decoded string or lexical representation of the value.
    /// Undefined values return `None`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value)
            | Self::Logical(value)
            | Self::Integer(value)
            | Self::Real(value)
            | Self::ComplexInteger(value)
            | Self::ComplexReal(value) => Some(value),
            Self::Undefined => None,
        }
    }

    /// Interprets a string value using the same rules as a FITS date keyword.
    /// See [`DateTime::parse`] for the accepted forms.
    pub fn as_datetime(&self) -> Option<DateTime<'_>> {
        let value = match self {
            Self::String(value) => value,
            _ => return None,
        };
        DateTime::parse(value)
    }

    /// Interprets a string value as a restricted-ASCII FITS unit string.
    /// Units are metadata associated with a value, not a separate FITS value
    /// datatype; see FITS 4.0 Sections 4.3 and 4.3.1.
    pub fn as_unit(&self) -> Option<UnitString<'_>> {
        let value = match self {
            Self::String(value) => value,
            _ => return None,
        };
        UnitString::parse(value)
    }
}

/// A borrowed FITS calendar date with an optional time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime<'a>(&'a str);

impl<'a> DateTime<'a> {
    /// Reads `YYYY-MM-DD` with an optional `Thh:mm:ss` and fractional seconds.
    /// FITS 4.0 section 9.1.1 also allows a signed five-digit year. The older
    /// `DD/MM/YY` form covers 1900 through 1999.
    ///
    /// FITS timestamps omit the time zone. For compatibility, the reader also
    /// accepts `Z` and numeric offsets (`+hh:mm` or `-hh:mm`), as well as
    /// lowercase `t` and `z`. Second 60 is allowed in the last minute of a UTC day.
    pub fn parse(value: &'a str) -> Option<Self> {
        let text = value.trim_end_matches(' ');
        let (year, month, day) = if let Some((day, rest)) = text.split_once('/') {
            let (month, year) = rest.split_once('/')?;
            (1900 + decimal_field(year, 2)?, month, day)
        } else {
            let date = if let Some((date, time)) = text.split_once(['T', 't']) {
                validate_clock(time)?;
                date
            } else {
                text
            };
            let (year_month, day) = date.rsplit_once('-')?;
            let (year, month) = year_month.rsplit_once('-')?;
            let year = if year.len() == 6 && year.starts_with(['+', '-']) {
                year.parse().ok()?
            } else {
                decimal_field(year, 4)?
            };
            (year, month, day)
        };
        let last_day = match decimal_field(month, 2)? {
            2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            1..=12 => 31,
            _ => return None,
        };
        (1..=last_day)
            .contains(&decimal_field(day, 2)?)
            .then_some(Self(value))
    }

    /// Returns the original string, including any trailing FITS spaces.
    pub fn as_str(self) -> &'a str {
        self.0
    }
}

fn decimal_field(field: &str, width: usize) -> Option<i32> {
    if field.len() != width || !field.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}

fn clock_minutes(value: &str) -> Option<i32> {
    let (hour, minute) = value.split_once(':')?;
    let hour = decimal_field(hour, 2)?;
    let minute = decimal_field(minute, 2)?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

fn validate_clock(time: &str) -> Option<()> {
    let (clock, offset_minutes) = if let Some((clock, offset)) = time.split_once('+') {
        (clock, clock_minutes(offset)?)
    } else if let Some((clock, offset)) = time.split_once('-') {
        (clock, -clock_minutes(offset)?)
    } else {
        (time.strip_suffix(['Z', 'z']).unwrap_or(time), 0)
    };
    let (hours_minutes, seconds) = clock.rsplit_once(':')?;
    let local_minute = clock_minutes(hours_minutes)?;
    let seconds = if let Some((seconds, fraction)) = seconds.split_once('.') {
        if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        seconds
    } else {
        seconds
    };
    let second = decimal_field(seconds, 2)?;
    let utc_minute = (local_minute - offset_minutes).rem_euclid(24 * 60);
    (second < 60 || (second == 60 && utc_minute == 23 * 60 + 59)).then_some(())
}

/// A restricted-ASCII unit string as described by FITS 4.0 Section 4.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitString<'a>(&'a str);

impl<'a> UnitString<'a> {
    /// Validates and borrows a restricted-ASCII unit string.
    pub fn parse(value: &'a str) -> Option<Self> {
        (!value.is_empty() && value.bytes().all(|byte| (32..=126).contains(&byte)))
            .then_some(Self(value))
    }

    pub fn as_str(self) -> &'a str {
        self.0
    }
}

/// One parsed 80-character FITS keyword record.
///
/// The value/comment split follows the keyword-record conventions in FITS 4.0
/// Section 4.4.1; commentary cards such as `COMMENT` and `HISTORY` have no
/// value field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub keyword: String,
    pub value: Option<Value>,
    pub comment: Option<String>,
}
