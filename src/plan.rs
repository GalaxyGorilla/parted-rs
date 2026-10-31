use crate::device::Device;
use crate::gpt::{Gpt, GptEntry};
use crate::mbr::{Mbr, MbrEntry};
use crate::model::{LabelKind, LbaRange};
use crate::util::{parse_guid, unique_guid};
use crate::{Error, Result};

#[derive(Clone, Debug)]
pub struct Write {
    pub(crate) lba: u64,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Plan {
    operation: String,
    label: LabelKind,
    pub(crate) writes: Vec<Write>,
    expected: Vec<Write>,
    target_size: u64,
    block_size: u32,
    identity: (u64, u64),
}

impl Plan {
    pub fn operation(&self) -> &str {
        &self.operation
    }

    pub fn label(&self) -> LabelKind {
        self.label
    }

    pub fn touched_ranges(&self, block_size: u32) -> Result<Vec<LbaRange>> {
        if block_size != self.block_size {
            return Err(Error::Invalid(
                "plan block size does not match target".into(),
            ));
        }
        let mut ranges = Vec::new();
        for write in &self.writes {
            if write.bytes.is_empty() || !write.bytes.len().is_multiple_of(block_size as usize) {
                return Err(Error::Invalid("planned write is not block aligned".into()));
            }
            let blocks = write.bytes.len() as u64 / u64::from(block_size);
            ranges.push(LbaRange {
                first: write.lba,
                last: write
                    .lba
                    .checked_add(blocks - 1)
                    .ok_or_else(|| Error::Invalid("planned write range overflow".into()))?,
            });
        }
        Ok(ranges)
    }

