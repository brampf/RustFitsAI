// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

use std::io::Cursor;

use rustfitsai::{BLOCK_SIZE, FitsError, FitsFile, FitsReader};

fn card(text: &str) -> [u8; 80] {
    let mut result = [b' '; 80];
    result[..text.len()].copy_from_slice(text.as_bytes());
    result
}

fn header(cards: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for text in cards {
        bytes.extend_from_slice(&card(text));
    }
    bytes.resize(BLOCK_SIZE, b' ');
    bytes
}

#[test]
fn parses_primary_hdu_and_data() {
    let mut bytes = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "NAXIS   =                    1",
        "NAXIS1  =                    3",
        "END",
    ]);
    bytes.extend_from_slice(b"abc");
    bytes.resize(BLOCK_SIZE * 2, 0);

    let file = FitsFile::from_bytes(&bytes).expect("valid FITS should parse");

    assert_eq!(file.hdus.len(), 1);
    assert_eq!(file.hdus[0].header.value("BITPIX"), Some("8"));
    assert_eq!(file.hdus[0].data, b"abc");
    assert_eq!(file.hdus[0].extension_type(), None);
    assert_eq!(file.hdus[0].header.raw_cards[4].trim(), "END");
    assert!(file.special_records.is_empty());
}

#[test]
fn parses_extensions_in_file_order() {
    let mut bytes = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "NAXIS   =                    0",
        "END",
    ]);
    bytes.extend_from_slice(&header(&[
        "XTENSION= 'CUSTOM  '",
        "BITPIX  =                    8",
        "NAXIS   =                    1",
        "NAXIS1  =                    4",
        "PCOUNT  =                    2",
        "GCOUNT  =                    1",
        "END",
    ]));
    bytes.extend_from_slice(b"abcdef");
    bytes.resize(BLOCK_SIZE * 3, 0);

    let file = FitsFile::from_bytes(&bytes).expect("valid extension should parse");

    assert_eq!(file.hdus.len(), 2);
    assert_eq!(file.hdus[1].extension_type(), Some("CUSTOM"));
    assert_eq!(file.hdus[1].data, b"abcdef");
    assert!(file.special_records.is_empty());
}

#[test]
fn classifies_non_extension_block_as_special_record() {
    let mut bytes = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "NAXIS   =                    0",
        "END",
    ]);
    let record = header(&[
        "BITPIX  =                    8",
        "NAXIS   =                    0",
        "END",
    ]);
    bytes.extend_from_slice(&record);

    let file = FitsFile::from_bytes(&bytes).expect("non-extension block is a special record");
    assert_eq!(file.hdus.len(), 1);
    assert_eq!(file.special_records, vec![record]);
}

#[test]
fn preserves_special_records_after_last_hdu() {
    let mut bytes = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "NAXIS   =                    0",
        "END",
    ]);
    let mut special = vec![b' '; BLOCK_SIZE];
    special[..8].copy_from_slice(b"SPECIAL!");
    bytes.extend_from_slice(&special);

    let file = FitsFile::from_bytes(&bytes).expect("valid special record should parse");

    assert_eq!(file.special_records, vec![special]);
}

#[test]
fn ignores_partial_physical_tail_after_special_records() {
    let mut bytes = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "NAXIS   =                    0",
        "END",
    ]);
    bytes.extend_from_slice(b"partial");

    let file = FitsFile::from_bytes(&bytes).expect("physical tail should be ignored");
    assert!(file.special_records.is_empty());
}

#[test]
fn rejects_xtension_in_special_record_prefix() {
    let mut bytes = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "NAXIS   =                    0",
        "END",
    ]);
    let mut special = vec![b' '; BLOCK_SIZE];
    special[..8].copy_from_slice(b"XTENSION");
    bytes.extend_from_slice(&special);

    assert!(matches!(
        FitsFile::from_bytes(&bytes),
        Err(FitsError::InvalidKeyword { .. }) | Err(FitsError::InvalidSpecialRecords { .. })
    ));
}

#[test]
fn streams_data_only_when_requested() {
    let mut bytes = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "NAXIS   =                    1",
        "NAXIS1  =                    6",
        "END",
    ]);
    bytes.extend_from_slice(b"stream");
    bytes.resize(BLOCK_SIZE * 2, 0);

    let mut reader = FitsReader::new(Cursor::new(bytes));
    let index = reader
        .next_hdu()
        .expect("header should parse")
        .expect("primary HDU should exist");
    assert_eq!(index.data_size, 6);
    assert!(matches!(reader.next_hdu(), Err(FitsError::DataPending)));

    let mut streamed = Vec::new();
    reader
        .read_data_to(&mut streamed)
        .expect("data should stream to the writer");
    assert_eq!(streamed, b"stream");
    assert!(reader.next_hdu().expect("end should be readable").is_none());
}
