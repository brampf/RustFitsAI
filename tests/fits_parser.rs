// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

use std::io::Cursor;

use rustfitsai::{BLOCK_SIZE, FitsError, FitsFile, FitsReader, Value};

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

    assert!(FitsFile::from_bytes(&bytes).is_err());
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

#[test]
fn accepts_spec_keyword_names_and_commentary_records() {
    let bytes = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "FOO-BAR =                    7",
        "COMMENT this is commentary",
        "NAXIS   =                    0",
        "END",
    ]);

    let file = FitsFile::from_bytes(&bytes).expect("valid keyword records should parse");
    assert_eq!(file.hdus[0].header.value("FOO-BAR"), Some("7"));
    assert_eq!(
        file.hdus[0].header.cards[3].comment.as_deref(),
        Some("this is commentary")
    );
}

#[test]
fn rejects_lowercase_and_embedded_spaces_in_keyword_names() {
    for invalid_card in [
        "simple  =                    T",
        "FOO BAR =                    7",
    ] {
        let bytes = header(&[invalid_card, "NAXIS   =                    0", "END"]);
        assert!(matches!(
            FitsFile::from_bytes(&bytes),
            Err(FitsError::InvalidKeywordName { .. })
        ));
    }
}

#[test]
fn requires_space_after_value_indicator() {
    let bytes = header(&[
        "SIMPLE  =X                   T",
        "NAXIS   =                    0",
        "END",
    ]);

    assert!(matches!(
        FitsFile::from_bytes(&bytes),
        Err(FitsError::InvalidValueIndicator { keyword }) if keyword == "SIMPLE"
    ));
}

#[test]
fn parses_string_values_and_distinguishes_undefined_values() {
    let bytes = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "STRKEY  = 'O''HARA  ' / a string",
        "NULLKEY =                    / undefined",
        "NAXIS   =                    0",
        "END",
    ]);

    let file = FitsFile::from_bytes(&bytes).expect("string values should parse");
    let cards = &file.hdus[0].header.cards;
    assert_eq!(file.hdus[0].header.value("STRKEY"), Some("O'HARA  "));
    assert_eq!(cards[2].value, Some(Value::String("O'HARA  ".to_string())));
    assert_eq!(cards[3].value, Some(Value::Undefined));
}

#[test]
fn reconstructs_continue_string_values() {
    let bytes = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "LONGKEY = 'first part&'",
        "CONTINUE  'second part&'",
        "CONTINUE  'last part'",
        "NAXIS   =                    0",
        "END",
    ]);

    let file = FitsFile::from_bytes(&bytes).expect("continued strings should parse");
    assert_eq!(
        file.hdus[0].header.string_value("LONGKEY").as_deref(),
        Some("first partsecond partlast part")
    );
}

#[test]
fn rejects_unterminated_string_values() {
    let bytes = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "STRKEY  = 'unterminated",
        "NAXIS   =                    0",
        "END",
    ]);

    assert!(matches!(
        FitsFile::from_bytes(&bytes),
        Err(FitsError::InvalidValue { keyword, .. }) if keyword == "STRKEY"
    ));
}

#[test]
fn classifies_logical_numeric_and_complex_values() {
    let bytes = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                   16",
        "LOGICAL =                    F",
        "INTEGER =                  +007",
        "REALNUM =              -1.2E+3",
        "REALINT =                 1E3",
        "CINT    =              (12,-4)",
        "CREAL   =          (1.5, -2.0)",
        "NAXIS   =                    0",
        "END",
    ]);

    let file = FitsFile::from_bytes(&bytes).expect("scalar values should parse");
    let header = &file.hdus[0].header;
    assert_eq!(header.cards[2].value, Some(Value::Logical("F".to_string())));
    assert_eq!(
        header.cards[3].value,
        Some(Value::Integer("+007".to_string()))
    );
    assert_eq!(
        header.cards[4].value,
        Some(Value::Real("-1.2E+3".to_string()))
    );
    assert_eq!(header.cards[5].value, Some(Value::Real("1E3".to_string())));
    assert_eq!(
        header.cards[6].value,
        Some(Value::ComplexInteger("(12, -4)".to_string()))
    );
    assert_eq!(
        header.cards[7].value,
        Some(Value::ComplexReal("(1.5, -2.0)".to_string()))
    );
}

#[test]
fn validates_datetime_and_unit_string_views() {
    let bytes = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "DATEKEY = '2026-09-19T12:34:56Z'",
        "BUNIT   = 'W m-2 Hz-1'",
        "NAXIS   =                    0",
        "END",
    ]);

    let file = FitsFile::from_bytes(&bytes).expect("datetime and unit strings should parse");
    let header = &file.hdus[0].header;
    let date = header.cards[2]
        .value
        .as_ref()
        .unwrap()
        .as_datetime()
        .unwrap();
    let unit = header.cards[3].value.as_ref().unwrap().as_unit().unwrap();
    assert_eq!(date.as_str(), "2026-09-19T12:34:56Z");
    assert_eq!(unit.as_str(), "W m-2 Hz-1");
}

#[test]
fn rejects_invalid_datetime_and_control_character_units() {
    assert!(rustfitsai::DateTime::parse("2026-09-19 12:34:56Z").is_none());
    assert!(rustfitsai::UnitString::parse("m\n").is_none());
}