    pub(crate) fn validate_target(&self, device: &Device) -> Result<()> {
        if device.size() != self.target_size
            || device.block_size() != self.block_size
            || device.identity()? != self.identity
        {
            return Err(Error::Refused(
                "planned target identity or geometry changed".into(),
            ));
        }
        for expected in &self.expected {
            let blocks = expected.bytes.len() as u64 / u64::from(self.block_size);
            if device.read_lba(expected.lba, blocks)? != expected.bytes {
                return Err(Error::Refused(
                    "partition metadata changed after planning; refusing stale plan".into(),
                ));
            }
        }
        Ok(())
    }
}

pub fn mklabel(device: &Device, kind: LabelKind) -> Result<Plan> {
    match kind {
        LabelKind::Mbr => {
            let mut sector = device.read_lba(0, 1)?;
            // Keep bootstrap code and disk identity while replacing only the table.
            sector[446..510].fill(0);
            sector[510..512].copy_from_slice(&[0x55, 0xaa]);
            let mut writes = Vec::new();
            if device.blocks() > 2 && &device.read_lba(1, 1)?[..8] == b"EFI PART" {
                writes.extend(
                    Gpt::erase_writes(device)?
                        .into_iter()
                        .map(|(lba, bytes)| Write { lba, bytes }),
                );
            }
            writes.push(Write {
                lba: 0,
                bytes: sector,
            });
            finalize_plan(device, "create MBR label", kind, writes)
        }
        LabelKind::Gpt => gpt_plan(
            device,
            "create GPT label",
            Gpt::empty(device.block_size(), device.blocks())?,
        ),
    }
}

pub fn mkpart(
    device: &Device,
    range: LbaRange,
    type_id: Option<&str>,
    name: Option<&str>,
) -> Result<Plan> {
    match detect(device)? {
        Table::Mbr(mut mbr) => {
            if name.is_some() {
                return Err(Error::Invalid(
                    "MBR primary partitions do not support names".into(),
                ));
            }
            let slot = mbr
                .entries
                .iter()
                .position(Option::is_none)
                .ok_or_else(|| Error::Invalid("MBR has no free primary entry".into()))?;
            if range.last > u64::from(u32::MAX) {
                return Err(Error::Invalid(
                    "MBR end exceeds the 32-bit LBA address space".into(),
                ));
            }
            let first_lba = u32::try_from(range.first)
                .map_err(|_| Error::Invalid("MBR start exceeds 32-bit LBA".into()))?;
            let sectors = u32::try_from(range.blocks()?)
                .map_err(|_| Error::Invalid("MBR length exceeds 32-bit sector count".into()))?;
            let partition_type = match type_id {
                Some(text) => u8::from_str_radix(text.trim_start_matches("0x"), 16)
                    .map_err(|_| Error::Invalid("MBR type must be a hex byte".into()))?,
                None => 0x83,
            };
            mbr.entries[slot] = Some(MbrEntry {
                bootable: false,
                partition_type,
                first_lba,
                sectors,
                chs_first: [0xff; 3],
                chs_last: [0xff; 3],
            });
            mbr_plan(device, format!("create partition {}", slot + 1), &mbr)
        }
        Table::Gpt(mut gpt) => {
            let slot = gpt
                .entries
                .iter()
                .position(Option::is_none)
                .ok_or_else(|| Error::Invalid("GPT has no free entry".into()))?;
            let guid = parse_guid(type_id.unwrap_or("0fc63daf-8483-4772-8e79-3d69d8477de4"))?;
            gpt.entries[slot] = Some(GptEntry {
                type_guid: guid,
                unique_guid: unique_guid()?,
                first_lba: range.first,
                last_lba: range.last,
                attributes: 0,
                name: name.unwrap_or("").into(),
            });
            gpt_plan(device, format!("create partition {}", slot + 1), gpt)
        }
    }
}

pub fn rm(device: &Device, number: u32) -> Result<Plan> {
    let index = number
        .checked_sub(1)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| Error::Invalid("partition numbers start at 1".into()))?;
    match detect(device)? {
        Table::Mbr(mut mbr) => {
            let slot = mbr
                .entries
                .get_mut(index)
                .ok_or_else(|| Error::Invalid("MBR partition number must be 1..4".into()))?;
            if slot.take().is_none() {
                return Err(Error::Invalid(format!("partition {number} does not exist")));
            }
            mbr_plan(device, format!("remove partition {number}"), &mbr)
        }
        Table::Gpt(mut gpt) => {
            let slot = gpt
                .entries
                .get_mut(index)
                .ok_or_else(|| Error::Invalid("GPT partition number is out of range".into()))?;
            if slot.take().is_none() {
                return Err(Error::Invalid(format!("partition {number} does not exist")));
            }
            gpt_plan(device, format!("remove partition {number}"), gpt)
        }
    }
}

pub fn resizepart(device: &Device, number: u32, new_last: u64) -> Result<Plan> {
    let index = number
        .checked_sub(1)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| Error::Invalid("partition numbers start at 1".into()))?;
    match detect(device)? {
        Table::Mbr(mut mbr) => {
            let entry = mbr
                .entries
                .get_mut(index)
                .and_then(Option::as_mut)
                .ok_or_else(|| Error::Invalid(format!("partition {number} does not exist")))?;
            if new_last < u64::from(entry.first_lba) {
                return Err(Error::Invalid("new end is before partition start".into()));
            }
            if new_last > u64::from(u32::MAX) {
                return Err(Error::Invalid(
                    "MBR end exceeds the 32-bit LBA address space".into(),
                ));
            }
            entry.sectors = u32::try_from(new_last - u64::from(entry.first_lba) + 1)
                .map_err(|_| Error::Invalid("MBR length exceeds 32-bit sector count".into()))?;
            mbr_plan(device, format!("resize partition {number}"), &mbr)
        }
        Table::Gpt(mut gpt) => {
            let entry = gpt
                .entries
                .get_mut(index)
                .and_then(Option::as_mut)
                .ok_or_else(|| Error::Invalid(format!("partition {number} does not exist")))?;
            if new_last < entry.first_lba {
                return Err(Error::Invalid("new end is before partition start".into()));
            }
            entry.last_lba = new_last;
            gpt_plan(device, format!("resize partition {number}"), gpt)
        }
    }
}

