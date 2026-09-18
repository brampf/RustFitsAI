// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

use std::io::{self, BufRead, BufReader, Cursor, Read, Write};

use super::{Card, FitsError, FitsFile, Hdu, HduIndex, Header, Value};

/// Logical FITS block size from FITS 4.0 Sections 3.1-3.3.
pub const BLOCK_SIZE: usize = 2880;
/// FITS header keyword-record size from FITS 4.0 Section 3.3.1.
pub const CARD_SIZE: usize = 80;
const IO_BUFFER_SIZE: usize = 64 * 1024;

/// Sequential, buffered FITS reader.
///
/// `FitsReader` keeps only a bounded I/O buffer and the current Header Unit in
/// memory. Callers must consume or skip the current Data Unit before requesting
/// the next HDU. This matches the ordered structure required by FITS 4.0
/// Sections 3.1 and 3.2 and avoids allocating very large Data Units.
pub struct FitsReader<R> {
    reader: BufReader<R>,
    first_hdu: bool,
    pending_data: Option<PendingData>,
    at_special_records: bool,
    lax_checksums: bool,
}

struct PendingData {
    size: usize,
    padded_size: usize,
    datasum: Option<u32>,
    checksum_header: Option<Vec<u8>>,
}

impl<R: Read> FitsReader<R> {
    /// Creates a reader with a bounded internal buffer over any `Read` source.
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::with_capacity(IO_BUFFER_SIZE, reader),
            first_hdu: true,
            pending_data: None,
            at_special_records: false,
            lax_checksums: false,
        }
    }

    /// Creates a reader with optional DATASUM/CHECKSUM validation disabled.
    pub fn with_lax(reader: R, lax_checksums: bool) -> Self {
        let mut result = Self::new(reader);
        result.lax_checksums = lax_checksums;
        result
    }

    /// Parses the next primary or conforming extension header.
    ///
    /// The returned [`HduIndex`] is an index into the still-streamed Data Unit.
    /// `None` indicates that the reader reached the special-record section or
    /// logical end of the FITS stream. See FITS 4.0 Sections 3.4 and 3.5.
    pub fn next_hdu(&mut self) -> Result<Option<HduIndex>, FitsError> {
        if self.pending_data.is_some() {
            return Err(FitsError::DataPending);
        }
        if self.at_special_records {
            return Ok(None);
        }

        if !self.first_hdu {
            let prefix = self.reader.fill_buf().map_err(io_error)?;
            if prefix.is_empty() || prefix.len() < 8 || &prefix[..8] != b"XTENSION" {
                self.at_special_records = true;
                return Ok(None);
            }
        }

        let header = parse_header(&mut self.reader)?;
        validate_header(&header, self.first_hdu)?;
        let data_size = data_size(&header, self.first_hdu)?;
        let padded_size = padded_size(data_size)?;
        let datasum = header
            .value("DATASUM")
            .and_then(|value| value.trim().parse().ok());
        let checksum_header = card_value(&header, "CHECKSUM")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(|_| header.raw_bytes.clone());
        self.pending_data = Some(PendingData {
            size: data_size,
            padded_size,
            datasum,
            checksum_header,
        });
        self.first_hdu = false;

        Ok(Some(HduIndex { header, data_size }))
    }

    /// Streams the current Data Unit to `writer` and discards its block padding.
    ///
    /// This is the preferred method for very large Data Units. FITS padding is
    /// defined by the 2880-byte blocking rules in Sections 3.1, 3.2, and 3.6.
    pub fn read_data_to<W: Write>(&mut self, mut writer: W) -> Result<(), FitsError> {
        let pending = self.pending_data.take().ok_or(FitsError::NoPendingData)?;
        let padding = pending.padded_size - pending.size;
        if self.lax_checksums {
            copy_exact(&mut self.reader, &mut writer, pending.size)?;
            copy_exact(&mut self.reader, &mut io::sink(), padding)?;
        } else {
            let mut checksum = OnesComplement::default();
            copy_exact_checksum(&mut self.reader, &mut writer, pending.size, &mut checksum)?;
            copy_exact_checksum(&mut self.reader, &mut io::sink(), padding, &mut checksum)?;
            let data_sum = checksum.finish();
            validate_data_checksum(pending.datasum, data_sum)?;
            validate_hdu_checksum(pending.checksum_header.as_deref(), data_sum)?;
        }
        Ok(())
    }

    /// Reads the current Data Unit into memory on explicit request.
    ///
    /// Prefer [`FitsReader::read_data_to`] or [`FitsReader::skip_data`] when the
    /// Data Unit may be large.
    pub fn read_data(&mut self) -> Result<Vec<u8>, FitsError> {
        let size = self
            .pending_data
            .as_ref()
            .ok_or(FitsError::NoPendingData)?
            .size;
        let mut data = Vec::new();
        data.try_reserve_exact(size)
            .map_err(|_| FitsError::SizeOverflow {
                keyword: "data allocation".to_string(),
            })?;
        self.read_data_to(&mut data)?;
        Ok(data)
    }

    /// Consumes the current Data Unit without retaining its bytes.
    pub fn skip_data(&mut self) -> Result<(), FitsError> {
        self.read_data_to(io::sink())
    }

    /// Collects trailing special records into memory.
    ///
    /// Special records are restricted-use 2880-byte blocks as described in
    /// FITS 4.0 Section 3.5. Use [`FitsReader::copy_special_records_to`] for a
    /// streaming alternative.
    pub fn read_special_records(&mut self) -> Result<Vec<Vec<u8>>, FitsError> {
        let mut bytes = Vec::new();
        self.copy_special_records_to(&mut bytes)?;
        Ok(bytes.chunks_exact(BLOCK_SIZE).map(Vec::from).collect())
    }

    /// Streams trailing special records to `writer` and returns their count.
    ///
    /// The first eight bytes of each record are checked against the `XTENSION`
    /// restriction in FITS 4.0 Section 3.5. A partial physical tail is ignored
    /// as permitted by the physical-blocking rules in Section 3.6.1.
    pub fn copy_special_records_to<W: Write>(&mut self, mut writer: W) -> Result<usize, FitsError> {
        if !self.at_special_records {
            return Err(FitsError::SpecialRecordsNotReady);
        }

        let mut record = [0_u8; BLOCK_SIZE];
        let mut count = 0;
        loop {
            let mut filled = 0;
            while filled < BLOCK_SIZE {
                let bytes_read = self.reader.read(&mut record[filled..]).map_err(io_error)?;
                if bytes_read == 0 {
                    return Ok(count);
                }
                filled += bytes_read;
            }
            if &record[..8] == b"XTENSION" {
                return Err(FitsError::InvalidSpecialRecords {
                    offset: count * BLOCK_SIZE,
                    message: "the first eight bytes must not contain XTENSION".to_string(),
                });
            }
            writer.write_all(&record).map_err(io_error)?;
            count += 1;
        }
    }
}

