// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FitsFile {
    pub hdus: Vec<Hdu>,
    pub special_records: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hdu {
    pub header: Header,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HduIndex {
    pub header: Header,
    pub data_size: usize,
}

impl Hdu {
    pub fn extension_type(&self) -> Option<&str> {
        self.header
            .value("XTENSION")
            .map(|value| value.trim_matches('\'').trim())
            .filter(|value| !value.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub cards: Vec<Card>,
    pub raw_cards: Vec<String>,
    pub header_size: usize,
}

impl Header {
    pub fn value(&self, keyword: &str) -> Option<&str> {
        self.cards
            .iter()
            .find(|card| card.keyword == keyword)
            .and_then(|card| card.value.as_deref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub keyword: String,
    pub value: Option<String>,
    pub comment: Option<String>,
}
