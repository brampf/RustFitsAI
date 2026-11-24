// SPDX-License-Identifier: MIT

use std::io::{self, Read, Write};

use rustfitsai::{BLOCK_SIZE, CARD_SIZE, DataValue, FitsError, FitsFile, FitsLimits, FitsReader};

const IMAGE: &[u8] = include_bytes!("fixtures/image.fits");
const EMPTY_PRIMARY: &[&str] = &[
    "SIMPLE  =                    T",
    "BITPIX  =                    8",
    "NAXIS   =                    0",
    "END",
];

fn header(cards: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for card in cards {
        assert!(card.len() <= CARD_SIZE);
        bytes.extend_from_slice(card.as_bytes());
        bytes.resize(bytes.len().div_ceil(CARD_SIZE) * CARD_SIZE, b' ');
    }
    bytes.resize(bytes.len().div_ceil(BLOCK_SIZE) * BLOCK_SIZE, b' ');
    bytes
}

fn primary_data(size: usize, integrity_cards: &[&str]) -> Vec<u8> {
    let axis = format!("NAXIS1  = {size:>20}");
    let mut cards = vec![
        EMPTY_PRIMARY[0],
        EMPTY_PRIMARY[1],
        "NAXIS   =                    1",
        &axis,
    ];
    cards.extend_from_slice(integrity_cards);
    cards.push("END");
    header(&cards)
}

#[test]
fn verifies_independently_generated_checksums_and_uses_original_header_padding() {
    let file = FitsFile::from_bytes(IMAGE).unwrap();
    assert_eq!(file.hdus.len(), 2);
    assert_eq!(file.hdus[0].data, [8, 16, 32, 64]);
    assert_eq!(file.hdus[1].extension_type(), Some("IMAGE"));

    let mut changed = IMAGE.to_vec();
    changed[BLOCK_SIZE - 1] = b'!';
    assert!(matches!(
        FitsFile::from_bytes(&changed),
        Err(FitsError::ChecksumMismatch { keyword, .. }) if keyword == "CHECKSUM"
    ));
    let mut lax = FitsReader::with_lax(changed.as_slice(), true);
    lax.next_hdu().unwrap().unwrap();
    let mut pixels = Vec::new();
    lax.read_data_values(|value| pixels.push(value)).unwrap();
    assert_eq!(pixels, [8, 16, 32, 64].map(DataValue::Byte));
}

#[test]
fn verifies_data_padding_and_byte_values_split_across_checksum_words() {
    let mut bytes = primary_data(3, &["DATASUM = '16909056'"]);
    bytes.extend_from_slice(&[1, 2, 3]);
    bytes.resize(BLOCK_SIZE * 2, 0);
    let mut reader = FitsReader::new(bytes.as_slice());
    reader.next_hdu().unwrap().unwrap();
    let mut pixels = Vec::new();
    reader.read_data_values(|pixel| pixels.push(pixel)).unwrap();
    assert_eq!(pixels, [1, 2, 3].map(DataValue::Byte));

    bytes[BLOCK_SIZE * 2 - 1] = 1;
    let mut reader = FitsReader::new(bytes.as_slice());
    reader.next_hdu().unwrap().unwrap();
    assert!(matches!(
        reader.skip_data(),
        Err(FitsError::ChecksumMismatch { keyword, .. }) if keyword == "DATASUM"
    ));
    assert!(matches!(reader.next_hdu(), Err(FitsError::ReaderFailed)));
}

#[test]
fn rejects_out_of_range_datasum_in_both_modes_and_accepts_u32_max() {
    let invalid = primary_data(0, &["DATASUM = '4294967296'"]);
    for lax_checksums in [false, true] {
        let mut reader = FitsReader::with_lax(invalid.as_slice(), lax_checksums);
        assert!(matches!(
            reader.next_hdu(),
            Err(FitsError::InvalidIntegrityKeyword { keyword, .. }) if keyword == "DATASUM"
        ));
    }
    let mut valid = primary_data(4, &["DATASUM = '4294967295'"]);
    valid.extend_from_slice(&[255; 4]);
    valid.resize(BLOCK_SIZE * 2, 0);
    assert_eq!(FitsFile::from_bytes(&valid).unwrap().hdus[0].data, [255; 4]);
}

