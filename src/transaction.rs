use crate::device::Device;
use crate::model::LbaRange;
use crate::plan::{Plan, Write};
use crate::util::{crc32, le_u32, le_u64};
use crate::{Error, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write as IoWrite};
use std::path::Path;

const MAGIC: &[u8; 8] = b"RSPTBK02";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitOutcome {
    pub reread_warning: Option<String>,
}

pub fn execute(device: &Device, plan: &Plan, backup_path: &Path) -> Result<CommitOutcome> {
    execute_inner(device, plan, backup_path, None)
}

fn execute_inner(
    device: &Device,
    plan: &Plan,
    backup_path: &Path,
    fail_after: Option<usize>,
) -> Result<CommitOutcome> {
    device.ensure_safe_to_write()?;
    plan.validate_target(device)?;
    let records = collect_originals(device, &plan.writes)?;
    write_backup(device, &records, backup_path)?;
    // Confirmation and durable backup creation can take time. Recheck both the
    // live device state and the exact metadata preimage immediately before the
    // first destructive write.
    device.ensure_safe_to_write()?;
    plan.validate_target(device)?;
    for (index, write) in plan.writes.iter().enumerate() {
        if fail_after == Some(index) {
            return Err(Error::Io(std::io::Error::other("injected write failure")));
        }
        device.write_lba(write.lba, &write.bytes)?;
        // Each commit phase reaches stable storage before the next phase.
        device.sync()?;
    }
    verify_writes(device, &plan.writes)?;
    crate::plan::read_model(device)?;
    let reread_warning = device
        .reread_partitions()
        .err()
        .map(|error| error.to_string());
    Ok(CommitOutcome { reread_warning })
}

pub fn restore(device: &Device, backup_path: &Path) -> Result<CommitOutcome> {
    let records = read_backup(device, backup_path)?;
    device.ensure_safe_to_write()?;
    for write in &records {
        device.write_lba(write.lba, &write.bytes)?;
        // Recovery records are ordered secondary-first. Make every phase
        // durable before allowing the next phase to overwrite primary metadata.
        device.sync()?;
        verify_writes(device, std::slice::from_ref(write))?;
    }
    let reread_warning = device
        .reread_partitions()
        .err()
        .map(|error| error.to_string());
    Ok(CommitOutcome { reread_warning })
}

fn collect_originals(device: &Device, writes: &[Write]) -> Result<Vec<Write>> {
    let mut ranges = current_metadata_ranges(device)?;
    for write in writes {
        if write.bytes.is_empty()
            || !write
                .bytes
                .len()
                .is_multiple_of(device.block_size() as usize)
        {
            return Err(Error::Invalid("planned write is not block aligned".into()));
        }
        let blocks = write.bytes.len() as u64 / u64::from(device.block_size());
        let range = LbaRange {
            first: write.lba,
            last: write
                .lba
                .checked_add(blocks - 1)
                .ok_or_else(|| Error::Invalid("planned write range overflow".into()))?,
        };
        if range.last >= device.blocks() {
            return Err(Error::Invalid("planned write is outside the device".into()));
        }
        ranges.push(range);
    }
    ranges.sort_by_key(|range| range.first);
    let mut merged: Vec<LbaRange> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.first <= previous.last.saturating_add(1)
        {
            previous.last = previous.last.max(range.last);
        } else {
            merged.push(range);
        }
    }
    // On GPT, restore the secondary metadata before the primary metadata.
    merged.reverse();
    merged
        .into_iter()
        .map(|range| {
            Ok(Write {
                lba: range.first,
                bytes: device.read_lba(range.first, range.blocks())?,
            })
        })
        .collect()
}

