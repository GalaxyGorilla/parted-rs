use parted::device::Device;
use parted::model::{Disk, LabelKind, LbaRange};
use parted::plan::{self, Plan};
use parted::{Error, Result};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Default)]
struct Options {
    script: bool,
    json: bool,
    dry_run: bool,
    unit: Option<String>,
    backup: Option<PathBuf>,
    image_sector: Option<u32>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("parted: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let (options, args) = parse_options(std::env::args().skip(1).collect())?;
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        print!("{}", parted::HELP);
        return Ok(());
    }
    if args == ["--version"] || args == ["-V"] {
        println!("parted {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.len() < 2 {
        return Err(Error::Invalid(
            "expected DEVICE and COMMAND; use --help".into(),
        ));
    }
    let path = Path::new(&args[0]);
    let command = args[1].as_str();
    if command == "restore" {
        if args.len() != 3 {
            return Err(Error::Invalid("usage: parted DEVICE restore BACKUP".into()));
        }
        if options.dry_run {
            return Err(Error::Invalid(
                "--dry-run is not supported for restore".into(),
            ));
        }
        if options.backup.is_some() {
            return Err(Error::Invalid(
                "--backup cannot be combined with restore".into(),
            ));
        }
        confirm(&options, "restore partition-table metadata from backup")?;
        let device = Device::open(path, true, options.image_sector)?;
        let outcome = parted::transaction::restore(&device, Path::new(&args[2]))?;
        if options.json {
            print!(
                "{{\"status\":\"restored\",\"backup\":\"{}\"",
                json_escape(&args[2])
            );
            if let Some(warning) = &outcome.reread_warning {
                print!(",\"kernel_reread_warning\":\"{}\"", json_escape(warning));
            }
            println!("}}");
        } else {
            println!("restored {}", args[2]);
        }
        print_reread_warning(outcome.reread_warning.as_deref());
        return Ok(());
    }

    let mut writable = !matches!(command, "print") && !options.dry_run;
    if !matches!(
        command,
        "print" | "mklabel" | "mkpart" | "rm" | "resizepart" | "name" | "type" | "set"
    ) {
        return Err(Error::Invalid(format!("unknown command: {command}")));
    }
    // Keep dry-run and print genuinely read-only at the OS level.
    if options.dry_run {
        writable = false;
    }
    let device = Device::open(path, writable, options.image_sector)?;
    if command == "print" {
        let include_free = match args.get(2).map(String::as_str) {
            None => false,
            Some("free") if args.len() == 3 => true,
            _ => return Err(Error::Invalid("usage: parted DEVICE print [free]".into())),
        };
        return print_disk(&plan::read_model(&device)?, options.json, include_free);
    }

    let plan = build_plan(&device, &args[1..], options.unit.as_deref().unwrap_or("s"))?;
    if !options.json || options.dry_run {
        print_plan(&plan, device.block_size(), options.json)?;
    } else if !options.script {
        print_plan_stderr(&plan, device.block_size())?;
    }
    if options.dry_run {
        return Ok(());
    }
    confirm(&options, plan.operation())?;
    let backup = options.backup.unwrap_or_else(default_backup_path);
    let outcome = parted::transaction::execute(&device, &plan, &backup).map_err(|error| {
        Error::Invalid(format!(
            "commit failed: {error}; recovery backup path (if created): {}",
            backup.display()
        ))
    })?;
    if options.json {
        print_committed_json(
            &plan,
            device.block_size(),
            &backup,
            outcome.reread_warning.as_deref(),
        )?;
    } else {
        println!("committed; recovery backup: {}", backup.display());
    }
    print_reread_warning(outcome.reread_warning.as_deref());
    Ok(())
}

fn parse_options(args: Vec<String>) -> Result<(Options, Vec<String>)> {
    let mut options = Options::default();
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-s" | "--script" => {
                if options.script {
                    return Err(Error::Invalid("duplicate --script".into()));
                }
                options.script = true;
            }
            "--json" => {
                if options.json {
                    return Err(Error::Invalid("duplicate --json".into()));
                }
                options.json = true;
            }
            "--dry-run" => {
                if options.dry_run {
                    return Err(Error::Invalid("duplicate --dry-run".into()));
                }
                options.dry_run = true;
            }
            "--unit" => {
                if options.unit.is_some() {
                    return Err(Error::Invalid("duplicate --unit".into()));
                }
                i += 1;
                options.unit = Some(
                    args.get(i)
                        .ok_or_else(|| Error::Invalid("--unit requires a value".into()))?
                        .clone(),
                );
            }
            "--backup" => {
                if options.backup.is_some() {
                    return Err(Error::Invalid("duplicate --backup".into()));
                }
                i += 1;
                options.backup =
                    Some(PathBuf::from(args.get(i).ok_or_else(|| {
                        Error::Invalid("--backup requires a path".into())
                    })?));
            }
            "--sector-size" => {
                if options.image_sector.is_some() {
                    return Err(Error::Invalid("duplicate --sector-size".into()));
                }
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| Error::Invalid("--sector-size requires a value".into()))?;
                let size = value
                    .parse()
                    .map_err(|_| Error::Invalid("invalid --sector-size".into()))?;
                if !matches!(size, 512 | 4096) {
                    return Err(Error::Invalid("--sector-size must be 512 or 4096".into()));
                }
                options.image_sector = Some(size);
            }
            value => rest.push(value.to_owned()),
        }
        i += 1;
    }
    Ok((options, rest))
}

