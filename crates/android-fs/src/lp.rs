//! Android's dynamic partitions, as liblp lays them out: a `super` image whose metadata
//! maps each logical partition onto a range of the device. Partitions are built straight
//! into their place, one after another, and the metadata is written at the front last.
//!
//! The layout is `lpmake`'s for a single block device: 4 KiB reserved, the geometry
//! twice, then a primary and a backup copy of every slot's metadata, and partitions from
//! the next 1 MiB boundary on, each starting at the next one after the last.

use crate::{Error, Region};
use android_firmware::{Name, Sha256Digest, SlotScheme, SuperLayout};
use std::{
    fs::{self, File},
    os::unix::fs::FileExt,
    path::Path,
};

const SECTOR: u64 = 512;
/// The bytes at the start of the device that liblp leaves alone.
const RESERVED: u64 = 4096;
const GEOMETRY_SIZE: u64 = 4096;
const BLOCK_SIZE: u64 = 4096;
/// Where partitions start: liblp's default alignment, which `lpmake` uses.
const ALIGNMENT: u64 = 1 << 20;

const GEOMETRY_MAGIC: u32 = 0x616c_4467;
const HEADER_MAGIC: u32 = 0x414c_5030;
const MAJOR_VERSION: u16 = 10;
/// Header 1.2 adds the flags; it is only written when a flag is set.
const EXPANDED_MINOR_VERSION: u16 = 2;
const VIRTUAL_AB_DEVICE: u32 = 1;
const READONLY: u32 = 1;
const NAME_LEN: usize = 36;
const PARTITION_SIZE: u32 = 52;
const EXTENT_SIZE: u32 = 24;
const GROUP_SIZE: u32 = 48;
const BLOCK_DEVICE_SIZE: u32 = 64;

/// A `super` image being built.
pub struct SuperImage {
    file: File,
    layout: SuperLayout,
    /// Where the next partition goes.
    next: u64,
    /// Each partition built so far, with its start and size in bytes.
    partitions: Vec<(Name, u64, u64)>,
    /// What those partitions take of their group.
    used: u64,
}

