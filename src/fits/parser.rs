// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

use std::io::{self, BufReader, Cursor, Read, Write};

use super::{
    Card, DataRepresentation, DataValue, DateTime, FitsError, FitsFile, Hdu, HduIndex, Header,
    Value,
};

/// Logical FITS block size from FITS 4.0 Sections 3.1-3.3.
pub const BLOCK_SIZE: usize = 2880;
/// FITS header keyword-record size from FITS 4.0 Section 3.3.1.
pub const CARD_SIZE: usize = 80;
const IO_BUFFER_SIZE: usize = 64 * 1024;

/// Limits on the FITS bytes retained by the reader and owned parsing methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FitsLimits {
    /// Original bytes per header, including padding to a complete FITS block.
    /// Defaults to 1 MiB.
    pub max_header_bytes: usize,
    /// Bytes retained by an owned read. Defaults to 256 MiB.
    ///
    /// [`FitsReader::read_data`] counts unpadded payload bytes and
    /// [`FitsReader::read_special_records`] counts complete special records.
    /// [`FitsFile::from_bytes_with_limits`] counts these across the whole file,
    /// including original header bytes. Parsed cards and container overhead
    /// use additional memory. This limit does not apply when streaming data or
    /// special records.
    pub max_owned_bytes: usize,
}