#[test]
fn blank_and_missing_checksum_values_skip_comparisons() {
    for cards in [
        &[][..],
        &["DATASUM = '        '", "CHECKSUM= '                '"][..],
    ] {
        let mut bytes = primary_data(1, cards);
        bytes.push(1);
        bytes.resize(BLOCK_SIZE * 2, 0);
        assert_eq!(FitsFile::from_bytes(&bytes).unwrap().hdus[0].data, [1]);
    }
}

#[test]
fn rejects_conflicting_integrity_cards_in_both_modes() {
    for (keyword, cards) in [
        ("DATASUM", ["DATASUM = '    '", "DATASUM = '0'"]),
        ("DATASUM", ["DATASUM = '0'", "DATASUM = '1'"]),
        (
            "CHECKSUM",
            [
                "CHECKSUM= '                '",
                "CHECKSUM= '0000000000000000'",
            ],
        ),
        (
            "CHECKSUM",
            [
                "CHECKSUM= '0000000000000000'",
                "CHECKSUM= '1111111111111111'",
            ],
        ),
    ] {
        let bytes = primary_data(0, &cards);
        for lax in [false, true] {
            let mut reader = FitsReader::with_lax(bytes.as_slice(), lax);
            assert!(
                matches!(reader.next_hdu(), Err(FitsError::InvalidIntegrityKeyword { keyword: actual, .. }) if actual == keyword)
            );
        }
    }
}

#[test]
fn accepts_equivalent_integrity_cards() {
    let bytes = primary_data(
        0,
        &[
            "DATASUM = '0'",
            "DATASUM = ' 0000 '",
            "CHECKSUM= '   '",
            "CHECKSUM= '                '",
        ],
    );
    assert!(FitsFile::from_bytes(&bytes).is_ok());

    let bytes = primary_data(
        0,
        &[
            "CHECKSUM= '0000000000000000'",
            "CHECKSUM= '0000000000000000'",
        ],
    );
    let mut reader = FitsReader::with_lax(bytes.as_slice(), true);
    reader.next_hdu().unwrap().unwrap();
    reader.skip_data().unwrap();
    assert!(
        matches!(FitsFile::from_bytes(&bytes), Err(FitsError::ChecksumMismatch { keyword, .. }) if keyword == "CHECKSUM")
    );
}

#[test]
fn validates_each_occurrence_of_reserved_cards() {
    for (first, second, keyword) in [
        (
            "ORIGIN  = 'Observatory'",
            "ORIGIN  =                    0",
            "ORIGIN",
        ),
        ("EXTEND  =                    T", "EXTEND  = 'T'", "EXTEND"),
        (
            "BLOCKED =                    T",
            "BLOCKED =                    F",
            "BLOCKED",
        ),
    ] {
        let bytes = primary_data(0, &[first, second]);
        assert!(matches!(
            FitsFile::from_bytes(&bytes),
            Err(FitsError::InvalidMandatoryValue { keyword: actual, .. } | FitsError::InvalidReservedValue { keyword: actual, .. }) if actual == keyword
        ));
    }
}

struct ShortReads<'a>(&'a [u8]);

impl Read for ShortReads<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let requested = bytes.len().min(3);
        self.0.read(&mut bytes[..requested])
    }
}