pub fn name(device: &Device, number: u32, value: &str) -> Result<Plan> {
    let index = partition_index(number)?;
    match detect(device)? {
        Table::Mbr(_) => Err(Error::Invalid("MBR partitions do not support names".into())),
        Table::Gpt(mut gpt) => {
            let entry = gpt
                .entries
                .get_mut(index)
                .and_then(Option::as_mut)
                .ok_or_else(|| Error::Invalid(format!("partition {number} does not exist")))?;
            entry.name = value.into();
            gpt_plan(device, format!("name partition {number}"), gpt)
        }
    }
}

pub fn set_type(device: &Device, number: u32, value: &str) -> Result<Plan> {
    let index = partition_index(number)?;
    match detect(device)? {
        Table::Mbr(mut mbr) => {
            let entry = mbr
                .entries
                .get_mut(index)
                .and_then(Option::as_mut)
                .ok_or_else(|| Error::Invalid(format!("partition {number} does not exist")))?;
            entry.partition_type = u8::from_str_radix(value.trim_start_matches("0x"), 16)
                .map_err(|_| Error::Invalid("MBR type must be a hex byte".into()))?;
            mbr_plan(device, format!("set partition {number} type"), &mbr)
        }
        Table::Gpt(mut gpt) => {
            let entry = gpt
                .entries
                .get_mut(index)
                .and_then(Option::as_mut)
                .ok_or_else(|| Error::Invalid(format!("partition {number} does not exist")))?;
            entry.type_guid = parse_guid(value)?;
            gpt_plan(device, format!("set partition {number} type"), gpt)
        }
    }
}

pub fn set_boot(device: &Device, number: u32, enabled: bool) -> Result<Plan> {
    let index = partition_index(number)?;
    match detect(device)? {
        Table::Mbr(mut mbr) => {
            let entry = mbr
                .entries
                .get_mut(index)
                .and_then(Option::as_mut)
                .ok_or_else(|| Error::Invalid(format!("partition {number} does not exist")))?;
            entry.bootable = enabled;
            mbr_plan(device, format!("set partition {number} boot flag"), &mbr)
        }
        Table::Gpt(_) => Err(Error::Invalid(
            "the MBR boot flag is not defined for GPT; use set NUMBER attribute BIT on|off".into(),
        )),
    }
}

pub fn set_attribute(device: &Device, number: u32, bit: u8, enabled: bool) -> Result<Plan> {
    if bit > 63 {
        return Err(Error::Invalid("GPT attribute bit must be 0..63".into()));
    }
    let index = partition_index(number)?;
    match detect(device)? {
        Table::Mbr(_) => Err(Error::Invalid(
            "GPT attributes are not defined for MBR partitions".into(),
        )),
        Table::Gpt(mut gpt) => {
            let entry = gpt
                .entries
                .get_mut(index)
                .and_then(Option::as_mut)
                .ok_or_else(|| Error::Invalid(format!("partition {number} does not exist")))?;
            let mask = 1_u64 << bit;
            if enabled {
                entry.attributes |= mask;
            } else {
                entry.attributes &= !mask;
            }
            gpt_plan(
                device,
                format!(
                    "{} GPT attribute bit {bit} on partition {number}",
                    if enabled { "set" } else { "clear" }
                ),
                gpt,
            )
        }
    }
}

fn partition_index(number: u32) -> Result<usize> {
    number
        .checked_sub(1)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| Error::Invalid("partition numbers start at 1".into()))
}

pub fn read_model(device: &Device) -> Result<crate::model::Disk> {
    match detect(device)? {
        Table::Mbr(mbr) => Ok(mbr.model(device.block_size(), device.blocks())),
        Table::Gpt(gpt) => Ok(gpt.model()),
    }
}

enum Table {
    Mbr(Box<Mbr>),
    Gpt(Gpt),
}

