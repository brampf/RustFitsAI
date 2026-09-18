// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

/// A complete, owned FITS file.
///
/// This model corresponds to the ordered primary HDU, conforming extensions,
/// and optional special records described in FITS 4.0 Sections 3.1, 3.4, and
/// 3.5. It is intentionally not used by the streaming CLI for large files.
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
/// This is the memory-efficient representation used by [`FitsReader`]. The
/// `data_size` value is derived from the FITS size keywords specified in
/// Section 4.4.1; the Data Unit remains in the input stream until requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HduIndex {
    /// Parsed header and raw 80-character cards for this HDU.
    pub header: Header,
    /// Number of meaningful Data Unit bytes, excluding FITS block padding.
    pub data_size: usize,
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
    pub raw_cards: Vec<String>,
    /// Header length in complete FITS blocks.
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
            .and_then(|card| card.value.as_deref())
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
    pub value: Option<String>,
    pub comment: Option<String>,
}