fn current_metadata_ranges(device: &Device) -> Result<Vec<LbaRange>> {
    let primary = device.read_lba(1, 1)?;
    if &primary[..8] != b"EFI PART" {
        return Ok(vec![LbaRange { first: 0, last: 0 }]);
    }
    // Fully validate both copies before trusting their range fields.
    crate::plan::read_model(device)?;
    let count = le_u32(&primary[80..84]);
    let entry_size = le_u32(&primary[84..88]);
    let array_blocks = u64::from(count)
        .checked_mul(u64::from(entry_size))
        .map(|bytes| bytes.div_ceil(u64::from(device.block_size())))
        .ok_or_else(|| Error::Invalid("GPT metadata range overflow".into()))?;
    let primary_array = le_u64(&primary[72..80]);
    let backup_header = le_u64(&primary[32..40]);
    let backup = device.read_lba(backup_header, 1)?;
    let backup_array = le_u64(&backup[72..80]);
    // Recovery order mirrors a normal GPT commit.
    Ok(vec![
        LbaRange {
            first: backup_array,
            last: backup_array + array_blocks - 1,
        },
        LbaRange {
            first: backup_header,
            last: backup_header,
        },
        LbaRange { first: 0, last: 0 },
        LbaRange {
            first: primary_array,
            last: primary_array + array_blocks - 1,
        },
        LbaRange { first: 1, last: 1 },
    ])
}

fn write_backup(device: &Device, records: &[Write], path: &Path) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(MAGIC)?;
    file.write_all(&device.size().to_le_bytes())?;
    file.write_all(&device.block_size().to_le_bytes())?;
    let (identity_kind, identity_value) = device.identity()?;
    file.write_all(&identity_kind.to_le_bytes())?;
    file.write_all(&identity_value.to_le_bytes())?;
    file.write_all(
        &u32::try_from(records.len())
            .map_err(|_| Error::Invalid("too many backup records".into()))?
            .to_le_bytes(),
    )?;
    for record in records {
        file.write_all(&record.lba.to_le_bytes())?;
        let len = u64::try_from(record.bytes.len())
            .map_err(|_| Error::Invalid("backup record is too large".into()))?;
        file.write_all(&len.to_le_bytes())?;
        file.write_all(&crc32(&record.bytes).to_le_bytes())?;
        file.write_all(&record.bytes)?;
    }
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        File::open(if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        })?
        .sync_all()?;
    }
    Ok(())
}

fn read_backup(device: &Device, path: &Path) -> Result<Vec<Write>> {
    let mut file = File::open(path)?;
    let mut magic = [0; 8];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(Error::Invalid("not a parted recovery backup".into()));
    }
    let size = read_u64(&mut file)?;
    let block_size = read_u32(&mut file)?;
    let identity_kind = read_u64(&mut file)?;
    let identity_value = read_u64(&mut file)?;
    let count = read_u32(&mut file)?;
    if size != device.size() || block_size != device.block_size() {
        return Err(Error::Refused(
            "backup device geometry does not match target".into(),
        ));
    }
    if (identity_kind, identity_value) != device.identity()? {
        return Err(Error::Refused(
            "backup belongs to a different device or image file".into(),
        ));
    }
    if count > 1024 {
        return Err(Error::Invalid("backup has too many records".into()));
    }
    let mut records = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let lba = read_u64(&mut file)?;
        let len = read_u64(&mut file)?;
        if len == 0
            || len % u64::from(block_size) != 0
            || len > size
            || lba
                .checked_mul(u64::from(block_size))
                .and_then(|n| n.checked_add(len))
                .is_none_or(|end| end > size)
        {
            return Err(Error::Invalid("invalid backup record range".into()));
        }
        let expected_crc = read_u32(&mut file)?;
        let mut bytes = vec![
            0;
            usize::try_from(len)
                .map_err(|_| Error::Invalid("backup record is too large".into()))?
        ];
        file.read_exact(&mut bytes)?;
        if crc32(&bytes) != expected_crc {
            return Err(Error::Invalid("backup record CRC mismatch".into()));
        }
        let range = LbaRange {
            first: lba,
            last: lba + len / u64::from(block_size) - 1,
        };
        if records.iter().any(|record: &Write| {
            let blocks = record.bytes.len() as u64 / u64::from(block_size);
            LbaRange {
                first: record.lba,
                last: record.lba + blocks - 1,
            }
            .overlaps(range)
        }) {
            return Err(Error::Invalid("backup records overlap".into()));
        }
        records.push(Write { lba, bytes });
    }
    let mut trailing = [0];
    if file.read(&mut trailing)? != 0 {
        return Err(Error::Invalid("backup has trailing data".into()));
    }
    Ok(records)
}

fn verify_writes(device: &Device, writes: &[Write]) -> Result<()> {
    for write in writes {
        let blocks = write.bytes.len() as u64 / u64::from(device.block_size());
        if device.read_lba(write.lba, blocks)? != write.bytes {
            return Err(Error::Invalid(format!(
                "post-write verification failed at LBA {}",
                write.lba
            )));
        }
    }
    Ok(())
}

