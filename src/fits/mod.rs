// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

//! FITS data models, validation, and sequential parsing.
//!
//! The implementation follows FITS Standard 4.0, especially Sections 3.1-3.6
//! and the keyword rules in Section 4.4.1.

mod error;
mod model;
mod parser;

pub use error::FitsError;
pub use model::{Card, DateTime, FitsFile, Hdu, HduIndex, Header, UnitString, Value};
pub use parser::{BLOCK_SIZE, CARD_SIZE, FitsReader};
