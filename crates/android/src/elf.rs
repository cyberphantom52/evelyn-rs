//! What Evelyn reads of 64-bit little-endian ELF files: section names, the
//! libraries a binary needs, and a dynamic symbol's place in the file, found
//! from the program headers alone, which come first: the start of a file is
//! enough.

use crate::Error;

pub struct Elf<'a> {
    data: &'a [u8],
    sections: Vec<Section>,
}

pub struct Section {
    pub name: String,
    kind: u32,
    offset: u64,
    size: u64,
    link: u32,
}

/// Section types.
const NOBITS: u32 = 8;
const DYNAMIC: u32 = 6;
/// Program header types.
const LOAD: u64 = 1;
const PT_DYNAMIC: u64 = 2;
/// Dynamic tags: a needed library, the symbol hash table, the string and
/// symbol tables.
const NEEDED: u64 = 1;
const HASH: u64 = 4;
const STRTAB: u64 = 5;
const SYMTAB: u64 = 6;

impl<'a> Elf<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, Error> {
        if data.get(..6) != Some(b"\x7fELF\x02\x01") {
            return Err(unsupported("not a 64-bit little-endian ELF file"));
        }
        let (offset, size, count, names) = (
            int(data, 0x28, 8)?,
            int(data, 0x3a, 2)?,
            int(data, 0x3c, 2)?,
            int(data, 0x3e, 2)? as usize,
        );
        let mut sections = Vec::new();
        for i in 0..count {
            let at = (offset + i * size) as usize;
            sections.push((
                int(data, at, 4)?,
                Section {
                    name: String::new(),
                    kind: int(data, at + 4, 4)? as u32,
                    offset: int(data, at + 24, 8)?,
                    size: int(data, at + 32, 8)?,
                    link: int(data, at + 40, 4)? as u32,
                },
            ));
        }
        let table = match sections.get(names) {
            Some((_, section)) => bytes(data, section)?.to_vec(),
            None => Vec::new(),
        };
        let sections = sections
            .into_iter()
            .map(|(name, mut section)| {
                section.name = string(&table, name as usize)?;
                Ok(section)
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self { data, sections })
    }

    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// The libraries the file names as needed, in order.
    pub fn needed(&self) -> Result<Vec<String>, Error> {
        let mut needed = Vec::new();
        for table in self.sections.iter().filter(|s| s.kind == DYNAMIC) {
            let strings = self.linked(table)?;
            for entry in bytes(self.data, table)?.as_chunks::<16>().0 {
                if int(entry, 0, 8)? == NEEDED {
                    needed.push(string(strings, int(entry, 8, 8)? as usize)?);
                }
            }
        }
        Ok(needed)
    }

    /// The bytes of the section `section` links to, its string table.
    fn linked(&self, section: &Section) -> Result<&'a [u8], Error> {
        let linked = self
            .sections
            .get(section.link as usize)
            .ok_or_else(|| unsupported("a section linked to no section"))?;
        bytes(self.data, linked)
    }
}

/// Where in the file the dynamic symbol `name` is, if the file defines it and
/// `data`, all of the file or its start, has its dynamic tables.
pub fn dynamic_symbol(data: &[u8], name: &str) -> Result<Option<usize>, Error> {
    if data.get(..6) != Some(b"\x7fELF\x02\x01") {
        return Err(unsupported("not a 64-bit little-endian ELF file"));
    }
    let (offset, size, count) = (
        int(data, 0x20, 8)?,
        int(data, 0x36, 2)?,
        int(data, 0x38, 2)?,
    );
    // (offset, address, size) of each loaded segment, and the dynamic segment.
    let mut loads = Vec::new();
    let mut dynamic = None;
    for i in 0..count {
        let at = (offset + i * size) as usize;
        let segment = (
            int(data, at + 8, 8)?,
            int(data, at + 16, 8)?,
            int(data, at + 32, 8)?,
        );
        match int(data, at, 4)? {
            LOAD => loads.push(segment),
            PT_DYNAMIC => dynamic = Some(segment),
            _ => {}
        }
    }
    let (dynamic, _, length) = dynamic.ok_or_else(|| unsupported("no dynamic segment"))?;
    let place = |address: u64| {
        loads
            .iter()
            .find(|(_, start, size)| (*start..start + size).contains(&address))
            .map(|(offset, start, _)| (offset + address - start) as usize)
            .ok_or_else(|| unsupported("a dynamic table outside the loaded segments"))
    };
    let (mut hash, mut strings, mut symbols) = (None, None, None);
    for entry in 0..length / 16 {
        let at = (dynamic + entry * 16) as usize;
        let value = int(data, at + 8, 8)?;
        match int(data, at, 8)? {
            HASH => hash = Some(place(value)?),
            STRTAB => strings = Some(place(value)?),
            SYMTAB => symbols = Some(place(value)?),
            _ => {}
        }
    }
    let (Some(hash), Some(strings), Some(symbols)) = (hash, strings, symbols) else {
        return Err(unsupported("no dynamic symbol table"));
    };
    let table = data
        .get(strings..)
        .ok_or_else(|| unsupported("truncated ELF file"))?;
    // The hash table's chain count is the number of symbols.
    for i in 0..int(data, hash + 4, 4)? as usize {
        let at = symbols + i * 24;
        if string(table, int(data, at, 4)? as usize)? == name {
            return Ok(Some(place(int(data, at + 8, 8)?)?));
        }
    }
    Ok(None)
}

fn bytes<'a>(data: &'a [u8], section: &Section) -> Result<&'a [u8], Error> {
    if section.kind == NOBITS {
        return Ok(&[]);
    }
    data.get(section.offset as usize..(section.offset + section.size) as usize)
        .ok_or_else(|| unsupported("a section past the end of the file"))
}

/// The little-endian integer of `size` bytes at `at`.
pub(crate) fn int(data: &[u8], at: usize, size: usize) -> Result<u64, Error> {
    let bytes = data
        .get(at..at + size)
        .ok_or_else(|| unsupported("truncated ELF file"))?;
    Ok(bytes
        .iter()
        .rev()
        .fold(0, |value, b| value << 8 | u64::from(*b)))
}

/// The NUL-terminated string at `at`.
fn string(table: &[u8], at: usize) -> Result<String, Error> {
    let rest = table
        .get(at..)
        .ok_or_else(|| unsupported("a name past its string table"))?;
    let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
}

fn unsupported(what: &str) -> Error {
    Error::Unsupported(what.to_owned())
}
