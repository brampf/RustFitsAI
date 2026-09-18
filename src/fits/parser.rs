// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

use super::{Card, FitsError, FitsFile, Hdu, Header};

pub const BLOCK_SIZE: usize = 2880;
pub const CARD_SIZE: usize = 80;

impl FitsFile {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FitsError> {
        if bytes.is_empty() {
            return Err(FitsError::EmptyFile);
        }

        let mut hdus = Vec::new();
        let mut offset = 0;
        let (header, header_end) = parse_header(bytes, offset)?;
        let primary_data_size = data_size(&header, true)?;
        let padded_data_size = padded_size(primary_data_size)?;
        let data_end = checked_add(header_end, padded_data_size, "data size")?;
        if data_end > bytes.len() {
            return Err(FitsError::Truncated {
                offset: header_end,
                expected: data_end - bytes.len(),
            });
        }
        hdus.push(Hdu {
            header,
            data: bytes[header_end..header_end + primary_data_size].to_vec(),
        });
        offset = data_end;

        while offset < bytes.len() && starts_extension(bytes, offset) {
            let (header, header_end) = parse_header(bytes, offset)?;
            let data_size = data_size(&header, false)?;
            let padded_data_size = padded_size(data_size)?;
            let data_end = checked_add(header_end, padded_data_size, "data size")?;
            if data_end > bytes.len() {
                return Err(FitsError::Truncated {
                    offset: header_end,
                    expected: data_end - bytes.len(),
                });
            }

            hdus.push(Hdu {
                header,
                data: bytes[header_end..header_end + data_size].to_vec(),
            });
            offset = data_end;
        }

        let special_records = parse_special_records(bytes, offset)?;
        Ok(Self {
            hdus,
            special_records,
        })
    }
}

fn starts_extension(bytes: &[u8], offset: usize) -> bool {
    if bytes.len() - offset < 8 {
        return false;
    }
    bytes[offset..offset + 8]
        .windows(8)
        .any(|window| window == b"XTENSION")
}

fn parse_special_records(bytes: &[u8], offset: usize) -> Result<Vec<Vec<u8>>, FitsError> {
    if offset == bytes.len() {
        return Ok(Vec::new());
    }
    if (bytes.len() - offset) % BLOCK_SIZE != 0 {
        return Err(FitsError::InvalidSpecialRecords {
            offset,
            message: "records must contain whole 2880-byte blocks".to_string(),
        });
    }

    let mut records = Vec::new();
    for (index, record) in bytes[offset..].chunks_exact(BLOCK_SIZE).enumerate() {
        if record[..8].windows(8).any(|window| window == b"XTENSION") {
            return Err(FitsError::InvalidSpecialRecords {
                offset: offset + index * BLOCK_SIZE,
                message: "the first eight bytes must not contain XTENSION".to_string(),
            });
        }
        records.push(record.to_vec());
    }
    Ok(records)
}

fn parse_header(bytes: &[u8], offset: usize) -> Result<(Header, usize), FitsError> {
    let mut cards = Vec::new();
    let mut raw_cards = Vec::new();
    let mut cursor = offset;

    loop {
        let block_end = cursor
            .checked_add(BLOCK_SIZE)
            .ok_or_else(|| FitsError::SizeOverflow {
                keyword: "header size".to_string(),
            })?;
        if block_end > bytes.len() {
            return Err(FitsError::Truncated {
                offset: cursor,
                expected: block_end - bytes.len(),
            });
        }

        let mut found_end = false;
        for card_bytes in bytes[cursor..block_end].chunks_exact(CARD_SIZE) {
            if card_bytes.iter().any(|byte| !(32..=126).contains(byte)) {
                return Err(FitsError::InvalidHeader {
                    offset: cursor,
                    message: "header contains a non-ASCII printable character".to_string(),
                });
            }
            raw_cards.push(String::from_utf8_lossy(card_bytes).into_owned());
            let keyword = String::from_utf8_lossy(&card_bytes[..8]).trim().to_string();
            if keyword == "END" {
                found_end = true;
                break;
            }
            if keyword.is_empty() {
                continue;
            }
            if card_bytes[8] != b'=' && keyword != "COMMENT" && keyword != "HISTORY" {
                return Err(FitsError::InvalidKeyword { offset: cursor });
            }
            cards.push(parse_card(card_bytes));
        }

        cursor = block_end;
        if found_end {
            return Ok((
                Header {
                    cards,
                    raw_cards,
                    header_size: cursor - offset,
                },
                cursor,
            ));
        }
    }
}

fn parse_card(card: &[u8]) -> Card {
    let keyword = String::from_utf8_lossy(&card[..8]).trim().to_string();
    if card[8] != b'=' {
        let text = String::from_utf8_lossy(&card[8..]).trim().to_string();
        return Card {
            keyword,
            value: None,
            comment: (!text.is_empty()).then_some(text),
        };
    }

    let field = String::from_utf8_lossy(&card[10..]);
    let (value, comment) = split_value_comment(&field);
    Card {
        keyword,
        value: Some(value.trim().to_string()),
        comment,
    }
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