fn detect(device: &Device) -> Result<Table> {
    let header = device.read_lba(1, 1)?;
    if &header[..8] == b"EFI PART" {
        Ok(Table::Gpt(Gpt::read(device)?))
    } else {
        let mbr = Mbr::read(device)?;
        if mbr
            .entries
            .iter()
            .flatten()
            .any(|entry| entry.partition_type == 0xee)
        {
            return Err(Error::Invalid(
                "protective MBR exists without a valid GPT; refusing MBR edits".into(),
            ));
        }
        Ok(Table::Mbr(Box::new(mbr)))
    }
}

fn mbr_plan(device: &Device, operation: String, mbr: &Mbr) -> Result<Plan> {
    let mut sector = device.read_lba(0, 1)?;
    sector[..512].copy_from_slice(&mbr.bytes(device.blocks())?);
    finalize_plan(
        device,
        operation,
        LabelKind::Mbr,
        vec![Write {
            lba: 0,
            bytes: sector,
        }],
    )
}

fn gpt_plan(device: &Device, operation: impl Into<String>, gpt: Gpt) -> Result<Plan> {
    let writes = gpt
        .writes()?
        .into_iter()
        .map(|(lba, bytes)| Write { lba, bytes })
        .collect();
    finalize_plan(device, operation, LabelKind::Gpt, writes)
}