fn build_plan(device: &Device, args: &[String], unit: &str) -> Result<Plan> {
    match args.first().map(String::as_str) {
        Some("mklabel") if args.len() == 2 => {
            let kind = match args[1].as_str() {
                "mbr" | "msdos" => LabelKind::Mbr,
                "gpt" => LabelKind::Gpt,
                other => return Err(Error::Invalid(format!("unsupported label: {other}"))),
            };
            plan::mklabel(device, kind)
        }
        Some("mkpart") if args.len() >= 3 => {
            let first = parse_lba(&args[1], unit, device.block_size(), false)?;
            let last = parse_lba(&args[2], unit, device.block_size(), true)?;
            let (type_id, name) = partition_options(&args[3..])?;
            plan::mkpart(
                device,
                LbaRange::new(first, last)?,
                type_id.as_deref(),
                name.as_deref(),
            )
        }
        Some("rm") if args.len() == 2 => plan::rm(device, parse_number(&args[1])?),
        Some("resizepart") if args.len() == 3 => plan::resizepart(
            device,
            parse_number(&args[1])?,
            parse_lba(&args[2], unit, device.block_size(), true)?,
        ),
        Some("name") if args.len() == 3 => plan::name(device, parse_number(&args[1])?, &args[2]),
        Some("type") if args.len() == 3 => {
            plan::set_type(device, parse_number(&args[1])?, &args[2])
        }
        Some("set") if args.len() == 4 && args[2] == "boot" => {
            plan::set_boot(device, parse_number(&args[1])?, parse_switch(&args[3])?)
        }
        _ => Err(Error::Invalid(
            "invalid command arguments; use --help".into(),
        )),
    }
}

fn partition_options(args: &[String]) -> Result<(Option<String>, Option<String>)> {
    let mut type_id = None;
    let mut name = None;
    let mut i = 0;
    while i < args.len() {
        let target = match args[i].as_str() {
            "--type" => &mut type_id,
            "--name" => &mut name,
            other => return Err(Error::Invalid(format!("unknown mkpart option: {other}"))),
        };
        if target.is_some() {
            return Err(Error::Invalid(format!(
                "duplicate mkpart option: {}",
                args[i]
            )));
        }
        i += 1;
        *target = Some(
            args.get(i)
                .ok_or_else(|| Error::Invalid("partition option requires a value".into()))?
                .clone(),
        );
        i += 1;
    }
    Ok((type_id, name))
}

fn parse_number(text: &str) -> Result<u32> {
    text.parse()
        .map_err(|_| Error::Invalid(format!("invalid partition number: {text}")))
}

fn parse_switch(text: &str) -> Result<bool> {
    match text {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(Error::Invalid("switch value must be on or off".into())),
    }
}

fn parse_lba(text: &str, default_unit: &str, block_size: u32, end: bool) -> Result<u64> {
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    if split == 0 {
        return Err(Error::Invalid(format!("invalid exact position: {text}")));
    }
    let value: u64 = text[..split]
        .parse()
        .map_err(|_| Error::Invalid(format!("invalid exact position: {text}")))?;
    let unit = if split == text.len() {
        default_unit
    } else {
        &text[split..]
    };
    if unit == "s" {
        return Ok(value);
    }
    let multiplier = match unit {
        "b" | "B" => 1,
        "KiB" => 1024,
        "MiB" => 1024_u64.pow(2),
        "GiB" => 1024_u64.pow(3),
        "TiB" => 1024_u64.pow(4),
        _ => return Err(Error::Invalid(format!("unsupported exact unit: {unit}"))),
    };
    let bytes = value
        .checked_mul(multiplier)
        .ok_or_else(|| Error::Invalid("position overflows".into()))?;
    if bytes % u64::from(block_size) != 0 {
        return Err(Error::Invalid(format!(
            "{text} is not exactly representable in {block_size}-byte sectors"
        )));
    }
    let lba = bytes / u64::from(block_size);
    if end {
        lba.checked_sub(1)
            .ok_or_else(|| Error::Invalid("end boundary must be after byte zero".into()))
    } else {
        Ok(lba)
    }
}

fn confirm(options: &Options, operation: &str) -> Result<()> {
    if options.script {
        return Ok(());
    }
    eprint!("Apply \"{operation}\"? Type yes to continue: ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if answer.trim() != "yes" {
        return Err(Error::Refused("operation not confirmed".into()));
    }
    Ok(())
}

