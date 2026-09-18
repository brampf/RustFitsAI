// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

use std::{env, fs, io, path::PathBuf, process};

use rustfitsai::FitsFile;

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("error: {error}");
        eprintln!("usage: rustfitsai [-v|--verbose] <input> [output]");
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
    let positional: Vec<&String> = arguments
        .iter()
        .filter(|argument| *argument != "--verbose" && *argument != "-v")
        .collect();
    if positional.is_empty() || positional.len() > 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected an input path and an optional output path",
        ));
    }

    let input = PathBuf::from(positional[0]);
    let contents = fs::read(&input)?;
    let fits = FitsFile::from_bytes(&contents)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    println!("Parsed {} HDU(s) from {}", fits.hdus.len(), input.display());
    for (index, hdu) in fits.hdus.iter().enumerate() {
        let extension = hdu.extension_type().map_or("PRIMARY", |name| name);
        println!(
            "  HDU {index} ({extension}): {} header bytes, {} data bytes",
            hdu.header.header_size,
            hdu.data.len()
        );
        if verbose {
            for line in &hdu.header.raw_cards {
                println!("    {line}");
            }
        }
    }
    println!("  Special records: {} block(s)", fits.special_records.len());

    if let Some(output) = positional.get(1) {
        fs::write(output, contents)?;
        println!("Copied {} to {}", input.display(), output);
    }
    Ok(())
}

fn print_help() {
    println!("usage: rustfitsai [-v|--verbose] <input> [output]");
    println!(
        "\nReads and parses FITS HDUs. Verbose mode prints every logical 80-character header card. If output is supplied, the original bytes are also written there."
    );
}