impl FitsFile {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FitsError> {
        if bytes.is_empty() {
            return Err(FitsError::EmptyFile);
        }

        let mut reader = FitsReader::new(Cursor::new(bytes));
        let mut hdus = Vec::new();
        while let Some(index) = reader.next_hdu()? {
            let data = reader.read_data()?;
            hdus.push(Hdu {
                header: index.header,
                data,
            });
        }
        let special_records = reader.read_special_records()?;
        Ok(Self {
            hdus,
            special_records,
        })
    }
}

fn parse_header<R: Read>(reader: &mut R) -> Result<Header, FitsError> {
    let mut cards = Vec::new();
    let mut raw_cards = Vec::new();
    let mut raw_bytes = Vec::new();
    let mut header_size = 0;
    let mut block = [0_u8; BLOCK_SIZE];

    loop {
        reader.read_exact(&mut block).map_err(io_error)?;
        raw_bytes.extend_from_slice(&block);
        header_size += BLOCK_SIZE;
        let mut found_end = false;
        for card_bytes in block.chunks_exact(CARD_SIZE) {
            if card_bytes.iter().any(|byte| !(32..=126).contains(byte)) {
                return Err(FitsError::InvalidHeader {
                    offset: header_size - BLOCK_SIZE,
                    message: "header contains a non-ASCII printable character".to_string(),
                });
            }
            raw_cards.push(String::from_utf8_lossy(card_bytes).into_owned());
            let keyword = keyword_name(&card_bytes[..8])?;
            if keyword == "END" {
                if card_bytes[8..].iter().any(|byte| *byte != b' ') {
                    return Err(FitsError::InvalidEndRecord);
                }
                found_end = true;
                break;
            }
            if keyword.is_empty() {
                continue;
            }
            cards.push(parse_card(card_bytes, keyword)?);
        }
        if found_end {
            return Ok(Header {
                cards,
                raw_cards,
                raw_bytes,
                header_size,
            });
        }
    }
}

fn copy_exact_checksum<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    size: usize,
    checksum: &mut OnesComplement,
) -> Result<(), FitsError> {
    let mut remaining = size;
    let mut buffer = [0_u8; IO_BUFFER_SIZE];
    while remaining > 0 {
        let requested = remaining.min(buffer.len());
        reader
            .read_exact(&mut buffer[..requested])
            .map_err(io_error)?;
        checksum.update(&buffer[..requested]);
        writer.write_all(&buffer[..requested]).map_err(io_error)?;
        remaining -= requested;
    }
    Ok(())
}

fn copy_exact<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    size: usize,
) -> Result<(), FitsError> {
    let mut remaining = size;
    let mut buffer = [0_u8; IO_BUFFER_SIZE];
    while remaining > 0 {
        let requested = remaining.min(buffer.len());
        reader
            .read_exact(&mut buffer[..requested])
            .map_err(io_error)?;
        writer.write_all(&buffer[..requested]).map_err(io_error)?;
        remaining -= requested;
    }
    Ok(())
}