fn print_plan(plan: &Plan, block_size: u32, json: bool) -> Result<()> {
    let ranges = plan.touched_ranges(block_size)?;
    if json {
        print!(
            "{{\"operation\":\"{}\",\"writes\":[",
            json_escape(plan.operation())
        );
        for (index, range) in ranges.iter().enumerate() {
            if index != 0 {
                print!(",");
            }
            print!(
                "{{\"first_lba\":{},\"last_lba\":{}}}",
                range.first, range.last
            );
        }
        println!("]}}");
    } else {
        println!("Plan: {}", plan.operation());
        for range in ranges {
            println!("  write LBA {}..={}", range.first, range.last);
        }
    }
    Ok(())
}

fn print_plan_stderr(plan: &Plan, block_size: u32) -> Result<()> {
    eprintln!("Plan: {}", plan.operation());
    for range in plan.touched_ranges(block_size)? {
        eprintln!("  write LBA {}..={}", range.first, range.last);
    }
    Ok(())
}

fn print_committed_json(
    plan: &Plan,
    block_size: u32,
    backup: &Path,
    reread_warning: Option<&str>,
) -> Result<()> {
    print!(
        "{{\"status\":\"committed\",\"operation\":\"{}\",\"backup\":\"{}\",\"writes\":[",
        json_escape(plan.operation()),
        json_escape(&backup.to_string_lossy())
    );
    for (index, range) in plan.touched_ranges(block_size)?.iter().enumerate() {
        if index != 0 {
            print!(",");
        }
        print!(
            "{{\"first_lba\":{},\"last_lba\":{}}}",
            range.first, range.last
        );
    }
    print!("]");
    if let Some(warning) = reread_warning {
        print!(",\"kernel_reread_warning\":\"{}\"", json_escape(warning));
    }
    println!("}}");
    Ok(())
}

fn print_reread_warning(warning: Option<&str>) {
    if let Some(warning) = warning {
        eprintln!(
            "parted: warning: on-disk commit succeeded, but the kernel did not reread the table: {warning}"
        );
    }
}

fn print_disk(disk: &Disk, json: bool, include_free: bool) -> Result<()> {
    if json {
        print!(
            "{{\"label\":\"{}\",\"logical_block_size\":{},\"blocks\":{},\"partitions\":[",
            label_name(disk.kind),
            disk.logical_block_size,
            disk.blocks
        );
        for (index, part) in disk.partitions.iter().enumerate() {
            if index != 0 {
                print!(",");
            }
            print!(
                "{{\"number\":{},\"first_lba\":{},\"last_lba\":{},\"type\":\"{}\",\"name\":\"{}\",\"attributes\":{},\"bootable\":{}}}",
                part.number,
                part.range.first,
                part.range.last,
                json_escape(&part.type_id),
                json_escape(&part.name),
                part.attributes,
                part.bootable
            );
        }
        print!("]");
        if include_free {
            print!(",\"free\":[");
            for (index, range) in disk.free_space().iter().enumerate() {
                if index != 0 {
                    print!(",");
                }
                print!(
                    "{{\"first_lba\":{},\"last_lba\":{}}}",
                    range.first, range.last
                );
            }
            print!("]");
        }
        println!("}}");
    } else {
        println!(
            "Label: {}  Sector size: {} B  Sectors: {}",
            label_name(disk.kind),
            disk.logical_block_size,
            disk.blocks
        );
        println!("Number  Start  End  Sectors  Type  Name  Flags");
        for part in &disk.partitions {
            println!(
                "{}  {}  {}  {}  {}  {}  {}",
                part.number,
                part.range.first,
                part.range.last,
                part.range.blocks(),
                part.type_id,
                part.name,
                if part.bootable { "boot" } else { "" }
            );
        }
        if include_free {
            for range in disk.free_space() {
                println!("free  {}  {}  {}", range.first, range.last, range.blocks());
            }
        }
    }
    Ok(())
}

fn label_name(kind: LabelKind) -> &'static str {
    match kind {
        LabelKind::Mbr => "mbr",
        LabelKind::Gpt => "gpt",
    }
}

fn json_escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn default_backup_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    PathBuf::from(format!("parted-backup-{nanos}.bin"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_unit_parser_rejects_fractional_sector() {
        assert_eq!(parse_lba("1MiB", "s", 512, false).unwrap(), 2048);
        assert_eq!(parse_lba("1MiB", "s", 512, true).unwrap(), 2047);
        assert_eq!(parse_lba("2048s", "s", 512, true).unwrap(), 2048);
        assert!(parse_lba("1KiB", "s", 4096, false).is_err());
        assert!(parse_lba("0B", "s", 512, true).is_err());
    }

    #[test]
    fn json_escaping_is_valid_shape() {
        assert_eq!(json_escape("a\"\n"), "a\\\"\\n");
    }
}
