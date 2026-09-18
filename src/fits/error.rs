// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

use std::fmt;

/// Errors raised while validating FITS structure or streaming its blocks.
///
/// The structural constraints represented here primarily come from FITS 4.0
/// Sections 3.1-3.6 and the header keyword/value rules in Sections 4.1-4.4.2.7.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FitsError {
    Io(String),
    EmptyFile,
    Truncated {
        offset: usize,
        expected: usize,
    },
    InvalidHeader {
        offset: usize,
        message: String,
    },
    InvalidKeyword {
        offset: usize,
    },
    InvalidKeywordName {
        keyword: String,
    },
    InvalidValueIndicator {
        keyword: String,
    },
    InvalidValue {
        keyword: String,
        value: String,
    },
    MissingMandatoryKeyword {
        keyword: String,
    },
    InvalidKeywordOrder {
        keyword: String,
    },
    InvalidMandatoryValue {
        keyword: String,
        value: String,
    },
    InvalidMandatoryFormat {
        keyword: String,
    },
    InvalidReservedValue {
        keyword: String,
        value: String,
    },
    InvalidReservedPlacement {
        keyword: String,
    },
    InvalidArrayKeyword {
        keyword: String,
        message: String,
    },
    InvalidIntegrityKeyword {
        keyword: String,
        message: String,
    },
    ChecksumMismatch {
        keyword: String,
        expected: String,
        actual: String,
    },
    InvalidEndRecord,
    MissingExtensionType,
    InvalidSpecialRecords {
        offset: usize,
        message: String,
    },
    DataPending,
    NoPendingData,
    SpecialRecordsNotReady,
    SizeOverflow {
        keyword: String,
    },
}

impl fmt::Display for FitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O error while reading FITS data: {error}"),
            Self::EmptyFile => write!(formatter, "FITS file is empty"),
            Self::Truncated { offset, expected } => write!(
                formatter,
                "FITS data is truncated at byte {offset}; expected {expected} more bytes"
            ),
            Self::InvalidHeader { offset, message } => {
                write!(formatter, "invalid FITS header at byte {offset}: {message}")
            }
            Self::InvalidKeyword { offset } => {
                write!(formatter, "invalid FITS keyword at byte {offset}")
            }
            Self::InvalidKeywordName { keyword } => {
                write!(formatter, "invalid FITS keyword name: {keyword:?}")
            }
            Self::InvalidValueIndicator { keyword } => {
                write!(
                    formatter,
                    "invalid value indicator for FITS keyword {keyword}"
                )
            }
            Self::InvalidValue { keyword, value } => {
                write!(formatter, "invalid value for {keyword}: {value}")
            }
            Self::MissingMandatoryKeyword { keyword } => {
                write!(formatter, "missing mandatory FITS keyword: {keyword}")
            }
            Self::InvalidKeywordOrder { keyword } => {
                write!(
                    formatter,
                    "invalid order for mandatory FITS keyword: {keyword}"
                )
            }
            Self::InvalidMandatoryValue { keyword, value } => {
                write!(
                    formatter,
                    "invalid value for mandatory FITS keyword {keyword}: {value}"
                )
            }
            Self::InvalidMandatoryFormat { keyword } => {
                write!(
                    formatter,
                    "invalid fixed format for mandatory FITS keyword: {keyword}"
                )
            }
            Self::InvalidReservedValue { keyword, value } => {
                write!(
                    formatter,
                    "invalid value for reserved FITS keyword {keyword}: {value}"
                )
            }
            Self::InvalidReservedPlacement { keyword } => {
                write!(
                    formatter,
                    "reserved FITS keyword has invalid placement: {keyword}"
                )
            }
            Self::InvalidArrayKeyword { keyword, message } => {
                write!(formatter, "invalid array keyword {keyword}: {message}")
            }
            Self::InvalidIntegrityKeyword { keyword, message } => {
                write!(formatter, "invalid integrity keyword {keyword}: {message}")
            }
            Self::ChecksumMismatch {
                keyword,
                expected,
                actual,
            } => {
                write!(
                    formatter,
                    "checksum mismatch for {keyword}: expected {expected}, calculated {actual}"
                )
            }
            Self::InvalidEndRecord => {
                write!(
                    formatter,
                    "END keyword record must be space-filled after the keyword"
                )
            }
            Self::MissingExtensionType => {
                write!(formatter, "extension HDU is missing the XTENSION keyword")
            }
            Self::InvalidSpecialRecords { offset, message } => {
                write!(
                    formatter,
                    "invalid special records at byte {offset}: {message}"
                )
            }
            Self::DataPending => write!(formatter, "the current HDU data must be consumed first"),
            Self::NoPendingData => write!(formatter, "no HDU data is available to read"),
            Self::SpecialRecordsNotReady => {
                write!(
                    formatter,
                    "the reader has not reached the special-record section"
                )
            }
            Self::SizeOverflow { keyword } => {
                write!(formatter, "FITS size overflow while reading {keyword}")
            }
        }
    }
}

impl std::error::Error for FitsError {}