impl Default for FitsLimits {
    fn default() -> Self {
        Self {
            max_header_bytes: 1024 * 1024,
            max_owned_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Sequential, buffered FITS reader.
///
/// `FitsReader` buffers input and parses headers one at a time. Callers must
/// consume or skip the current Data Unit before requesting
/// the next HDU. This matches the ordered structure required by FITS 4.0
/// Sections 3.1 and 3.2 and avoids allocating very large Data Units.
/// Header, data, and special-record parsing or I/O errors stop the reader.
/// Subsequent operations then return [`FitsError::ReaderFailed`]. Call-order
/// errors and owned Data Unit size or allocation errors leave the reader usable.
pub struct FitsReader<R> {
    reader: BufReader<R>,
    first_hdu: bool,
    pending_data: Option<PendingData>,
    at_special_records: bool,
    special_prefix: [u8; 8],
    special_prefix_len: usize,
    lax_checksums: bool,
    limits: FitsLimits,
    failed: bool,
}

struct PendingData {
    size: usize,
    padded_size: usize,
    datasum: Option<u32>,
    header_sum: Option<u32>,
    representation: super::DataRepresentation,
}

impl<R: Read> FitsReader<R> {
    /// Creates a strict reader with the default [`FitsLimits`].
    pub fn new(reader: R) -> Self {
        Self::with_lax_and_limits(reader, false, FitsLimits::default())
    }

    /// Creates a reader with default limits and optional lax checksum handling.
    /// Lax mode skips checksum accumulation and comparisons. Structural and
    /// keyword validation, including `DATASUM` syntax and range, remain active.
    pub fn with_lax(reader: R, lax_checksums: bool) -> Self {
        Self::with_lax_and_limits(reader, lax_checksums, FitsLimits::default())
    }

    /// Creates a strict reader with custom limits.
    pub fn with_limits(reader: R, limits: FitsLimits) -> Self {
        Self::with_lax_and_limits(reader, false, limits)
    }

    /// Creates a reader with custom limits and optional lax checksum handling.
    pub fn with_lax_and_limits(reader: R, lax_checksums: bool, limits: FitsLimits) -> Self {
        Self {
            reader: BufReader::with_capacity(IO_BUFFER_SIZE, reader),
            first_hdu: true,
            pending_data: None,
            at_special_records: false,
            special_prefix: [0; 8],
            special_prefix_len: 0,
            lax_checksums,
            limits,
            failed: false,
        }
    }

    /// Parses the next primary or conforming extension header.
    ///
    /// The returned [`HduIndex`] is an index into the still-streamed Data Unit.
    /// `None` indicates that the reader reached the special-record section or
    /// logical end of the FITS stream. See FITS 4.0 Sections 3.4 and 3.5.
    pub fn next_hdu(&mut self) -> Result<Option<HduIndex>, FitsError> {
        self.next_hdu_with_budget(None)
    }

    fn next_hdu_with_budget(
        &mut self,
        owned_remaining: Option<usize>,
    ) -> Result<Option<HduIndex>, FitsError> {
        self.check_usable()?;
        if self.pending_data.is_some() {
            return Err(FitsError::DataPending);
        }
        if self.at_special_records {
            return Ok(None);
        }
        self.failed = true;
        let next_hdu = self.parse_next_hdu(owned_remaining)?;
        self.failed = false;
        Ok(next_hdu)
    }

    fn parse_next_hdu(
        &mut self,
        owned_remaining: Option<usize>,
    ) -> Result<Option<HduIndex>, FitsError> {
        if !self.first_hdu {
            let filled = read_up_to(&mut self.reader, &mut self.special_prefix)?;
            if filled < self.special_prefix.len() || &self.special_prefix != b"XTENSION" {
                self.special_prefix_len = filled;
                self.at_special_records = true;
                return Ok(None);
            }
        }

        let prefix = if self.first_hdu {
            &[][..]
        } else {
            &self.special_prefix[..]
        };
        let header = parse_header(&mut self.reader, prefix, self.limits, owned_remaining)?;
        let integrity = validate_header(&header, self.first_hdu)?;
        let data_size = data_size(&header, self.first_hdu)?;
        let padded_size = padded_size(data_size)?;
        let header_sum = if !self.lax_checksums && integrity.has_checksum {
            let mut checksum = OnesComplement::default();
            checksum.update(&header.raw_bytes);
            Some(checksum.finish())
        } else {
            None
        };
        self.pending_data = Some(PendingData {
            size: data_size,
            padded_size,
            datasum: if self.lax_checksums {
                None
            } else {
                integrity.datasum
            },
            header_sum,
            representation: DataRepresentation::from_bitpix(header.value("BITPIX").unwrap())
                .expect("validated BITPIX has a data representation"),
        });
        self.first_hdu = false;

        Ok(Some(HduIndex { header, data_size }))
    }

    /// Streams the current Data Unit to `writer` and discards its block padding.
    ///
    /// This is the preferred method for very large Data Units. FITS padding is
    /// defined by the 2880-byte blocking rules in Sections 3.1, 3.2, and 3.6.
    /// If strict mode has a nonblank `DATASUM` or `CHECKSUM`, verification
    /// includes the payload and its padding.
    /// Treat bytes written as provisional until this method succeeds.
    pub fn read_data_to<W: Write>(&mut self, mut writer: W) -> Result<(), FitsError> {
        let pending = self.take_pending_data()?;
        let padding = pending.padded_size - pending.size;
        let mut checksum = pending.checksum();
        copy_exact(
            &mut self.reader,
            &mut writer,
            pending.size,
            checksum.as_mut(),
        )?;
        copy_exact(
            &mut self.reader,
            &mut io::sink(),
            padding,
            checksum.as_mut(),
        )?;
        pending.validate_checksums(checksum)?;
        self.failed = false;
        Ok(())
    }

    /// Reads the current Data Unit into memory on explicit request.
    ///
    /// Prefer [`FitsReader::read_data_to`] or [`FitsReader::skip_data`] when the
    /// Data Unit may be large. An owned size limit or allocation failure leaves
    /// the Data Unit pending so it can still be streamed or skipped.
    pub fn read_data(&mut self) -> Result<Vec<u8>, FitsError> {
        self.check_usable()?;
        let size = self
            .pending_data
            .as_ref()
            .ok_or(FitsError::NoPendingData)?
            .size;
        check_limit(size, self.limits.max_owned_bytes, "owned data")?;
        let mut data = Vec::new();
        data.try_reserve_exact(size)
            .map_err(|_| FitsError::SizeOverflow {
                keyword: "data allocation".to_string(),
            })?;
        self.read_data_to(&mut data)?;
        Ok(data)
    }

    /// Decodes the current Data Unit value-by-value in FITS big-endian order.
    ///
    /// The callback receives one scalar at a time, so this method does not
    /// allocate the Data Unit. `BITPIX=8` is exposed as [`DataValue::Byte`];
    /// callers may validate the high bit when using the bytes as ASCII as
    /// required by FITS 5.1.
    /// Treat values delivered to the callback as provisional until success.
    pub fn read_data_values<F>(&mut self, mut callback: F) -> Result<(), FitsError>
    where
        F: FnMut(DataValue),
    {
        let pending = self.take_pending_data()?;
        let width = match pending.representation {
            super::DataRepresentation::Byte => 1,
            super::DataRepresentation::Integer16 => 2,
            super::DataRepresentation::Integer32 | super::DataRepresentation::Float32 => 4,
            super::DataRepresentation::Integer64 | super::DataRepresentation::Float64 => 8,
        };
        if pending.size % width != 0 {
            return Err(FitsError::InvalidValue {
                keyword: "BITPIX".to_string(),
                value: "data size is not aligned to its scalar width".to_string(),
            });
        }
        let padding = pending.padded_size - pending.size;
        let mut checksum = pending.checksum();
        let mut value = [0_u8; 8];
        for _ in 0..pending.size / width {
            self.reader
                .read_exact(&mut value[..width])
                .map_err(io_error)?;
            if let Some(checksum) = &mut checksum {
                checksum.update(&value[..width]);
            }
            callback(decode_value(pending.representation, &value));
        }
        copy_exact(
            &mut self.reader,
            &mut io::sink(),
            padding,
            checksum.as_mut(),
        )?;
        pending.validate_checksums(checksum)?;
        self.failed = false;
        Ok(())
    }

    fn check_usable(&self) -> Result<(), FitsError> {
        if self.failed {
            Err(FitsError::ReaderFailed)
        } else {
            Ok(())
        }
    }

    fn take_pending_data(&mut self) -> Result<PendingData, FitsError> {
        self.check_usable()?;
        let pending = self.pending_data.take().ok_or(FitsError::NoPendingData)?;
        self.failed = true;
        Ok(pending)
    }

    /// Consumes the current Data Unit without retaining its bytes.
    pub fn skip_data(&mut self) -> Result<(), FitsError> {
        self.read_data_to(io::sink())
    }

    /// Collects trailing special records into memory.
    ///
    /// Special records are restricted-use 2880-byte blocks as described in
    /// FITS 4.0 Section 3.5. Use [`FitsReader::copy_special_records_to`] for a
    /// streaming alternative. Exceeding the owned size limit stops the reader.
    pub fn read_special_records(&mut self) -> Result<Vec<Vec<u8>>, FitsError> {
        self.read_special_records_with_budget(self.limits.max_owned_bytes)
    }

    fn read_special_records_with_budget(
        &mut self,
        mut owned_remaining: usize,
    ) -> Result<Vec<Vec<u8>>, FitsError> {
        self.start_special_records()?;
        let mut records = Vec::new();
        let mut record = [0_u8; BLOCK_SIZE];
        while self.next_special_record(&mut record, records.len())? {
            owned_remaining = owned_remaining
                .checked_sub(BLOCK_SIZE)
                .ok_or_else(|| limit_error("owned FITS read", self.limits.max_owned_bytes))?;
            records.push(record.to_vec());
        }
        self.failed = false;
        Ok(records)
    }

    /// Streams trailing special records to `writer` and returns their count.
    ///
    /// The first eight bytes of each record are checked against the `XTENSION`
    /// restriction in FITS 4.0 Section 3.5. A partial physical tail is ignored
    /// as permitted by the physical-blocking rules in Section 3.6.1.
    /// Treat bytes written as provisional until this method succeeds.
    pub fn copy_special_records_to<W: Write>(&mut self, mut writer: W) -> Result<usize, FitsError> {
        self.start_special_records()?;
        let mut record = [0_u8; BLOCK_SIZE];
        let mut count = 0;
        while self.next_special_record(&mut record, count)? {
            writer.write_all(&record).map_err(io_error)?;
            count += 1;
        }
        self.failed = false;
        Ok(count)
    }

    fn start_special_records(&mut self) -> Result<(), FitsError> {
        self.check_usable()?;
        if !self.at_special_records {
            return Err(FitsError::SpecialRecordsNotReady);
        }
        self.failed = true;
        Ok(())
    }

    fn next_special_record(
        &mut self,
        record: &mut [u8; BLOCK_SIZE],
        count: usize,
    ) -> Result<bool, FitsError> {
        let prefix_len = std::mem::take(&mut self.special_prefix_len);
        record[..prefix_len].copy_from_slice(&self.special_prefix[..prefix_len]);
        let read = read_up_to(&mut self.reader, &mut record[prefix_len..])?;
        if read + prefix_len < BLOCK_SIZE {
            return Ok(false);
        }
        if &record[..8] == b"XTENSION" {
            return Err(FitsError::InvalidSpecialRecords {
                offset: count.saturating_mul(BLOCK_SIZE),
                message: "the first eight bytes must not contain XTENSION".to_string(),
            });
        }
        Ok(true)
    }
}

impl PendingData {
    fn checksum(&self) -> Option<OnesComplement> {
        (self.datasum.is_some() || self.header_sum.is_some()).then(OnesComplement::default)
    }

    fn validate_checksums(self, checksum: Option<OnesComplement>) -> Result<(), FitsError> {
        if let Some(checksum) = checksum {
            let data_sum = checksum.finish();
            validate_data_checksum(self.datasum, data_sum)?;
            validate_hdu_checksum(self.header_sum, data_sum)?;
        }
        Ok(())
    }
}

impl FitsFile {
    /// Parses a complete FITS slice with strict checksum handling and default limits.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FitsError> {
        Self::from_bytes_with_limits(bytes, FitsLimits::default())
    }

    /// Parses a complete FITS slice using strict checksums and custom limits.
    ///
    /// The owned budget includes the bytes retained for every header, unpadded
    /// data payload, and complete special record. A Data Unit whose padded
    /// length exceeds the remaining input returns [`FitsError::Truncated`]
    /// before its payload allocation is attempted.
    pub fn from_bytes_with_limits(bytes: &[u8], limits: FitsLimits) -> Result<Self, FitsError> {
        if bytes.is_empty() {
            return Err(FitsError::EmptyFile);
        }

        let mut reader = FitsReader::with_limits(Cursor::new(bytes), limits);
        let mut owned_remaining = limits.max_owned_bytes;
        let mut hdus = Vec::new();
        while let Some(index) = reader.next_hdu_with_budget(Some(owned_remaining))? {
            let consumed =
                reader.reader.get_ref().position() as usize - reader.reader.buffer().len();
            let available = bytes.len() - consumed;
            let padded = padded_size(index.data_size)?;
            if padded > available {
                return Err(FitsError::Truncated {
                    offset: bytes.len(),
                    expected: padded - available,
                });
            }
            owned_remaining = owned_remaining
                .checked_sub(index.header.header_size)
                .and_then(|remaining| remaining.checked_sub(index.data_size))
                .ok_or_else(|| limit_error("owned FITS read", limits.max_owned_bytes))?;
            let data = reader.read_data()?;
            hdus.push(Hdu {
                header: index.header,
                data,
            });
        }
        let special_records = reader.read_special_records_with_budget(owned_remaining)?;
        Ok(Self {
            hdus,
            special_records,
        })
    }
}

fn parse_header<R: Read>(
    reader: &mut R,
    mut prefix: &[u8],
    limits: FitsLimits,
    owned_remaining: Option<usize>,
) -> Result<Header, FitsError> {
    let mut cards = Vec::new();
    let mut raw_cards = Vec::new();
    let mut raw_bytes = Vec::new();
    let mut header_size = 0;
    let mut block = [0_u8; BLOCK_SIZE];

    loop {
        let next_size = checked_add(header_size, BLOCK_SIZE, "header size")?;
        check_limit(next_size, limits.max_header_bytes, "header")?;
        if owned_remaining.is_some_and(|remaining| next_size > remaining) {
            return Err(limit_error("owned FITS read", limits.max_owned_bytes));
        }
        block[..prefix.len()].copy_from_slice(prefix);
        reader
            .read_exact(&mut block[prefix.len()..])
            .map_err(io_error)?;
        prefix = &[];
        raw_bytes.extend_from_slice(&block);
        header_size = next_size;
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

fn copy_exact<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    size: usize,
    mut checksum: Option<&mut OnesComplement>,
) -> Result<(), FitsError> {
    let mut remaining = size;
    let mut buffer = [0_u8; IO_BUFFER_SIZE];
    while remaining > 0 {
        let requested = remaining.min(buffer.len());
        reader
            .read_exact(&mut buffer[..requested])
            .map_err(io_error)?;
        if let Some(checksum) = &mut checksum {
            checksum.update(&buffer[..requested]);
        }
        writer.write_all(&buffer[..requested]).map_err(io_error)?;
        remaining -= requested;
    }
    Ok(())
}

fn read_up_to<R: Read>(reader: &mut R, buffer: &mut [u8]) -> Result<usize, FitsError> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(filled)
}

fn check_limit(size: usize, limit: usize, resource: &'static str) -> Result<(), FitsError> {
    if size > limit {
        return Err(limit_error(resource, limit));
    }
    Ok(())
}

fn limit_error(resource: &'static str, limit: usize) -> FitsError {
    FitsError::LimitExceeded { resource, limit }
}

fn decode_value(representation: DataRepresentation, bytes: &[u8; 8]) -> DataValue {
    match representation {
        DataRepresentation::Byte => DataValue::Byte(bytes[0]),
        DataRepresentation::Integer16 => {
            DataValue::Integer16(i16::from_be_bytes([bytes[0], bytes[1]]))
        }
        DataRepresentation::Integer32 => {
            DataValue::Integer32(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }
        DataRepresentation::Integer64 => DataValue::Integer64(i64::from_be_bytes(*bytes)),
        DataRepresentation::Float32 => DataValue::Float32(f32::from_bits(u32::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
        ]))),
        DataRepresentation::Float64 => {
            DataValue::Float64(f64::from_bits(u64::from_be_bytes(*bytes)))
        }
    }
}

#[derive(Default)]
struct OnesComplement {
    sum: u32,
    tail: [u8; 4],
    tail_len: usize,
}

impl OnesComplement {
    fn update(&mut self, bytes: &[u8]) {
        let mut bytes = bytes;
        if self.tail_len != 0 {
            let needed = self.tail.len() - self.tail_len;
            let take = needed.min(bytes.len());
            self.tail[self.tail_len..self.tail_len + take].copy_from_slice(&bytes[..take]);
            self.tail_len += take;
            if self.tail_len < self.tail.len() {
                return;
            }
            self.sum = fold_sum(self.sum, u32::from_be_bytes(self.tail));
            bytes = &bytes[take..];
        }
        let mut words = bytes.chunks_exact(4);
        for word in &mut words {
            self.sum = fold_sum(
                self.sum,
                u32::from_be_bytes([word[0], word[1], word[2], word[3]]),
            );
        }
        let remainder = words.remainder();
        self.tail_len = remainder.len();
        self.tail[..self.tail_len].copy_from_slice(remainder);
    }