#[test]
fn short_reads_preserve_extension_boundaries_and_special_record_bytes() {
    let mut bytes = header(EMPTY_PRIMARY);
    bytes.extend(header(&[
        "XTENSION= 'CUSTOM  '",
        "BITPIX  =                   16",
        "NAXIS   =                    1",
        "NAXIS1  =                 1439",
        "PCOUNT  =                    2",
        "GCOUNT  =                    1",
        "END",
    ]));
    let data_len = 2882;
    bytes.extend(vec![7; data_len]);
    bytes.resize(BLOCK_SIZE * 4, 0);
    bytes.extend(header(&[
        "XTENSION= 'IMAGE   '",
        "BITPIX  =                    8",
        "NAXIS   =                    0",
        "PCOUNT  =                    0",
        "GCOUNT  =                    1",
        "BUNIT   = 'adu'",
        "END",
    ]));
    let special = header(&["SPECIAL!"]);
    bytes.extend_from_slice(&special);
    bytes.extend_from_slice(b"tail");

    let mut reader = FitsReader::new(ShortReads(&bytes));
    reader.next_hdu().unwrap().unwrap();
    reader.skip_data().unwrap();
    assert_eq!(reader.next_hdu().unwrap().unwrap().data_size, data_len);
    assert_eq!(reader.read_data().unwrap(), vec![7; data_len]);
    let image = reader.next_hdu().unwrap().unwrap();
    assert_eq!(image.header.value("BUNIT"), Some("adu"));
    reader.skip_data().unwrap();
    assert!(reader.next_hdu().unwrap().is_none());
    assert_eq!(reader.read_special_records().unwrap(), vec![special]);
}

#[test]
fn bounds_raw_header_bytes_at_complete_blocks() {
    let mut cards = EMPTY_PRIMARY[..3].to_vec();
    cards.resize(BLOCK_SIZE / CARD_SIZE, "COMMENT first block");
    cards.push("END");
    let bytes = header(&cards);
    let limits = FitsLimits {
        max_header_bytes: BLOCK_SIZE * 2 - 1,
        ..FitsLimits::default()
    };
    let mut reader = FitsReader::with_limits(bytes.as_slice(), limits);
    assert!(matches!(
        reader.next_hdu(),
        Err(FitsError::LimitExceeded {
            resource: "header",
            ..
        })
    ));
    assert!(matches!(reader.next_hdu(), Err(FitsError::ReaderFailed)));
    let larger = FitsLimits {
        max_header_bytes: BLOCK_SIZE * 2,
        ..limits
    };
    assert_eq!(
        FitsFile::from_bytes_with_limits(&bytes, larger)
            .unwrap()
            .hdus[0]
            .header
            .header_size,
        BLOCK_SIZE * 2
    );
}

#[test]
fn owns_data_and_special_records_only_within_budget_but_can_stream_them() {
    let limits = FitsLimits {
        max_owned_bytes: 3,
        ..FitsLimits::default()
    };
    let mut bytes = IMAGE.to_vec();
    let special = header(&["SPECIAL!"]);
    bytes.extend_from_slice(&special);
    let mut reader = FitsReader::with_limits(bytes.as_slice(), limits);
    reader.next_hdu().unwrap().unwrap();
    assert!(matches!(
        reader.read_data(),
        Err(FitsError::LimitExceeded { limit: 3, .. })
    ));
    let mut pixels = Vec::new();
    reader.read_data_to(&mut pixels).unwrap();
    assert_eq!(pixels, [8, 16, 32, 64]);
    reader.next_hdu().unwrap().unwrap();
    reader.skip_data().unwrap();
    assert!(reader.next_hdu().unwrap().is_none());
    let mut streamed = Vec::new();
    assert_eq!(reader.copy_special_records_to(&mut streamed).unwrap(), 1);
    assert_eq!(streamed, special);

    let mut reader = FitsReader::with_limits(bytes.as_slice(), limits);
    while reader.next_hdu().unwrap().is_some() {
        reader.skip_data().unwrap();
    }
    assert!(matches!(
        reader.read_special_records(),
        Err(FitsError::LimitExceeded { limit: 3, .. })
    ));
    assert!(matches!(
        reader.copy_special_records_to(io::sink()),
        Err(FitsError::ReaderFailed)
    ));
}

