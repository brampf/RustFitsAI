# RustFitsAI

A standalone Rust application boilerplate for reading and writing FITS files.

## FITS Format

FITS (Flexible Image Transport System) is a block-based format for storing
astronomical data in Header and Data Units (HDUs). This implementation is based
on the current FITS Standard, version 4.0: [FITS Standard 4.0](https://fits.gsfc.nasa.gov/standard40/fits_standard40aa-le.pdf).

The FITS reader parses a file into HDUs. Each HDU contains its parsed header
cards and its unpadded data bytes. Headers and data are located according to
the FITS 2880-byte block and 80-byte card rules. Primary HDUs, standard image
and table extensions, and other extension names are processed in file order.
Extension data sizes use the FITS `BITPIX`, `NAXISn`, `PCOUNT`, and `GCOUNT`
keywords. Trailing non-extension blocks are preserved as raw
`FitsFile::special_records`, one 2880-byte block per entry, and their first
eight bytes are validated not to contain `XTENSION`.

For large files, use the streaming `FitsReader`: `next_hdu()` reads only the
header as an index, while `read_data_to()` streams the corresponding Data Unit
to a writer and `skip_data()` discards it without allocating the full Data Unit.
The `read_data()` method is available when an in-memory Data Unit is explicitly
needed. `FitsFile::from_bytes()` remains a convenience API for already-loaded
byte slices. The streaming reader treats an incomplete trailing physical block
after the last complete FITS structure as media padding and discards it.

Data Units can also be decoded incrementally with `read_data_values()`. The
decoder handles FITS big-endian signed integers and IEEE-754 values according
to `BITPIX`; `BITPIX=8` is exposed as raw bytes because FITS uses it for both
unsigned byte arrays and ASCII character data.

Header Values are exposed as `Value::String`, `Value::Logical`, `Value::Integer`,
`Value::Real`, `Value::ComplexInteger`, `Value::ComplexReal`, or
`Value::Undefined`. Quoted strings decode doubled single quotes, and
`Header::string_value()` reconstructs FITS 4.0 `CONTINUE` long-string records.
FITS dates are not a separate value datatype: ISO-8601 datetime strings can be
validated with `Value::as_datetime()`, while physical units can be inspected as
restricted-ASCII strings with `Value::as_unit()`.

## Usage

```text
cargo run -- [-v|--verbose] <input-path> [output-path]
```

The app reads and validates the input FITS file, then prints one-line details
for each HDU. When an output path is supplied, it also writes the original
bytes to that path.

Parse only:

```text
cargo run -- input.fits
```

Parse and write a copy:

```text
cargo run -- input.fits output.fits
```

Verbose mode prints every logical 80-character header card for each HDU:

```text
cargo run -- --verbose input.fits
```

For command help:

```text
cargo run -- --help
```

## Project layout

```text
src/
	lib.rs              Public library API
	main.rs             Command-line application and file I/O
	fits/
		mod.rs            FITS module surface
		model.rs          FITS data models
		parser.rs         Header and HDU parsing
		error.rs          Parser error types
tests/
	fits_parser.rs      Public API integration tests
demoData/             Local, Git-ignored sample files
```

## Development

```text
cargo check
cargo fmt -- --check
cargo test
```

## License

This project is licensed under the MIT License. See [LICENSE](LICENSE) for
the full license text.