    fn finish(mut self) -> u32 {
        if self.tail_len != 0 {
            self.tail[self.tail_len..].fill(0);
            self.sum = fold_sum(self.sum, u32::from_be_bytes(self.tail));
        }
        self.sum
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

fn validate_hdu_checksum(header_sum: Option<u32>, data_sum: u32) -> Result<(), FitsError> {
    let Some(header_sum) = header_sum else {
        return Ok(());
    };
    let actual = fold_sum(header_sum, data_sum);
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
    let (sum, carry) = left.overflowing_add(right);
    sum + u32::from(carry)
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

fn validate_header(header: &Header, primary: bool) -> Result<IntegrityKeywords, FitsError> {
    let keywords = || header.cards.iter().map(|card| card.keyword.as_str());
    let naxis = validate_integer(header, "NAXIS", |value| (0..=999).contains(&value))?;
    let naxis = naxis as usize;
    let random_groups = primary && header.value("GROUPS") == Some("T");
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
    if random_groups {
        expected.extend([
            "GROUPS".to_string(),
            "PCOUNT".to_string(),
            "GCOUNT".to_string(),
        ]);
    } else if !primary {
        expected.extend(["PCOUNT".to_string(), "GCOUNT".to_string()]);
    }
    let mut actual = keywords();
    for expected_keyword in expected {
        let keyword = actual.next();
        if keyword != Some(expected_keyword.as_str()) {
            return Err(if keyword.is_none() {
                FitsError::MissingMandatoryKeyword {
                    keyword: expected_keyword,
                }
            } else {
                FitsError::InvalidKeywordOrder {
                    keyword: expected_keyword,
                }
            });
        }
    }
    if primary {
        if keywords().any(|keyword| keyword == "XTENSION") {
            return Err(FitsError::InvalidKeywordOrder {
                keyword: "XTENSION".to_string(),
            });
        }
        validate_logical(header, "SIMPLE")?;
        validate_fixed_logical(header, "SIMPLE")?;
        if random_groups {
            validate_logical(header, "GROUPS")?;
            validate_fixed_logical(header, "GROUPS")?;
            validate_integer(header, "PCOUNT", |value| value >= 0)?;
            validate_integer(header, "GCOUNT", |value| value >= 0)?;
            validate_fixed_integer(header, "PCOUNT")?;
            validate_fixed_integer(header, "GCOUNT")?;
        }
    } else {
        if keywords().any(|keyword| keyword == "SIMPLE") {
            return Err(FitsError::InvalidKeywordOrder {
                keyword: "SIMPLE".to_string(),
            });
        }
        validate_string(header, "XTENSION")?;
        validate_fixed_string(header, "XTENSION", true)?;
        validate_integer(header, "PCOUNT", |value| value >= 0)?;
        validate_integer(header, "GCOUNT", |value| value >= 0)?;
        if let Some(index) = keywords().position(|keyword| keyword == "INHERIT") {
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
    validate_extension_keywords(header, primary)?;
    let integrity = parse_integrity_keywords(header)?;
    if random_groups {
        validate_random_groups_keywords(header, naxis)?;
    }
    validate_integer(header, "BITPIX", |value| {
        matches!(value, 8 | 16 | 32 | 64 | -32 | -64)
    })?;
    validate_fixed_integer(header, "BITPIX")?;
    for axis in 1..=naxis {
        validate_integer(header, &format!("NAXIS{axis}"), |value| value >= 0)?;
        validate_fixed_integer(header, &format!("NAXIS{axis}"))?;
    }
    if random_groups && header.value("NAXIS1") != Some("0") {
        return Err(FitsError::InvalidMandatoryValue {
            keyword: "NAXIS1".to_string(),
            value: header.value("NAXIS1").unwrap_or("missing").to_string(),
        });
    }
    if primary {
        validate_fixed_integer(header, "NAXIS")?;
    } else {
        validate_fixed_integer(header, "NAXIS")?;
        validate_fixed_integer(header, "PCOUNT")?;
        validate_fixed_integer(header, "GCOUNT")?;
    }
    for keyword in keywords() {
        if let Some(axis) = indexed_axis(keyword) {
            if axis == 0 || axis > naxis {
                return Err(FitsError::InvalidKeywordOrder {
                    keyword: keyword.to_string(),
                });
            }
        }
    }
    Ok(integrity)
}

fn validate_reserved_keywords(header: &Header, primary: bool) -> Result<(), FitsError> {
    for (index, card) in header.cards.iter().enumerate() {
        match card.keyword.as_str() {
            keyword if keyword.starts_with("DATE") => validate_date_keyword(card)?,
            "ORIGIN" | "TELESCOP" | "INSTRUME" | "OBSERVER" | "OBJECT" | "AUTHOR" | "REFERENC" => {
                validate_string_value(&card.keyword, card.value.as_ref())?
            }
            "EXTEND" => {
                if !primary {
                    return Err(FitsError::InvalidReservedPlacement {
                        keyword: "EXTEND".to_string(),
                    });
                }
                validate_logical_value(&card.keyword, card.value.as_ref())?;
            }
            "BLOCKED" => {
                if !primary || index >= 36 {
                    return Err(FitsError::InvalidReservedPlacement {
                        keyword: "BLOCKED".to_string(),
                    });
                }
                if !matches!(&card.value, Some(Value::Logical(value)) if value == "T") {
                    return Err(FitsError::InvalidReservedValue {
                        keyword: card.keyword.clone(),
                        value: card
                            .value
                            .as_ref()
                            .and_then(Value::as_str)
                            .unwrap_or("missing")
                            .to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_random_groups_keywords(header: &Header, naxis: usize) -> Result<(), FitsError> {
    if naxis < 2 {
        return Err(FitsError::InvalidMandatoryValue {
            keyword: "NAXIS".to_string(),
            value: naxis.to_string(),
        });
    }
    let parameter_count = keyword_usize(header, "PCOUNT")?;
    const PSCAL: u8 = 1;
    const PZERO: u8 = 2;
    // A parameter keyword has a five-byte prefix and at most three index digits.
    let mut scaling = [0_u8; 1000];
    for card in &header.cards {
        let prefix = card.keyword.get(..5).unwrap_or("");
        let suffix = card.keyword.get(5..).unwrap_or("");
        if !matches!(prefix, "PTYPE" | "PSCAL" | "PZERO") {
            continue;
        }
        let Ok(index) = suffix.parse::<usize>() else {
            return Err(FitsError::InvalidReservedValue {
                keyword: card.keyword.clone(),
                value: "invalid random-groups parameter index".to_string(),
            });
        };
        if index == 0 || index > parameter_count {
            return Err(FitsError::InvalidReservedPlacement {
                keyword: card.keyword.clone(),
            });
        }
        match (prefix, &card.value) {
            ("PTYPE", value) => validate_string_value(&card.keyword, value.as_ref())?,
            ("PSCAL" | "PZERO", Some(Value::Integer(_) | Value::Real(_))) => {
                scaling[index] |= if prefix == "PSCAL" { PSCAL } else { PZERO };
            }
            ("PSCAL" | "PZERO", _) => {
                return Err(FitsError::InvalidReservedValue {
                    keyword: card.keyword.clone(),
                    value: "expected a floating-point value".to_string(),
                });
            }
            _ => unreachable!(),
        }
    }
    if let Some(index) = scaling
        .iter()
        .position(|flags| matches!(*flags, PSCAL | PZERO))
    {
        return Err(FitsError::InvalidReservedValue {
            keyword: format!("PSCAL{index}"),
            value: "PSCALn and PZEROn must be used together".to_string(),
        });
    }
    Ok(())
}

fn validate_array_keywords(header: &Header, primary: bool) -> Result<(), FitsError> {
    let is_image = primary || extension_type(header) == Some("IMAGE");
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

fn validate_extension_keywords(header: &Header, primary: bool) -> Result<(), FitsError> {
    if let Some(card) = header.cards.iter().find(|card| card.keyword == "EXTNAME") {
        reserved_string(card)?;
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
    Ok(())
}

struct IntegrityKeywords {
    datasum: Option<u32>,
    has_checksum: bool,
}

fn parse_integrity_keywords(header: &Header) -> Result<IntegrityKeywords, FitsError> {
    let mut datasum = None;
    let mut checksum = None;
    for card in &header.cards {
        match card.keyword.as_str() {
            "DATASUM" => validate_integrity_repeat(&mut datasum, parse_datasum(card)?, card)?,
            "CHECKSUM" => validate_integrity_repeat(&mut checksum, parse_checksum(card)?, card)?,
            _ => {}
        }
    }
    Ok(IntegrityKeywords {
        datasum: datasum.flatten(),
        has_checksum: checksum.flatten().is_some(),
    })
}

fn validate_integrity_repeat<T: PartialEq>(
    previous: &mut Option<T>,
    value: T,
    card: &Card,
) -> Result<(), FitsError> {
    if previous.as_ref().is_some_and(|previous| *previous != value) {
        return Err(FitsError::InvalidIntegrityKeyword {
            keyword: card.keyword.clone(),
            message: "conflicting duplicate values".to_string(),
        });
    }
    *previous = Some(value);
    Ok(())
}

fn reserved_string(card: &Card) -> Result<&str, FitsError> {
    let Some(Value::String(value)) = &card.value else {
        return Err(FitsError::InvalidReservedValue {
            keyword: card.keyword.clone(),
            value: "expected a character string".to_string(),
        });
    };
    Ok(value)
}

fn parse_datasum(card: &Card) -> Result<Option<u32>, FitsError> {
    let trimmed = reserved_string(card)?.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return trimmed
            .parse::<u32>()
            .map(Some)
            .map_err(|_| FitsError::InvalidIntegrityKeyword {
                keyword: card.keyword.clone(),
                message: "value exceeds the unsigned 32-bit range".to_string(),
            });
    }
    Err(FitsError::InvalidIntegrityKeyword {
        keyword: card.keyword.clone(),
        message: "expected an unsigned decimal string or blanks".to_string(),
    })
}

fn parse_checksum(card: &Card) -> Result<Option<&str>, FitsError> {
    let value = reserved_string(card)?;
    if value.trim().is_empty() {
        Ok(None)
    } else if value.len() == 16 {
        Ok(Some(value))
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
    let value = reserved_string(card)?;
    if DateTime::parse(value).is_some() {
        Ok(())
    } else {
        Err(FitsError::InvalidReservedValue {
            keyword: card.keyword.clone(),
            value: value.to_string(),
        })
    }
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

fn validate_logical_value(keyword: &str, value: Option<&Value>) -> Result<(), FitsError> {
    if !matches!(value, Some(Value::Logical(value)) if value == "T" || value == "F") {
        return Err(FitsError::InvalidMandatoryValue {
            keyword: keyword.to_string(),
            value: value
                .and_then(Value::as_str)
                .unwrap_or("missing")
                .to_string(),
        });
    }
    Ok(())
}

fn validate_string(header: &Header, keyword: &str) -> Result<(), FitsError> {
    validate_string_value(keyword, card_value(header, keyword))
}

fn validate_string_value(keyword: &str, value: Option<&Value>) -> Result<(), FitsError> {
    if !matches!(value, Some(Value::String(_))) {
        return Err(FitsError::InvalidMandatoryValue {
            keyword: keyword.to_string(),
            value: value
                .and_then(Value::as_str)
                .unwrap_or("missing")
                .to_string(),
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
    if primary && header.value("GROUPS") == Some("T") {
        let axes = keyword_usize(header, "NAXIS")?;
        let mut elements_per_group = 1;
        for axis in 2..=axes {
            elements_per_group = checked_mul(
                elements_per_group,
                keyword_usize(header, &format!("NAXIS{axis}"))?,
                "NAXIS",
            )?;
        }
        let group_values = checked_add(
            keyword_usize(header, "PCOUNT")?,
            elements_per_group,
            "PCOUNT/NAXIS",
        )?;
        return checked_mul(
            checked_mul(group_values, keyword_usize(header, "GCOUNT")?, "GCOUNT")?,
            bytes_per_value(header)?,
            "BITPIX/GCOUNT",
        );
    }
    let axes = axis_product(header)?;
    if primary {
        return checked_mul(axes, bytes_per_value(header)?, "BITPIX/NAXIS");
    }
    let with_parameters = checked_add(axes, keyword_usize(header, "PCOUNT")?, "PCOUNT")?;
    checked_mul(
        checked_mul(with_parameters, keyword_usize(header, "GCOUNT")?, "GCOUNT")?,
        bytes_per_value(header)?,
        "BITPIX/GCOUNT",
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
