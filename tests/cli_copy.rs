// SPDX-License-Identifier: MIT

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const OBSERVATION: &[u8] = include_bytes!("fixtures/image.fits");

fn copy(dir: &Path, input: &str, output: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rustfitsai"))
        .current_dir(dir)
        .args([input, output])
        .output()
        .unwrap()
}

#[test]
fn copies_every_byte_including_special_records_and_the_physical_tail() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = OBSERVATION.to_vec();
    bytes.extend_from_slice(&[b' '; rustfitsai::BLOCK_SIZE]);
    bytes.extend_from_slice(b"physical tail");
    fs::write(dir.path().join("input.fits"), &bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            dir.path().join("input.fits"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
    }

    let result = copy(dir.path(), "input.fits", "copy.fits");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(dir.path().join("copy.fits")).unwrap(), bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dir.path().join("copy.fits"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }
    assert!(String::from_utf8_lossy(&result.stdout).contains("Special records: 1 block(s)"));
}

#[test]
fn refuses_existing_destinations_and_preserves_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.fits");
    let output = dir.path().join("output.fits");
    fs::write(&input, OBSERVATION).unwrap();
    fs::write(&output, b"existing contents").unwrap();

    for destination in ["input.fits", "output.fits"] {
        assert!(!copy(dir.path(), "input.fits", destination).status.success());
    }
    assert_eq!(fs::read(input).unwrap(), OBSERVATION);
    assert_eq!(fs::read(output).unwrap(), b"existing contents");
}

#[cfg(unix)]
#[test]
fn refuses_hard_links_and_symbolic_links_including_dangling_links() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.fits");
    fs::write(&input, OBSERVATION).unwrap();
    fs::hard_link(&input, dir.path().join("hard")).unwrap();
    symlink("input.fits", dir.path().join("link")).unwrap();
    symlink("absent", dir.path().join("dangling")).unwrap();

    for destination in ["hard", "link", "dangling"] {
        assert!(!copy(dir.path(), "input.fits", destination).status.success());
    }
    assert_eq!(fs::read(&input).unwrap(), OBSERVATION);
    assert_eq!(fs::read(dir.path().join("hard")).unwrap(), OBSERVATION);
    assert_eq!(
        fs::read_link(dir.path().join("link")).unwrap(),
        Path::new("input.fits")
    );
    assert_eq!(
        fs::read_link(dir.path().join("dangling")).unwrap(),
        Path::new("absent")
    );
    assert!(!dir.path().join("absent").exists());
}

#[test]
fn a_checksum_failure_leaves_no_destination_or_staging_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = OBSERVATION.to_vec();
    bytes[rustfitsai::BLOCK_SIZE] ^= 1;
    fs::write(dir.path().join("input.fits"), &bytes).unwrap();

    let result = copy(dir.path(), "input.fits", "copy.fits");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("DATASUM"));
    assert_eq!(fs::read(dir.path().join("input.fits")).unwrap(), bytes);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn concurrent_copies_publish_only_one_destination() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("input.fits"), OBSERVATION).unwrap();
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| copy(dir.path(), "input.fits", "copy.fits"));
        let second = copy(dir.path(), "input.fits", "copy.fits");
        (first.join().unwrap(), second)
    });
    assert_ne!(first.status.success(), second.status.success());
    assert_eq!(fs::read(dir.path().join("copy.fits")).unwrap(), OBSERVATION);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn applies_the_requested_header_limit_from_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("input.fits"), OBSERVATION).unwrap();
    let run = |limit| {
        Command::new(env!("CARGO_BIN_EXE_rustfitsai"))
            .current_dir(dir.path())
            .args(["--max-header-bytes", limit, "input.fits", "copy.fits"])
            .output()
            .unwrap()
    };

    let smaller = run("2879");
    assert!(!smaller.status.success());
    assert!(String::from_utf8_lossy(&smaller.stderr).contains("2879"));
    assert!(!dir.path().join("copy.fits").exists());
    assert!(run("2880").status.success());
    assert_eq!(fs::read(dir.path().join("copy.fits")).unwrap(), OBSERVATION);
}
