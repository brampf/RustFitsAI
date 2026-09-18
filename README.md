# RustFitsAI

A standalone Rust application boilerplate for reading and writing FITS files.

The FITS reader parses a file into HDUs. Each HDU contains its parsed header
cards and its unpadded data bytes. Headers and data are located according to
the FITS 2880-byte block and 80-byte card rules. Primary HDUs, standard image
and table extensions, and other extension names are processed in file order.
Extension data sizes use the FITS `BITPIX`, `NAXISn`, `PCOUNT`, and `GCOUNT`
keywords. Trailing non-extension blocks are preserved as raw
`FitsFile::special_records`, one 2880-byte block per entry, and their first
eight bytes are validated not to contain `XTENSION`.

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