// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

use std::io::{self, BufRead, BufReader, Cursor, Read, Write};

use super::{Card, FitsError, FitsFile, Hdu, HduIndex, Header};

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
}

struct PendingData {
    size: usize,
    padded_size: usize,
}

impl<R: Read> FitsReader<R> {
    /// Creates a reader with a bounded internal buffer over any `Read` source.
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::with_capacity(IO_BUFFER_SIZE, reader),
            first_hdu: true,
            pending_data: None,
            at_special_records: false,
        }
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
        let data_size = data_size(&header, self.first_hdu)?;
        let padded_size = padded_size(data_size)?;
        self.pending_data = Some(PendingData {
            size: data_size,
            padded_size,
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
        copy_exact(&mut self.reader, &mut writer, pending.size)?;
        discard_exact(&mut self.reader, padding)?;
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
    let mut header_size = 0;
    let mut block = [0_u8; BLOCK_SIZE];

    loop {
        reader.read_exact(&mut block).map_err(io_error)?;
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
                header_size,
            });
        }
    }
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

fn discard_exact<R: Read>(reader: &mut R, size: usize) -> Result<(), FitsError> {
    copy_exact(reader, &mut io::sink(), size)
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
    if card[8] == b'=' && !has_value_indicator {
        return Err(FitsError::InvalidValueIndicator { keyword });
    }
    if !has_value_indicator || keyword == "COMMENT" || keyword == "HISTORY" {
        let start = if has_value_indicator { 10 } else { 8 };
        let text = String::from_utf8_lossy(&card[start..]).trim().to_string();
        return Ok(Card {
            keyword,
            value: None,
            comment: (!text.is_empty()).then_some(text),
        });
    }

    let field = String::from_utf8_lossy(&card[10..]);
    let (value, comment) = split_value_comment(&field);
    Ok(Card {
        keyword,
        value: Some(value.trim().to_string()),
        comment,
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
        8 | 16 | 32 | 64 | 128 => Ok(bitpix.unsigned_abs() as usize / 8),
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