impl SuperImage {
    pub fn create(path: &Path, layout: SuperLayout) -> Result<Self, Error> {
        if !u64::from(layout.metadata_size).is_multiple_of(BLOCK_SIZE) {
            return Err(invalid(format!(
                "metadata size {} is not a multiple of {BLOCK_SIZE}",
                layout.metadata_size
            )));
        }
        let next = first_data_byte(&layout);
        if next + BLOCK_SIZE > layout.size {
            return Err(invalid(format!(
                "a {}-byte device has no room after its metadata",
                layout.size
            )));
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        Ok(Self {
            file,
            layout,
            next,
            partitions: Vec::new(),
            used: 0,
        })
    }

    /// Where the next partition is built.
    pub fn region(&self) -> Result<Region, Error> {
        Ok(Region::new(self.file.try_clone()?, self.next))
    }

    /// Record the partition just built in [`Self::region`], `size` bytes long, if it
    /// fits in the device and in its group.
    pub fn add(&mut self, name: Name, size: u64) -> Result<(), Error> {
        let size = size.next_multiple_of(BLOCK_SIZE);
        if self.next + size > self.layout.size {
            return Err(invalid(format!(
                "{name} needs {size} bytes from byte {}, past the {}-byte device",
                self.next, self.layout.size
            )));
        }
        let capacity = self.layout.group_capacity();
        if self.used + size > capacity {
            return Err(invalid(format!(
                "{name} needs {size} bytes, but group {} has {} of its {capacity} left",
                self.layout.group,
                capacity - self.used
            )));
        }
        self.used += size;
        self.partitions.push((name, self.next, size));
        self.next = (self.next + size).next_multiple_of(ALIGNMENT);
        Ok(())
    }

    /// Write the geometry and every copy of the metadata, and give the image the
    /// device's size; the space no partition uses stays a hole.
    pub fn finish(self) -> Result<(), Error> {
        let metadata = self.metadata()?;
        let max_size = metadata_max_size(&self.layout);
        if metadata.len() as u64 > max_size {
            return Err(invalid(format!(
                "{} bytes of metadata exceed the {max_size} evelyn.toml allows",
                metadata.len()
            )));
        }
        self.file.set_len(self.layout.size)?;
        let geometry = self.geometry();
        self.file.write_all_at(&geometry, RESERVED)?;
        self.file
            .write_all_at(&geometry, RESERVED + GEOMETRY_SIZE)?;
        let slots = u64::from(self.layout.metadata_slots());
        let primary = RESERVED + 2 * GEOMETRY_SIZE;
        for slot in 0..slots {
            self.file
                .write_all_at(&metadata, primary + max_size * slot)?;
            self.file
                .write_all_at(&metadata, primary + max_size * (slots + slot))?;
        }
        Ok(())
    }

    fn geometry(&self) -> Vec<u8> {
        let mut geometry = Vec::with_capacity(GEOMETRY_SIZE as usize);
        geometry.extend(GEOMETRY_MAGIC.to_le_bytes());
        geometry.extend(52_u32.to_le_bytes());
        geometry.extend([0; 32]);
        geometry.extend((metadata_max_size(&self.layout) as u32).to_le_bytes());
        geometry.extend(self.layout.metadata_slots().to_le_bytes());
        geometry.extend((BLOCK_SIZE as u32).to_le_bytes());
        let checksum = Sha256Digest::bytes(&geometry);
        geometry[8..40].copy_from_slice(checksum.as_bytes());
        geometry.resize(GEOMETRY_SIZE as usize, 0);
        geometry
    }

    /// One slot's metadata, the same in every slot. The first slot's partitions hold
    /// the images; on A/B, the second slot's are declared empty.
    fn metadata(&self) -> Result<Vec<u8>, Error> {
        let suffixes: &[&str] = match self.layout.slots {
            SlotScheme::Single => &[""],
            SlotScheme::Ab | SlotScheme::VirtualAb => &["_a", "_b"],
        };
        let (mut partitions, mut extents, mut groups) = (Vec::new(), Vec::new(), Vec::new());
        group(&mut groups, "default", 0)?;
        for (slot, suffix) in suffixes.iter().enumerate() {
            let group_name = format!("{}{suffix}", self.layout.group);
            group(&mut groups, &group_name, self.layout.group_capacity())?;
            for (name, start, size) in &self.partitions {
                let first_extent = extents.len() as u32 / EXTENT_SIZE;
                let filled = slot == 0 && *size > 0;
                if filled {
                    extents.extend((size / SECTOR).to_le_bytes());
                    extents.extend(0_u32.to_le_bytes()); // linear
                    extents.extend((start / SECTOR).to_le_bytes());
                    extents.extend(0_u32.to_le_bytes()); // on the one block device
                }
                partitions.extend(fixed(&format!("{name}{suffix}"))?);
                partitions.extend(READONLY.to_le_bytes());
                partitions.extend(first_extent.to_le_bytes());
                partitions.extend(u32::from(filled).to_le_bytes());
                partitions.extend((slot as u32 + 1).to_le_bytes());
            }
        }
        let mut block_devices = Vec::new();
        block_devices.extend((first_data_byte(&self.layout) / SECTOR).to_le_bytes());
        block_devices.extend((ALIGNMENT as u32).to_le_bytes());
        block_devices.extend(0_u32.to_le_bytes()); // alignment offset
        block_devices.extend(self.layout.size.to_le_bytes());
        block_devices.extend(fixed("super")?);
        block_devices.extend(0_u32.to_le_bytes()); // flags

        let tables = [partitions, extents, groups, block_devices];
        let sizes = [PARTITION_SIZE, EXTENT_SIZE, GROUP_SIZE, BLOCK_DEVICE_SIZE];
        let virtual_ab = matches!(self.layout.slots, SlotScheme::VirtualAb);
        let (minor, header_size) = if virtual_ab {
            (EXPANDED_MINOR_VERSION, 256)
        } else {
            (0, 128)
        };
        let all_tables = tables.concat();
        let mut header = Vec::with_capacity(header_size);
        header.extend(HEADER_MAGIC.to_le_bytes());
        header.extend(MAJOR_VERSION.to_le_bytes());
        header.extend(minor.to_le_bytes());
        header.extend((header_size as u32).to_le_bytes());
        header.extend([0; 32]);
        header.extend((all_tables.len() as u32).to_le_bytes());
        header.extend(Sha256Digest::bytes(&all_tables).as_bytes());
        let mut offset = 0_u32;
        for (table, entry_size) in tables.iter().zip(sizes) {
            header.extend(offset.to_le_bytes());
            header.extend((table.len() as u32 / entry_size).to_le_bytes());
            header.extend(entry_size.to_le_bytes());
            offset += table.len() as u32;
        }
        if virtual_ab {
            header.extend(VIRTUAL_AB_DEVICE.to_le_bytes());
            header.resize(header_size, 0);
        }
        let checksum = Sha256Digest::bytes(&header);
        header[12..44].copy_from_slice(checksum.as_bytes());
        header.extend(all_tables);
        Ok(header)
    }
}

fn group(table: &mut Vec<u8>, name: &str, maximum_size: u64) -> Result<(), Error> {
    table.extend(fixed(name)?);
    table.extend(0_u32.to_le_bytes()); // flags
    table.extend(maximum_size.to_le_bytes());
    Ok(())
}

/// A name in its fixed field, padded with zeros.
fn fixed(name: &str) -> Result<[u8; NAME_LEN], Error> {
    let mut field = [0; NAME_LEN];
    field
        .get_mut(..name.len())
        .ok_or_else(|| invalid(format!("{name} is longer than {NAME_LEN} bytes")))?
        .copy_from_slice(name.as_bytes());
    Ok(field)
}

fn metadata_max_size(layout: &SuperLayout) -> u64 {
    u64::from(layout.metadata_size)
}

/// The first byte partitions may use: after the reserved bytes, both geometry copies
/// and both copies of every slot's metadata, at the next aligned offset.
fn first_data_byte(layout: &SuperLayout) -> u64 {
    let metadata = metadata_max_size(layout) * u64::from(layout.metadata_slots());
    (RESERVED + 2 * (GEOMETRY_SIZE + metadata)).next_multiple_of(ALIGNMENT)
}

fn invalid(message: String) -> Error {
    Error::Super(message)
}
