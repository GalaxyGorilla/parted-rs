use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LbaRange {
    pub first: u64,
    pub last: u64,
}

impl LbaRange {
    pub fn new(first: u64, last: u64) -> Result<Self> {
        if first > last {
            return Err(Error::Invalid("range start is after its end".into()));
        }
        Ok(Self { first, last })
    }

    pub fn blocks(self) -> u64 {
        self.last - self.first + 1
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.first <= other.last && other.first <= self.last
    }

    pub fn bytes(self, block_size: u64) -> Result<(u64, u64)> {
        let start = self
            .first
            .checked_mul(block_size)
            .ok_or_else(|| Error::Invalid("byte offset overflow".into()))?;
        let len = self
            .blocks()
            .checked_mul(block_size)
            .ok_or_else(|| Error::Invalid("byte length overflow".into()))?;
        Ok((start, len))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Partition {
    pub number: u32,
    pub range: LbaRange,
    pub type_id: String,
    pub unique_id: Option<String>,
    pub name: String,
    pub attributes: u64,
    pub bootable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabelKind {
    Mbr,
    Gpt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Disk {
    pub kind: LabelKind,
    pub logical_block_size: u32,
    pub blocks: u64,
    pub usable: LbaRange,
    pub partitions: Vec<Partition>,
}

impl Disk {
    pub fn validate(&self) -> Result<()> {
        let mut ranges = Vec::new();
        for part in &self.partitions {
            if part.range.first < self.usable.first || part.range.last > self.usable.last {
                return Err(Error::Invalid(format!(
                    "partition {} is outside the usable device range",
                    part.number
                )));
            }
            if ranges.iter().any(|r: &LbaRange| r.overlaps(part.range)) {
                return Err(Error::Invalid("partition ranges overlap".into()));
            }
            ranges.push(part.range);
        }
        Ok(())
    }

    pub fn free_space(&self) -> Vec<LbaRange> {
        let mut used: Vec<_> = self.partitions.iter().map(|p| p.range).collect();
        used.sort_by_key(|r| r.first);
        let mut next = self.usable.first;
        let mut free = Vec::new();
        for range in used {
            if next < range.first {
                free.push(LbaRange {
                    first: next,
                    last: range.first - 1,
                });
            }
            next = range.last.saturating_add(1);
        }
        if next <= self.usable.last {
            free.push(LbaRange {
                first: next,
                last: self.usable.last,
            });
        }
        free
    }
}

pub fn align_up(value: u64, alignment: u64) -> Result<u64> {
    if alignment == 0 {
        return Err(Error::Invalid("alignment must not be zero".into()));
    }
    value
        .checked_add(alignment - 1)
        .map(|v| v / alignment * alignment)
        .ok_or_else(|| Error::Invalid("alignment overflow".into()))
}

pub fn align_down(value: u64, alignment: u64) -> Result<u64> {
    if alignment == 0 {
        return Err(Error::Invalid("alignment must not be zero".into()));
    }
    Ok(value / alignment * alignment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_free_ranges() {
        let disk = Disk {
            kind: LabelKind::Mbr,
            logical_block_size: 512,
            blocks: 100,
            usable: LbaRange { first: 1, last: 99 },
            partitions: vec![Partition {
                number: 1,
                range: LbaRange {
                    first: 10,
                    last: 19,
                },
                type_id: "83".into(),
                unique_id: None,
                name: String::new(),
                attributes: 0,
                bootable: false,
            }],
        };
        assert_eq!(
            disk.free_space(),
            vec![
                LbaRange { first: 1, last: 9 },
                LbaRange {
                    first: 20,
                    last: 99
                }
            ]
        );
    }

    #[test]
    fn exact_alignment() {
        assert_eq!(align_up(2049, 2048).unwrap(), 4096);
        assert_eq!(align_down(4095, 2048).unwrap(), 2048);
    }
}
