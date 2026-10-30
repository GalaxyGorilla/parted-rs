use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn temp_path(extension: &str) -> PathBuf {
    let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "parted-cli-{}-{sequence}.{extension}",
        std::process::id()
    ))
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_parted"))
        .args(args)
        .output()
        .unwrap()
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn reports_version_without_a_device() {
    let output = run(&["--version"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("parted {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn json_write_print_dry_run_and_restore_are_machine_readable() {
    let image = temp_path("img");
    let label_backup = temp_path("label.bak");
    let part_backup = temp_path("part.bak");
    std::fs::write(&image, vec![0; 8 * 1024 * 1024]).unwrap();

    let output = run(&[
        "--script",
        "--json",
        "--backup",
        path(&label_backup),
        path(&image),
        "mklabel",
        "gpt",
    ]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    assert!(stdout.starts_with("{\"status\":\"committed\""));
    assert!(stdout.contains(path(&label_backup)));

    let output = run(&["--json", path(&image), "print"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    assert!(stdout.contains("\"label\":\"gpt\""));

    let before = std::fs::read(&image).unwrap();
    let output = run(&[
        "--json",
        "--dry-run",
        path(&image),
        "mkpart",
        "1MiB",
        "4MiB",
        "--name",
        "data",
    ]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    assert!(stdout.contains("\"operation\":\"create partition 1\""));
    assert_eq!(std::fs::read(&image).unwrap(), before);

    let output = run(&[
        "--script",
        "--backup",
        path(&part_backup),
        path(&image),
        "mkpart",
        "1MiB",
        "4MiB",
    ]);
    assert!(output.status.success(), "{output:?}");

    let output = run(&["--json", path(&image), "print"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("\"first_lba\":2048,\"last_lba\":8191")
    );

    let output = run(&[
        "--script",
        "--json",
        path(&image),
        "restore",
        path(&part_backup),
    ]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    assert!(stdout.starts_with("{\"status\":\"restored\""));
    assert_eq!(std::fs::read(&image).unwrap(), before);

    std::fs::remove_file(image).unwrap();
    std::fs::remove_file(label_backup).unwrap();
    std::fs::remove_file(part_backup).unwrap();
}

#[test]
fn four_kib_image_sector_size_roundtrips() {
    let image = temp_path("4k.img");
    let backup = temp_path("4k.bak");
    std::fs::write(&image, vec![0; 16 * 1024 * 1024]).unwrap();

    let output = run(&[
        "--script",
        "--json",
        "--sector-size",
        "4096",
        "--backup",
        path(&backup),
        path(&image),
        "mklabel",
        "gpt",
    ]);
    assert!(output.status.success(), "{output:?}");

    let output = run(&["--json", "--sector-size", "4096", path(&image), "print"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("\"logical_block_size\":4096")
    );

    std::fs::remove_file(image).unwrap();
    std::fs::remove_file(backup).unwrap();
}

#[test]
fn rejects_duplicate_options_and_mbr_partition_names() {
    let duplicate = run(&["--json", "--json", "unused", "print"]);
    assert!(!duplicate.status.success());
    assert!(
        String::from_utf8(duplicate.stderr)
            .unwrap()
            .contains("duplicate --json")
    );

    let image = temp_path("mbr.img");
    let label_backup = temp_path("mbr-label.bak");
    std::fs::write(&image, vec![0; 8 * 1024 * 1024]).unwrap();
    let output = run(&[
        "--script",
        "--backup",
        path(&label_backup),
        path(&image),
        "mklabel",
        "mbr",
    ]);
    assert!(output.status.success(), "{output:?}");

    let output = run(&[
        "--script",
        path(&image),
        "mkpart",
        "2048",
        "4095",
        "--name",
        "unsupported",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("do not support names")
    );

    std::fs::remove_file(image).unwrap();
    std::fs::remove_file(label_backup).unwrap();
}