#[derive(Default)]
struct OnesComplement {
    sum: u64,
    tail: Vec<u8>,
}

impl OnesComplement {
    fn update(&mut self, bytes: &[u8]) {
        let mut bytes = bytes;
        if !self.tail.is_empty() {
            let needed = 4 - self.tail.len();
            let take = needed.min(bytes.len());
            self.tail.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.tail.len() == 4 {
                self.add_word(&self.tail.clone());
                self.tail.clear();
            }
        }
        for chunk in bytes.chunks_exact(4) {
            self.add_word(chunk);
        }
        self.tail
            .extend_from_slice(bytes.chunks_exact(4).remainder());
    }

    fn add_word(&mut self, word: &[u8]) {
        self.sum += u32::from_be_bytes([word[0], word[1], word[2], word[3]]) as u64;
        self.fold();
    }

    fn fold(&mut self) {
        while self.sum >> 32 != 0 {
            self.sum = (self.sum & 0xffff_ffff) + (self.sum >> 32);
        }
    }

    fn finish(mut self) -> u32 {
        if !self.tail.is_empty() {
            let mut word = [0_u8; 4];
            word[..self.tail.len()].copy_from_slice(&self.tail);
            self.add_word(&word);
        }
        self.fold();
        self.sum as u32
    }
}

