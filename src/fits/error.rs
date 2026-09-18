// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FitsError {
    EmptyFile,
    Truncated { offset: usize, expected: usize },
    InvalidHeader { offset: usize, message: String },
    InvalidKeyword { offset: usize },
    InvalidValue { keyword: String, value: String },
    MissingExtensionType,
    InvalidSpecialRecords { offset: usize, message: String },
    SizeOverflow { keyword: String },
}

impl fmt::Display for FitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
            Self::SizeOverflow { keyword } => {
                write!(formatter, "FITS size overflow while reading {keyword}")
            }
        }
    }
}

impl std::error::Error for FitsError {}