#[test]
fn owned_file_budget_counts_all_retained_headers_payload_and_special_records() {
    let mut bytes = IMAGE.to_vec();
    bytes.extend(header(&["SPECIAL!"]));
    let retained = BLOCK_SIZE * 3 + 4;
    let limits = FitsLimits {
        max_owned_bytes: retained,
        ..FitsLimits::default()
    };
    assert_eq!(
        FitsFile::from_bytes_with_limits(&bytes, limits)
            .unwrap()
            .special_records
            .len(),
        1
    );
    for too_small in [BLOCK_SIZE * 2 + 3, retained - 1] {
        let smaller = FitsLimits {
            max_owned_bytes: too_small,
            ..limits
        };
        assert!(
            matches!(FitsFile::from_bytes_with_limits(&bytes, smaller), Err(FitsError::LimitExceeded { limit, .. }) if limit == too_small)
        );
    }
}

#[test]
fn reports_truncated_owned_payload_before_considering_its_allocation() {
    let bytes = primary_data(BLOCK_SIZE * 2, &[]);
    let limits = FitsLimits {
        max_owned_bytes: BLOCK_SIZE,
        ..FitsLimits::default()
    };
    assert_eq!(
        FitsFile::from_bytes_with_limits(&bytes, limits),
        Err(FitsError::Truncated {
            offset: BLOCK_SIZE,
            expected: BLOCK_SIZE * 2
        }),
    );
}

struct PartialWriter(Vec<u8>);

impl Write for PartialWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.is_empty() {
            self.0.push(bytes[0]);
            Ok(1)
        } else {
            Err(io::Error::other("test write failure"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn readers_stop_after_partial_writes_or_reads() {
    let mut reader = FitsReader::new(IMAGE);
    reader.next_hdu().unwrap().unwrap();
    let mut writer = PartialWriter(Vec::new());
    assert!(matches!(
        reader.read_data_to(&mut writer),
        Err(FitsError::Io(_))
    ));
    assert_eq!(writer.0, [8]);
    assert!(matches!(reader.next_hdu(), Err(FitsError::ReaderFailed)));

    let mut reader = FitsReader::new(&IMAGE[..BLOCK_SIZE + 2]);
    reader.next_hdu().unwrap().unwrap();
    let mut pixels = Vec::new();
    assert!(matches!(
        reader.read_data_values(|pixel| pixels.push(pixel)),
        Err(FitsError::Io(_))
    ));
    assert_eq!(pixels, [8, 16].map(DataValue::Byte));
    assert!(matches!(reader.skip_data(), Err(FitsError::ReaderFailed)));
}

#[test]
fn validates_only_supplied_random_group_parameter_cards() {
    let group_header = |count: usize, parameter: &str| {
        header(&[
            EMPTY_PRIMARY[0],
            "BITPIX  =                   16",
            "NAXIS   =                    2",
            "NAXIS1  =                    0",
            "NAXIS2  =                    1",
            "GROUPS  =                    T",
            &format!("PCOUNT  = {count:>20}"),
            "GCOUNT  =                    1",
            parameter,
            "END",
        ])
    };
    for (keyword, parameter) in [
        ("PTYPE999", "PTYPE999= 'WEIGHT  '"),
        ("PSCAL999", "PSCAL999=                  2.5"),
        ("PZERO999", "PZERO999=                    4"),
    ] {
        let mut data = group_header(1000, parameter);
        data.resize(BLOCK_SIZE * 2, 0);
        assert_eq!(
            FitsFile::from_bytes(&data).unwrap().hdus[0].data.len(),
            2002
        );
        assert!(
            matches!(
                FitsFile::from_bytes(&group_header(998, parameter)),
                Err(FitsError::InvalidReservedPlacement { keyword: actual }) if actual == keyword
            ),
            "{keyword}"
        );
    }

    for (keyword, parameter) in [
        ("PSCAL999", "PSCAL999= 'bad'"),
        ("PZERO999", "PZERO999=                    T"),
    ] {
        assert!(
            matches!(
                FitsFile::from_bytes(&group_header(1000, parameter)),
                Err(FitsError::InvalidReservedValue { keyword: actual, .. }) if actual == keyword
            ),
            "{keyword}"
        );
    }
}
