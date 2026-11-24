# RustFitsAI

A Rust library and command line tool for reading astronomical FITS files.

## Description

RustFitsAI reads the Header and Data Units (HDUs) in a FITS file. Use the command
line tool to inspect an observation, print its header cards or make a verified
copy. The library gives you the same reader so you can process an image one
data unit at a time.

## Getting started

You need Rust and Cargo with support for the 2024 edition. From the project
directory, try the included synthetic 2 × 2 image. It has four pixels and a
second, empty image extension.

```sh
cargo run --locked -- tests/fixtures/image.fits
```

The primary HDU contains 4 data bytes. Use `-v` or `--verbose` to print the
80-character header cards. To save a byte-for-byte copy, supply a new filename.

```sh
cargo run --locked -- --verbose tests/fixtures/image.fits
cargo run --locked -- tests/fixtures/image.fits copy.fits
cargo run --locked -- --help
```

Choose an output directory you control. The copy appears after the whole file
passes the selected checks. An existing destination is refused, including a
link to the input. By default the reader checks any supplied `DATASUM` and
`CHECKSUM` values. Use `--lax` to inspect
a file while skipping those comparisons. Header syntax and data lengths are
still checked.

## Features

- Read the primary HDU, image and table extensions, and other conforming
  extension types in file order. Table data is available as raw bytes.
- Stream a data unit to a writer or decode its stored values using `BITPIX`.
  Integers and floating-point values use FITS big-endian byte order.
- Inspect typed header values, `CONTINUE` strings and random-groups metadata.
  Trailing special records are available as complete 2880-byte blocks.

The reader checks FITS structure and selected keywords. Table column schemas
and image rendering are outside its scope. [FITS Standard 4.0](https://science.data.nasa.gov/fits/documentation/standard)

## Documentation

### Reading a FITS file

`next_hdu()` reads a header. Consume or skip its data before requesting the
next HDU. This example prints the size of each data unit in the sample file.

```rust
use rustfitsai::FitsReader;
use std::{error::Error, fs::File, io};

fn main() -> Result<(), Box<dyn Error>> {
    let mut reader = FitsReader::new(File::open("tests/fixtures/image.fits")?);
    while let Some(hdu) = reader.next_hdu()? {
        println!("{} data bytes", hdu.data_size);
        reader.skip_data()?;
    }
    reader.copy_special_records_to(io::sink())?;
    Ok(())
}
```

Use `read_data_to()` for raw bytes or `read_data_values()` for individual
values. The final checksum becomes available after the last byte of the HDU.
Treat streamed output as provisional until the method succeeds.

### Working with large files

The default header limit is 1 MiB per HDU. Set `--max-header-bytes N` on the
command line if an observation needs a larger header. The in-memory readers
have a 256 MiB limit on the FITS bytes they collect. `FitsFile::from_bytes()` applies
it across the complete file it builds. Parsed cards and other Rust structures
use additional memory. Streaming a data unit to a writer can exceed 256 MiB.
Use `FitsReader::with_limits()` or `FitsFile::from_bytes_with_limits()` to
choose limits appropriate for your observations.

## Tests

```sh
cargo fmt --all --check
cargo test --locked
cargo doc --locked --no-deps --open
```

## License

Released under the MIT License. [License text](https://github.com/brampf/RustFitsAI/blob/master/LICENSE)
