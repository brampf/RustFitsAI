// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

//! FITS data models, validation, and sequential parsing.
//!
//! The implementation follows FITS Standard 4.0, especially Sections 3.1-3.6
//! the keyword/value rules in Sections 4.1-4.4.2.7, and data representation in
//! Sections 5 and 6.

mod error;
mod model;
mod parser;

pub use error::FitsError;
pub use model::{
    Card, DataRepresentation, DataValue, DateTime, FitsFile, Hdu, HduIndex, Header,
    RandomGroupsDescriptor, UnitString, Value,
};
pub use parser::{BLOCK_SIZE, CARD_SIZE, FitsLimits, FitsReader};
