// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

use std::{env, fs, io, path::PathBuf, process};

use rustfitsai::FitsReader;

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("error: {error}");
        eprintln!("usage: rustfitsai [-v|--verbose] [-lax|--lax] <input> [output]");
        process::exit(1);
    }
}

fn run(arguments: Vec<String>) -> io::Result<()> {
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        print_help();
        return Ok(());
    }

    let verbose = arguments
        .iter()
        .any(|argument| argument == "--verbose" || argument == "-v");
    let lax = arguments
        .iter()
        .any(|argument| argument == "--lax" || argument == "-lax");
    let positional: Vec<&String> = arguments
        .iter()
        .filter(|argument| !matches!(argument.as_str(), "--verbose" | "-v" | "--lax" | "-lax"))
        .collect();
    if positional.is_empty() || positional.len() > 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected an input path and an optional output path",
        ));
    }

    let input = PathBuf::from(positional[0]);
    let file = fs::File::open(&input)?;
    let mut reader = FitsReader::with_lax(file, lax);
    let mut hdu_count = 0;
    while let Some(index) = reader
        .next_hdu()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
    {
        let extension = index
            .header
            .value("XTENSION")
            .map(|value| value.trim_matches('\'').trim())
            .filter(|value| !value.is_empty())
            .unwrap_or("PRIMARY");
        println!(
            "  HDU {hdu_count} ({extension}): {} header bytes, {} data bytes",
            index.header.header_size, index.data_size
        );
        if verbose {
            for line in &index.header.raw_cards {
                println!("    {line}");
            }
        }
        reader
            .skip_data()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        hdu_count += 1;
    }
    let special_records = reader
        .copy_special_records_to(io::sink())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    println!("Parsed {hdu_count} HDU(s) from {}", input.display());
    println!("  Special records: {special_records} block(s)");

    if let Some(output) = positional.get(1) {
        fs::copy(&input, output)?;
        println!("Copied {} to {}", input.display(), output);
    }
    Ok(())
}

fn print_help() {
    println!("usage: rustfitsai [-v|--verbose] [-lax|--lax] <input> [output]");
    println!(
        "\nReads and parses FITS HDUs. Verbose mode prints every logical 80-character header card. Lax mode skips DATASUM/CHECKSUM validation. If output is supplied, the original bytes are also written there."
    );
}