fn read_u32(reader: &mut impl Read) -> Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> Result<u64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LabelKind;
    use crate::plan;

    #[test]
    fn backup_restores_after_every_injected_write_failure() {
        for fail_after in 0..5 {
            let base =
                std::env::temp_dir().join(format!("parted-tx-{}-{fail_after}", std::process::id()));
            let image = base.with_extension("img");
            let backup = base.with_extension("bak");
            std::fs::write(&image, vec![0; 1024 * 1024]).unwrap();
            let original = std::fs::read(&image).unwrap();
            let read = Device::open(&image, false, None).unwrap();
            let plan = plan::mklabel(&read, LabelKind::Gpt).unwrap();
            drop(read);
            assert_eq!(plan.writes.len(), 5);
            let write = Device::open(&image, true, None).unwrap();
            assert!(execute_inner(&write, &plan, &backup, Some(fail_after)).is_err());
            drop(write);

            let write = Device::open(&image, true, None).unwrap();
            restore(&write, &backup).unwrap();
            drop(write);
            assert_eq!(std::fs::read(&image).unwrap(), original);
            std::fs::remove_file(image).unwrap();
            std::fs::remove_file(backup).unwrap();
        }
    }

    #[test]
    fn backup_refuses_a_different_same_size_image() {
        let base = std::env::temp_dir().join(format!("parted-tx-wrong-{}", std::process::id()));
        let first = base.with_extension("first.img");
        let second = base.with_extension("second.img");
        let backup = base.with_extension("bak");
        std::fs::write(&first, vec![0; 1024 * 1024]).unwrap();
        std::fs::write(&second, vec![0; 1024 * 1024]).unwrap();

        let device = Device::open(&first, true, None).unwrap();
        let plan = plan::mklabel(&device, LabelKind::Gpt).unwrap();
        execute(&device, &plan, &backup).unwrap();
        drop(device);

        let wrong = Device::open(&second, true, None).unwrap();
        let error = restore(&wrong, &backup).unwrap_err();
        assert!(error.to_string().contains("different device"));
        drop(wrong);

        std::fs::remove_file(first).unwrap();
        std::fs::remove_file(second).unwrap();
        std::fs::remove_file(backup).unwrap();
    }

    #[test]
    fn restore_rejects_overlapping_backup_records() {
        let base = std::env::temp_dir().join(format!("parted-tx-overlap-{}", std::process::id()));
        let image = base.with_extension("img");
        let backup = base.with_extension("bak");
        std::fs::write(&image, vec![0; 1024 * 1024]).unwrap();

        let device = Device::open(&image, true, None).unwrap();
        let plan = plan::mklabel(&device, LabelKind::Gpt).unwrap();
        execute(&device, &plan, &backup).unwrap();

        let mut bytes = std::fs::read(&backup).unwrap();
        let first_len = u64::from_le_bytes(bytes[48..56].try_into().unwrap()) as usize;
        let second_lba_offset = 60 + first_len;
        let second_lba = bytes[second_lba_offset..second_lba_offset + 8].to_vec();
        bytes[40..48].copy_from_slice(&second_lba);
        std::fs::write(&backup, bytes).unwrap();

        let error = restore(&device, &backup).unwrap_err();
        assert!(error.to_string().contains("overlap"));
        drop(device);

        std::fs::remove_file(image).unwrap();
        std::fs::remove_file(backup).unwrap();
    }

    #[test]
    fn execute_refuses_metadata_changed_after_planning() {
        let base = std::env::temp_dir().join(format!("parted-tx-stale-{}", std::process::id()));
        let image = base.with_extension("img");
        let backup = base.with_extension("bak");
        std::fs::write(&image, vec![0; 1024 * 1024]).unwrap();

        let device = Device::open(&image, true, None).unwrap();
        let plan = plan::mklabel(&device, LabelKind::Mbr).unwrap();
        device.write_at(0, &[0xa5]).unwrap();
        device.sync().unwrap();

        let error = execute(&device, &plan, &backup).unwrap_err();
        assert!(error.to_string().contains("changed after planning"));
        assert!(!backup.exists());
        drop(device);

        std::fs::remove_file(image).unwrap();
    }
}