fn finalize_plan(
    device: &Device,
    operation: impl Into<String>,
    label: LabelKind,
    writes: Vec<Write>,
) -> Result<Plan> {
    let mut expected = Vec::with_capacity(writes.len());
    for write in &writes {
        if write.bytes.is_empty()
            || !write
                .bytes
                .len()
                .is_multiple_of(device.block_size() as usize)
        {
            return Err(Error::Invalid("planned write is not block aligned".into()));
        }
        let blocks = write.bytes.len() as u64 / u64::from(device.block_size());
        expected.push(Write {
            lba: write.lba,
            bytes: device.read_lba(write.lba, blocks)?,
        });
    }
    Ok(Plan {
        operation: operation.into(),
        label,
        writes,
        expected,
        target_size: device.size(),
        block_size: device.block_size(),
        identity: device.identity()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(device: &Device, plan: &Plan) {
        for write in &plan.writes {
            device.write_lba(write.lba, &write.bytes).unwrap();
        }
        device.sync().unwrap();
    }

    #[test]
    fn plans_do_not_write() {
        let path = std::env::temp_dir().join(format!("parted-plan-{}", std::process::id()));
        std::fs::write(&path, vec![0xa5; 1024 * 1024]).unwrap();
        let before = std::fs::read(&path).unwrap();
        let device = Device::open(&path, false, None).unwrap();
        let plan = mklabel(&device, LabelKind::Gpt).unwrap();
        assert!(!plan.writes.is_empty());
        drop(device);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn mbr_create_add_resize_remove() {
        let path = std::env::temp_dir().join(format!("parted-mbr-plan-{}", std::process::id()));
        std::fs::write(&path, vec![0; 1024 * 1024]).unwrap();
        let device = Device::open(&path, true, None).unwrap();
        apply(&device, &mklabel(&device, LabelKind::Mbr).unwrap());
        apply(
            &device,
            &mkpart(
                &device,
                LbaRange {
                    first: 8,
                    last: 100,
                },
                Some("83"),
                None,
            )
            .unwrap(),
        );
        assert_eq!(read_model(&device).unwrap().partitions[0].range.last, 100);
        apply(&device, &set_boot(&device, 1, true).unwrap());
        apply(&device, &set_type(&device, 1, "0c").unwrap());
        let model = read_model(&device).unwrap();
        assert!(model.partitions[0].bootable);
        assert_eq!(model.partitions[0].type_id, "0c");
        apply(&device, &resizepart(&device, 1, 120).unwrap());
        assert_eq!(read_model(&device).unwrap().partitions[0].range.last, 120);
        apply(&device, &rm(&device, 1).unwrap());
        assert!(read_model(&device).unwrap().partitions.is_empty());
        drop(device);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn gpt_create_add_resize_remove_and_reject_bounds() {
        let path = std::env::temp_dir().join(format!("parted-gpt-plan-{}", std::process::id()));
        std::fs::write(&path, vec![0; 4 * 1024 * 1024]).unwrap();
        let device = Device::open(&path, true, None).unwrap();
        apply(&device, &mklabel(&device, LabelKind::Gpt).unwrap());
        let model = read_model(&device).unwrap();
        let start = model.usable.first;
        apply(
            &device,
            &mkpart(
                &device,
                LbaRange {
                    first: start,
                    last: start + 99,
                },
                None,
                Some("data"),
            )
            .unwrap(),
        );
        assert_eq!(read_model(&device).unwrap().partitions[0].name, "data");
        apply(&device, &name(&device, 1, "renamed").unwrap());
        let type_guid = "21686148-6449-6e6f-744e-656564454649";
        apply(&device, &set_type(&device, 1, type_guid).unwrap());
        let model = read_model(&device).unwrap();
        assert_eq!(model.partitions[0].name, "renamed");
        assert_eq!(model.partitions[0].type_id, type_guid);
        assert!(set_boot(&device, 1, true).is_err());
        apply(&device, &set_attribute(&device, 1, 60, true).unwrap());
        assert_eq!(
            read_model(&device).unwrap().partitions[0].attributes,
            1_u64 << 60
        );
        apply(&device, &set_attribute(&device, 1, 60, false).unwrap());
        assert_eq!(read_model(&device).unwrap().partitions[0].attributes, 0);
        assert!(set_attribute(&device, 1, 64, true).is_err());
        apply(&device, &resizepart(&device, 1, start + 199).unwrap());
        assert!(
            mkpart(
                &device,
                LbaRange {
                    first: start + 100,
                    last: start + 300,
                },
                None,
                None
            )
            .is_err()
        );
        apply(&device, &rm(&device, 1).unwrap());
        assert!(read_model(&device).unwrap().partitions.is_empty());
        drop(device);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn refuses_to_treat_orphaned_protective_mbr_as_mbr() {
        let path = std::env::temp_dir().join(format!("parted-protective-{}", std::process::id()));
        std::fs::write(&path, vec![0; 4 * 1024 * 1024]).unwrap();
        let device = Device::open(&path, true, None).unwrap();
        apply(&device, &mklabel(&device, LabelKind::Gpt).unwrap());
        device
            .write_lba(1, &vec![0; device.block_size() as usize])
            .unwrap();

        let error = read_model(&device).unwrap_err();
        assert!(error.to_string().contains("protective MBR"));

        drop(device);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn gpt_to_mbr_erases_both_gpt_metadata_copies() {
        let path = std::env::temp_dir().join(format!("parted-convert-{}", std::process::id()));
        std::fs::write(&path, vec![0; 4 * 1024 * 1024]).unwrap();
        let device = Device::open(&path, true, None).unwrap();
        apply(&device, &mklabel(&device, LabelKind::Gpt).unwrap());
        let model = read_model(&device).unwrap();
        apply(
            &device,
            &mkpart(
                &device,
                LbaRange {
                    first: model.usable.first,
                    last: model.usable.first + 99,
                },
                None,
                Some("data"),
            )
            .unwrap(),
        );

        apply(&device, &mklabel(&device, LabelKind::Mbr).unwrap());
        assert_eq!(read_model(&device).unwrap().kind, LabelKind::Mbr);
        assert!(
            device
                .read_lba(1, 33)
                .unwrap()
                .iter()
                .all(|byte| *byte == 0)
        );
        assert!(
            device
                .read_lba(device.blocks() - 33, 33)
                .unwrap()
                .iter()
                .all(|byte| *byte == 0)
        );

        drop(device);
        std::fs::remove_file(path).unwrap();
    }
}
