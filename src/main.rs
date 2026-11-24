// Copyright (c) 2026 May
// SPDX-License-Identifier: MIT

use std::{
    env,
    error::Error,
    ffi::OsString,
    fs::File,
    io::{self, Read, Write},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::ExitCode,
};

use rustfitsai::{FitsError, FitsLimits, FitsReader};
use tempfile::NamedTempFile;

const USAGE: &str =
    "usage: rustfitsai [-v|--verbose] [--lax] [--max-header-bytes N] <input> [output]";

#[derive(Default)]
struct Options {
    verbose: bool,
    lax: bool,
    limits: FitsLimits,
}

fn main() -> ExitCode {
    match run(env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut arguments: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let mut options = Options::default();
    let mut paths = Vec::new();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("-h" | "--help") => {
                print_help();
                return Ok(());
            }
            Some("-v" | "--verbose") => options.verbose = true,
            Some("-lax" | "--lax") => options.lax = true,
            Some("--max-header-bytes") => {
                let limit = arguments
                    .next()
                    .and_then(|value| value.to_str()?.parse::<NonZeroUsize>().ok())
                    .ok_or("--max-header-bytes requires a positive byte count")?;
                options.limits.max_header_bytes = limit.get();
            }
            Some("--") => {
                paths.extend(arguments.map(PathBuf::from));
                break;
            }
            _ => paths.push(PathBuf::from(argument)),
        }
    }

    let (input, output) = match paths.as_slice() {
        [input] => (input, None),
        [input, output] => (input, Some(output)),
        _ => return Err("expected an input path and an optional output path".into()),
    };
    let source = File::open(input)?;
    if let Some(output) = output {
        copy_validated(source, input, output, &options)?;
    } else {
        inspect(source, input, &options)?;
    }
    Ok(())
}

fn copy_validated(
    source: File,
    input: &Path,
    output: &Path,
    options: &Options,
) -> Result<(), Box<dyn Error>> {
    if output.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("output already exists: {}", output.display()),
        )
        .into());
    }
    let permissions = source.metadata()?.permissions();
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::Permissions::from_mode(permissions.mode() & 0o777)
    };
    let directory = output.parent().filter(|p| !p.as_os_str().is_empty());
    let mut staged = NamedTempFile::new_in(directory.unwrap_or(Path::new(".")))?;
    let reader = CopyingReader {
        source,
        copy: staged.as_file_mut(),
    };
    inspect(reader, input, options)?;
    staged.as_file().sync_all()?;
    let copy = staged.persist_noclobber(output).map_err(|err| err.error)?;
    // Keep the staging file private. Restore ordinary access bits on the published handle.
    if let Err(error) = copy.set_permissions(permissions) {
        eprintln!("warning: copy saved, could not preserve permissions: {error}");
    }
    println!("Copied {} to {}", input.display(), output.display());
    Ok(())
}

// Record each byte as it enters the parser, including header padding and trailing records.
struct CopyingReader<'a> {
    source: File,
    copy: &'a mut File,
}

impl Read for CopyingReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.source.read(buffer)?;
        self.copy.write_all(&buffer[..count])?;
        Ok(count)
    }
}

fn inspect(source: impl Read, input: &Path, options: &Options) -> Result<(), FitsError> {
    let mut reader = FitsReader::with_lax_and_limits(source, options.lax, options.limits);
    let mut hdu_count = 0_u64;
    while let Some(index) = reader.next_hdu()? {
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
        if options.verbose {
            for line in &index.header.raw_cards {
                println!("    {line}");
            }
        }
        reader.skip_data()?;
        hdu_count += 1;
    }
    let special_records = reader.copy_special_records_to(io::sink())?;
    println!("Parsed {hdu_count} HDU(s) from {}", input.display());
    println!("  Special records: {special_records} block(s)");

    Ok(())
}

fn print_help() {
    println!("{USAGE}");
    println!("  -v, --verbose         Print the 80-character header cards.");
    println!("  -lax, --lax           Skip DATASUM and CHECKSUM comparisons.");
    println!("  --max-header-bytes N  Maximum bytes per padded HDU header (default 1 MiB).");
    println!("  output                Save the bytes read to a new file after parsing succeeds.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_replaced_input_path_does_not_change_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.fits");
        let output = dir.path().join("copy.fits");
        let observation = include_bytes!("../tests/fixtures/image.fits");
        fs::write(&input, observation).unwrap();
        let source = File::open(&input).unwrap();
        fs::rename(&input, dir.path().join("original.fits")).unwrap();
        fs::write(&input, b"a different file").unwrap();

        copy_validated(source, &input, &output, &Options::default()).unwrap();
        assert_eq!(fs::read(output).unwrap(), observation);
    }
}