fn validate_data_checksum(expected: Option<u32>, actual: u32) -> Result<(), FitsError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    if expected != actual {
        return Err(FitsError::ChecksumMismatch {
            keyword: "DATASUM".to_string(),
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(())
}

fn validate_hdu_checksum(header_bytes: Option<&[u8]>, data_sum: u32) -> Result<(), FitsError> {
    let Some(header_bytes) = header_bytes else {
        return Ok(());
    };
    let mut header_sum = OnesComplement::default();
    let mut header_bytes = header_bytes.to_vec();
    for card in header_bytes.chunks_exact_mut(CARD_SIZE) {
        if &card[..8] == b"CHECKSUM" {
            card[11..27].fill(b'0');
            break;
        }
    }
    header_sum.update(&header_bytes);
    let actual = fold_sum(header_sum.finish(), data_sum);
    if actual != u32::MAX {
        return Err(FitsError::ChecksumMismatch {
            keyword: "CHECKSUM".to_string(),
            expected: "ones-complement negative zero".to_string(),
            actual: format!("{actual:08X}"),
        });
    }
    Ok(())
}

fn fold_sum(left: u32, right: u32) -> u32 {
    let mut sum = left as u64 + right as u64;
    while sum >> 32 != 0 {
        sum = (sum & 0xffff_ffff) + (sum >> 32);
    }
    sum as u32
}

fn io_error(error: io::Error) -> FitsError {
    FitsError::Io(error.to_string())
}

fn keyword_name(field: &[u8]) -> Result<String, FitsError> {
    let first_space = field.iter().position(|byte| *byte == b' ');
    let name_end = first_space.unwrap_or(field.len());
    if field[name_end..].iter().any(|byte| *byte != b' ') {
        return Err(FitsError::InvalidKeywordName {
            keyword: String::from_utf8_lossy(field).into_owned(),
        });
    }
    for byte in &field[..name_end] {
        if !(byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-') {
            return Err(FitsError::InvalidKeywordName {
                keyword: String::from_utf8_lossy(field).into_owned(),
            });
        }
    }
    Ok(String::from_utf8_lossy(&field[..name_end]).into_owned())
}

fn parse_card(card: &[u8], keyword: String) -> Result<Card, FitsError> {
    let has_value_indicator = &card[8..10] == b"= ";
    if keyword == "CONTINUE" && has_value_indicator {
        return Err(FitsError::InvalidValueIndicator { keyword });
    }
    if card[8] == b'=' && !has_value_indicator {
        return Err(FitsError::InvalidValueIndicator { keyword });
    }
    if !has_value_indicator || keyword == "COMMENT" || keyword == "HISTORY" {
        if keyword == "CONTINUE" && !has_value_indicator {
            let field = String::from_utf8_lossy(&card[10..]);
            if let Some((value, comment)) = parse_string_value(&field, &keyword)? {
                return Ok(Card {
                    keyword,
                    value: Some(Value::String(value)),
                    comment,
                });
            }
        }
        let start = if has_value_indicator { 10 } else { 8 };
        let text = String::from_utf8_lossy(&card[start..]).trim().to_string();
        return Ok(Card {
            keyword,
            value: None,
            comment: (!text.is_empty()).then_some(text),
        });
    }

    let field = String::from_utf8_lossy(&card[10..]);
    let Some((value, comment)) = parse_string_value(&field, &keyword)? else {
        let (value, comment) = split_value_comment(&field);
        let trimmed = value.trim();
        let value = if trimmed.is_empty() {
            Value::Undefined
        } else {
            classify_scalar(trimmed, &keyword)?
        };
        return Ok(Card {
            keyword,
            value: Some(value),
            comment,
        });
    };
    Ok(Card {
        keyword,
        value: Some(Value::String(value)),
        comment,
    })
}

fn classify_scalar(value: &str, keyword: &str) -> Result<Value, FitsError> {
    if value == "T" || value == "F" {
        return Ok(Value::Logical(value.to_string()));
    }
    if is_integer(value) {
        return Ok(Value::Integer(value.to_string()));
    }
    if is_real(value) {
        return Ok(Value::Real(value.to_string()));
    }
    if let Some((real, imaginary)) = parse_complex(value, is_integer) {
        return Ok(Value::ComplexInteger(format!("({real}, {imaginary})")));
    }
    if let Some((real, imaginary)) = parse_complex(value, is_real) {
        return Ok(Value::ComplexReal(format!("({real}, {imaginary})")));
    }
    Err(FitsError::InvalidValue {
        keyword: keyword.to_string(),
        value: value.to_string(),
    })
}

fn is_integer(value: &str) -> bool {
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_real(value: &str) -> bool {
    let value = value.strip_prefix(['+', '-']).unwrap_or(value);
    let (mantissa, exponent) = value
        .split_once(['E', 'D'])
        .map_or((value, None), |(mantissa, exponent)| {
            (mantissa, Some(exponent))
        });
    if let Some(exponent) = exponent {
        if !is_integer(exponent) {
            return false;
        }
    }
    if let Some((integer, fraction)) = mantissa.split_once('.') {
        return (!integer.is_empty() || !fraction.is_empty())
            && integer.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.bytes().all(|byte| byte.is_ascii_digit());
    }
    exponent.is_some() && !mantissa.is_empty() && mantissa.bytes().all(|byte| byte.is_ascii_digit())
}

fn parse_complex<F>(value: &str, component_is_valid: F) -> Option<(&str, &str)>
where
    F: Fn(&str) -> bool,
{
    let inner = value.strip_prefix('(')?.strip_suffix(')')?;
    let (real, imaginary) = inner.split_once(',')?;
    let real = real.trim();
    let imaginary = imaginary.trim();
    component_is_valid(real)
        .then_some((real, imaginary))
        .filter(|(_, imaginary)| component_is_valid(imaginary))
}

fn parse_string_value(
    field: &str,
    keyword: &str,
) -> Result<Option<(String, Option<String>)>, FitsError> {
    let leading = field.len() - field.trim_start_matches(' ').len();
    let value_field = &field[leading..];
    if !value_field.starts_with('\'') {
        return Ok(None);
    }

    let bytes = value_field.as_bytes();
    let mut index = 1;
    let mut value = String::new();
    while index < bytes.len() {
        if bytes[index] != b'\'' {
            value.push(bytes[index] as char);
            index += 1;
            continue;
        }
        if index + 1 < bytes.len() && bytes[index + 1] == b'\'' {
            value.push('\'');
            index += 2;
        } else {
            let remainder = &value_field[index + 1..];
            let comment = parse_comment_suffix(remainder, keyword)?;
            return Ok(Some((value, comment)));
        }
    }
    Err(FitsError::InvalidValue {
        keyword: keyword.to_string(),
        value: "unterminated character string".to_string(),
    })
}

fn parse_comment_suffix(suffix: &str, keyword: &str) -> Result<Option<String>, FitsError> {
    let suffix = suffix.trim_start_matches(' ');
    if suffix.is_empty() {
        return Ok(None);
    }
    if let Some(comment) = suffix.strip_prefix('/') {
        return Ok(Some(comment.trim().to_string()));
    }
    Err(FitsError::InvalidValue {
        keyword: keyword.to_string(),
        value: "characters after closing quote must be spaces or a comment".to_string(),
    })
}

fn split_value_comment(field: &str) -> (String, Option<String>) {
    let mut in_string = false;
    for (index, character) in field.char_indices() {
        if character == '\'' {
            in_string = !in_string;
        } else if character == '/' && !in_string {
            let comment = field[index + 1..].trim();
            return (
                field[..index].trim_end().to_string(),
                (!comment.is_empty()).then_some(comment.to_string()),
            );
        }
    }
    (field.trim_end().to_string(), None)
}

fn validate_header(header: &Header, primary: bool) -> Result<(), FitsError> {
    let keywords: Vec<&str> = header
        .cards
        .iter()
        .map(|card| card.keyword.as_str())
        .collect();
    let naxis = validate_integer(header, "NAXIS", |value| (0..=999).contains(&value))?;
    let naxis = naxis as usize;
    let mut expected: Vec<String> = if primary {
        vec![
            "SIMPLE".to_string(),
            "BITPIX".to_string(),
            "NAXIS".to_string(),
        ]
    } else {
        vec![
            "XTENSION".to_string(),
            "BITPIX".to_string(),
            "NAXIS".to_string(),
        ]
    };
    for axis in 1..=naxis {
        expected.push(format!("NAXIS{axis}"));
    }
    if !primary {
        expected.extend(["PCOUNT".to_string(), "GCOUNT".to_string()]);
    }
    for (index, expected_keyword) in expected.iter().enumerate() {
        if keywords.get(index) != Some(&expected_keyword.as_str()) {
            return Err(if keywords.get(index).is_none() {
                FitsError::MissingMandatoryKeyword {
                    keyword: (*expected_keyword).to_string(),
                }
            } else {
                FitsError::InvalidKeywordOrder {
                    keyword: (*expected_keyword).to_string(),
                }
            });
        }
    }
    if primary {
        if keywords.contains(&"XTENSION") {
            return Err(FitsError::InvalidKeywordOrder {
                keyword: "XTENSION".to_string(),
            });
        }
        validate_logical(header, "SIMPLE")?;
        validate_fixed_logical(header, "SIMPLE")?;
    } else {
        if keywords.contains(&"SIMPLE") {
            return Err(FitsError::InvalidKeywordOrder {
                keyword: "SIMPLE".to_string(),
            });
        }
        validate_string(header, "XTENSION")?;
        validate_fixed_string(header, "XTENSION", true)?;
        validate_integer(header, "PCOUNT", |value| value >= 0)?;
        validate_integer(header, "GCOUNT", |value| value >= 0)?;
        if let Some(index) = keywords.iter().position(|keyword| *keyword == "INHERIT") {
            let mandatory_count = 5 + naxis;
            if index != mandatory_count {
                return Err(FitsError::InvalidReservedPlacement {
                    keyword: "INHERIT".to_string(),
                });
            }
        }
    }
    validate_reserved_keywords(header, primary)?;
    validate_array_keywords(header, primary)?;
    validate_extension_and_integrity_keywords(header, primary)?;
    validate_integer(header, "BITPIX", |value| {
        matches!(value, 8 | 16 | 32 | 64 | -32 | -64)
    })?;
    validate_fixed_integer(header, "BITPIX")?;
    for axis in 1..=naxis {
        validate_integer(header, &format!("NAXIS{axis}"), |value| value >= 0)?;
        validate_fixed_integer(header, &format!("NAXIS{axis}"))?;
    }
    if primary {
        validate_fixed_integer(header, "NAXIS")?;
    } else {
        validate_fixed_integer(header, "NAXIS")?;
        validate_fixed_integer(header, "PCOUNT")?;
        validate_fixed_integer(header, "GCOUNT")?;
    }
    for keyword in keywords {
        if let Some(axis) = indexed_axis(keyword) {
            if axis == 0 || axis > naxis {
                return Err(FitsError::InvalidKeywordOrder {
                    keyword: keyword.to_string(),
                });
            }
        }
    }
    Ok(())
}

fn validate_reserved_keywords(header: &Header, primary: bool) -> Result<(), FitsError> {
    for (index, card) in header.cards.iter().enumerate() {
        match card.keyword.as_str() {
            keyword if keyword.starts_with("DATE") => validate_date_keyword(card)?,
            "ORIGIN" | "TELESCOP" | "INSTRUME" | "OBSERVER" | "OBJECT" | "AUTHOR" | "REFERENC" => {
                validate_string(header, card.keyword.as_str())?
            }
            "EXTEND" => {
                if !primary {
                    return Err(FitsError::InvalidReservedPlacement {
                        keyword: "EXTEND".to_string(),
                    });
                }
                validate_logical(header, "EXTEND")?;
            }
            "BLOCKED" => {
                if !primary || index >= 36 {
                    return Err(FitsError::InvalidReservedPlacement {
                        keyword: "BLOCKED".to_string(),
                    });
                }
                if header.value("BLOCKED") != Some("T") {
                    return Err(FitsError::InvalidReservedValue {
                        keyword: "BLOCKED".to_string(),
                        value: header.value("BLOCKED").unwrap_or("missing").to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_array_keywords(header: &Header, primary: bool) -> Result<(), FitsError> {
    let is_image = primary || header.value("XTENSION") == Some("IMAGE");
    let array_keywords = ["BSCALE", "BZERO", "BUNIT", "BLANK", "DATAMIN", "DATAMAX"];
    if !is_image {
        if let Some(keyword) = array_keywords
            .iter()
            .find(|keyword| header.cards.iter().any(|card| card.keyword == **keyword))
        {
            return Err(FitsError::InvalidArrayKeyword {
                keyword: (*keyword).to_string(),
                message: "keyword is only valid for array HDUs".to_string(),
            });
        }
        return Ok(());
    }

    let has_bscale = header.cards.iter().any(|card| card.keyword == "BSCALE");
    let has_bzero = header.cards.iter().any(|card| card.keyword == "BZERO");
    if has_bscale != has_bzero {
        return Err(FitsError::InvalidArrayKeyword {
            keyword: if has_bscale { "BSCALE" } else { "BZERO" }.to_string(),
            message: "BSCALE and BZERO must be used together".to_string(),
        });
    }
    for keyword in ["BSCALE", "BZERO"] {
        if has_bscale && !is_real_value(header, keyword) {
            return Err(FitsError::InvalidArrayKeyword {
                keyword: keyword.to_string(),
                message: "expected a floating-point value".to_string(),
            });
        }
    }
    if header.cards.iter().any(|card| card.keyword == "BUNIT") {
        if card_value(header, "BUNIT")
            .and_then(Value::as_unit)
            .is_none()
        {
            return Err(FitsError::InvalidArrayKeyword {
                keyword: "BUNIT".to_string(),
                message: "expected a valid unit string".to_string(),
            });
        }
    }
    if header.cards.iter().any(|card| card.keyword == "BLANK") {
        let bitpix = header
            .value("BITPIX")
            .and_then(|value| value.parse::<i64>().ok());
        if !bitpix.is_some_and(|value| value > 0) || !is_integer_value(header, "BLANK") {
            return Err(FitsError::InvalidArrayKeyword {
                keyword: "BLANK".to_string(),
                message: "BLANK requires positive BITPIX and an integer value".to_string(),
            });
        }
    }
    for keyword in ["DATAMIN", "DATAMAX"] {
        if header.cards.iter().any(|card| card.keyword == keyword)
            && !is_real_value(header, keyword)
        {
            return Err(FitsError::InvalidArrayKeyword {
                keyword: keyword.to_string(),
                message: "expected a floating-point value".to_string(),
            });
        }
    }
    Ok(())
}

fn validate_extension_and_integrity_keywords(
    header: &Header,
    primary: bool,
) -> Result<(), FitsError> {
    for keyword in ["EXTNAME", "DATASUM", "CHECKSUM"] {
        if header.cards.iter().any(|card| card.keyword == keyword)
            && !matches!(card_value(header, keyword), Some(Value::String(_)))
        {
            return Err(FitsError::InvalidReservedValue {
                keyword: keyword.to_string(),
                value: "expected a character string".to_string(),
            });
        }
    }
    for keyword in ["EXTVER", "EXTLEVEL"] {
        if header.cards.iter().any(|card| card.keyword == keyword)
            && !is_integer_value(header, keyword)
        {
            return Err(FitsError::InvalidReservedValue {
                keyword: keyword.to_string(),
                value: "expected an integer".to_string(),
            });
        }
    }
    if let Some(value) = header.value("EXTLEVEL") {
        if value.parse::<i64>().ok().is_none_or(|value| value < 1) {
            return Err(FitsError::InvalidReservedValue {
                keyword: "EXTLEVEL".to_string(),
                value: value.to_string(),
            });
        }
    }
    if header.cards.iter().any(|card| card.keyword == "INHERIT") {
        if primary {
            return Err(FitsError::InvalidReservedPlacement {
                keyword: "INHERIT".to_string(),
            });
        }
        validate_logical(header, "INHERIT")?;
    }
    validate_datasum(header)?;
    validate_checksum(header)?;
    Ok(())
}

fn validate_datasum(header: &Header) -> Result<(), FitsError> {
    let Some(Value::String(value)) = card_value(header, "DATASUM") else {
        return Ok(());
    };
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(())
    } else {
        Err(FitsError::InvalidIntegrityKeyword {
            keyword: "DATASUM".to_string(),
            message: "expected an unsigned decimal string or blanks".to_string(),
        })
    }
}

fn validate_checksum(header: &Header) -> Result<(), FitsError> {
    let Some(Value::String(value)) = card_value(header, "CHECKSUM") else {
        return Ok(());
    };
    let trimmed = value.trim();
    if trimmed.is_empty() || value.len() == 16 {
        Ok(())
    } else {
        Err(FitsError::InvalidIntegrityKeyword {
            keyword: "CHECKSUM".to_string(),
            message: "expected a 16-character checksum string or blanks".to_string(),
        })
    }
}

fn card_value<'a>(header: &'a Header, keyword: &str) -> Option<&'a Value> {
    header
        .cards
        .iter()
        .find(|card| card.keyword == keyword)
        .and_then(|card| card.value.as_ref())
}

fn is_real_value(header: &Header, keyword: &str) -> bool {
    matches!(
        card_value(header, keyword),
        Some(Value::Integer(_) | Value::Real(_))
    )
}

fn is_integer_value(header: &Header, keyword: &str) -> bool {
    matches!(card_value(header, keyword), Some(Value::Integer(_)))
}

fn validate_date_keyword(card: &Card) -> Result<(), FitsError> {
    let Some(Value::String(value)) = card.value.as_ref() else {
        return Err(FitsError::InvalidReservedValue {
            keyword: card.keyword.clone(),
            value: "expected a character string".to_string(),
        });
    };
    if is_iso_date(value) || is_iso_datetime(value) || is_legacy_date(value) {
        Ok(())
    } else {
        Err(FitsError::InvalidReservedValue {
            keyword: card.keyword.clone(),
            value: value.to_string(),
        })
    }
}

fn is_iso_date(value: &str) -> bool {
    let parts: Vec<_> = value.split('-').collect();
    if parts.len() != 3
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
        || !parts
            .iter()
            .all(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return false;
    }
    let year = parts[0].parse::<u32>().ok();
    let month = parts[1].parse::<u32>().ok();
    let day = parts[2].parse::<u32>().ok();
    matches!((year, month, day), (Some(year), Some(month), Some(day)) if (1..=12).contains(&month) && (1..=days_in_month(year, month)).contains(&day))
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn is_iso_datetime(value: &str) -> bool {
    let Some((date, time)) = value.split_once('T') else {
        return false;
    };
    if !is_iso_date(date) {
        return false;
    }
    let time = time.strip_suffix('Z').unwrap_or(time);
    let parts: Vec<_> = time.split(':').collect();
    if parts.len() != 3 || parts[0].len() != 2 || parts[1].len() != 2 {
        return false;
    }
    let (seconds, fraction) = parts[2]
        .split_once('.')
        .map_or((parts[2], None), |(s, f)| (s, Some(f)));
    parts[0].bytes().all(|byte| byte.is_ascii_digit())
        && parts[1].bytes().all(|byte| byte.is_ascii_digit())
        && seconds.len() == 2
        && seconds.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.is_none_or(|fraction| {
            !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn is_legacy_date(value: &str) -> bool {
    let parts: Vec<_> = value.split('/').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn indexed_axis(keyword: &str) -> Option<usize> {
    let suffix = keyword.strip_prefix("NAXIS")?;
    if suffix.is_empty() || (suffix.len() > 1 && suffix.starts_with('0')) {
        return None;
    }
    suffix.parse().ok()
}

fn validate_integer<F>(header: &Header, keyword: &str, predicate: F) -> Result<i64, FitsError>
where
    F: Fn(i64) -> bool,
{
    let value = header
        .value(keyword)
        .ok_or_else(|| FitsError::MissingMandatoryKeyword {
            keyword: keyword.to_string(),
        })?;
    let parsed = value
        .parse::<i64>()
        .map_err(|_| FitsError::InvalidMandatoryValue {
            keyword: keyword.to_string(),
            value: value.to_string(),
        })?;
    if !predicate(parsed) {
        return Err(FitsError::InvalidMandatoryValue {
            keyword: keyword.to_string(),
            value: value.to_string(),
        });
    }
    Ok(parsed)
}

fn validate_logical(header: &Header, keyword: &str) -> Result<(), FitsError> {
    if !matches!(header.value(keyword), Some("T" | "F")) {
        return Err(FitsError::InvalidMandatoryValue {
            keyword: keyword.to_string(),
            value: header.value(keyword).unwrap_or("missing").to_string(),
        });
    }
    Ok(())
}

fn validate_string(header: &Header, keyword: &str) -> Result<(), FitsError> {
    let is_string = header
        .cards
        .iter()
        .find(|card| card.keyword == keyword)
        .is_some_and(|card| matches!(card.value.as_ref(), Some(Value::String(_))));
    if !is_string {
        return Err(FitsError::InvalidMandatoryValue {
            keyword: keyword.to_string(),
            value: header.value(keyword).unwrap_or("missing").to_string(),
        });
    }
    Ok(())
}

fn raw_card<'a>(header: &'a Header, keyword: &str) -> Result<&'a [u8], FitsError> {
    header
        .raw_cards
        .iter()
        .find(|card| {
            card.as_bytes()
                .get(..8)
                .is_some_and(|field| String::from_utf8_lossy(field).trim() == keyword)
        })
        .map(|card| card.as_bytes())
        .ok_or_else(|| FitsError::MissingMandatoryKeyword {
            keyword: keyword.to_string(),
        })
}

fn validate_fixed_logical(header: &Header, keyword: &str) -> Result<(), FitsError> {
    let card = raw_card(header, keyword)?;
    if card.get(29).is_none_or(|byte| !matches!(byte, b'T' | b'F')) {
        return Err(FitsError::InvalidMandatoryFormat {
            keyword: keyword.to_string(),
        });
    }
    Ok(())
}

fn validate_fixed_integer(header: &Header, keyword: &str) -> Result<(), FitsError> {
    let card = raw_card(header, keyword)?;
    if card.len() < 30 || card[29] == b' ' {
        return Err(FitsError::InvalidMandatoryFormat {
            keyword: keyword.to_string(),
        });
    }
    let field =
        std::str::from_utf8(&card[10..30]).map_err(|_| FitsError::InvalidMandatoryFormat {
            keyword: keyword.to_string(),
        })?;
    if field.trim() != header.value(keyword).unwrap_or("") {
        return Err(FitsError::InvalidMandatoryFormat {
            keyword: keyword.to_string(),
        });
    }
    Ok(())
}

fn validate_fixed_string(
    header: &Header,
    keyword: &str,
    require_eight: bool,
) -> Result<(), FitsError> {
    let card = raw_card(header, keyword)?;
    if card.get(10) != Some(&b'\'') {
        return Err(FitsError::InvalidMandatoryFormat {
            keyword: keyword.to_string(),
        });
    }
    if require_eight
        && header
            .string_value(keyword)
            .is_none_or(|value| value.len() < 8)
    {
        return Err(FitsError::InvalidMandatoryFormat {
            keyword: keyword.to_string(),
        });
    }
    Ok(())
}

fn data_size(header: &Header, primary: bool) -> Result<usize, FitsError> {
    if !primary && extension_type(header).is_none() {
        return Err(FitsError::MissingExtensionType);
    }
    let axes = axis_product(header)?;
    let array_bytes = checked_mul(axes, bytes_per_value(header)?, "BITPIX/NAXIS")?;
    let with_parameters = checked_add(
        array_bytes,
        keyword_usize_or_zero(header, "PCOUNT")?,
        "PCOUNT",
    )?;
    checked_mul(
        with_parameters,
        keyword_usize_or_one(header, "GCOUNT")?,
        "GCOUNT",
    )
}

fn extension_type(header: &Header) -> Option<&str> {
    header
        .value("XTENSION")
        .map(|value| value.trim_matches('\'').trim())
        .filter(|value| !value.is_empty())
}

fn axis_product(header: &Header) -> Result<usize, FitsError> {
    let count = keyword_usize_or_zero(header, "NAXIS")?;
    let mut product = 1;
    for axis in 1..=count {
        product = checked_mul(
            product,
            keyword_usize(header, &format!("NAXIS{axis}"))?,
            "NAXIS",
        )?;
    }
    if count == 0 { Ok(0) } else { Ok(product) }
}

fn bytes_per_value(header: &Header) -> Result<usize, FitsError> {
    let bitpix = header
        .value("BITPIX")
        .ok_or_else(|| FitsError::InvalidValue {
            keyword: "BITPIX".to_string(),
            value: "missing".to_string(),
        })?
        .parse::<i64>()
        .map_err(|_| FitsError::InvalidValue {
            keyword: "BITPIX".to_string(),
            value: "not an integer".to_string(),
        })?;
    match bitpix.unsigned_abs() {
        8 | 16 | 32 | 64 => Ok(bitpix.unsigned_abs() as usize / 8),
        _ if bitpix == -32 || bitpix == -64 => Ok(bitpix.unsigned_abs() as usize / 8),
        _ => Err(FitsError::InvalidValue {
            keyword: "BITPIX".to_string(),
            value: bitpix.to_string(),
        }),
    }
}

fn keyword_usize(header: &Header, keyword: &str) -> Result<usize, FitsError> {
    header
        .value(keyword)
        .ok_or_else(|| FitsError::InvalidValue {
            keyword: keyword.to_string(),
            value: "missing".to_string(),
        })?
        .parse::<usize>()
        .map_err(|_| FitsError::InvalidValue {
            keyword: keyword.to_string(),
            value: "not a non-negative integer".to_string(),
        })
}

fn keyword_usize_or_zero(header: &Header, keyword: &str) -> Result<usize, FitsError> {
    header
        .value(keyword)
        .map_or(Ok(0), |_| keyword_usize(header, keyword))
}

fn keyword_usize_or_one(header: &Header, keyword: &str) -> Result<usize, FitsError> {
    header
        .value(keyword)
        .map_or(Ok(1), |_| keyword_usize(header, keyword))
}

fn checked_mul(left: usize, right: usize, keyword: &str) -> Result<usize, FitsError> {
    left.checked_mul(right)
        .ok_or_else(|| FitsError::SizeOverflow {
            keyword: keyword.to_string(),
        })
}

fn checked_add(left: usize, right: usize, keyword: &str) -> Result<usize, FitsError> {
    left.checked_add(right)
        .ok_or_else(|| FitsError::SizeOverflow {
            keyword: keyword.to_string(),
        })
}

fn padded_size(size: usize) -> Result<usize, FitsError> {
    let remainder = size % BLOCK_SIZE;
    if remainder == 0 {
        Ok(size)
    } else {
        checked_add(size, BLOCK_SIZE - remainder, "data padding")
    }
}
