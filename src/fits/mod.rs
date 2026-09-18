// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

mod error;
mod model;
mod parser;

pub use error::FitsError;
pub use model::{Card, FitsFile, Hdu, HduIndex, Header};
pub use parser::{BLOCK_SIZE, CARD_SIZE, FitsReader};
