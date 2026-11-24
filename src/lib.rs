// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

#![doc = include_str!("../README.md")]

pub mod fits;

pub use fits::{
    BLOCK_SIZE, CARD_SIZE, Card, DataRepresentation, DataValue, DateTime, FitsError, FitsFile,
    FitsLimits, FitsReader, Hdu, HduIndex, Header, RandomGroupsDescriptor, UnitString, Value,
};
