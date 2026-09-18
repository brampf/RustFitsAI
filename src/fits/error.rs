// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FitsError {
    Io(String),
    EmptyFile,
    Truncated { offset: usize, expected: usize },
    InvalidHeader { offset: usize, message: String },
    InvalidKeyword { offset: usize },
    InvalidValue { keyword: String, value: String },
    MissingExtensionType,
    InvalidSpecialRecords { offset: usize, message: String },
    DataPending,
    NoPendingData,
    SpecialRecordsNotReady,
    SizeOverflow { keyword: String },
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
            Self::InvalidValue { keyword, value } => {
                write!(formatter, "invalid value for {keyword}: {value}")
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
