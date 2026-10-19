//! A strict check of a package archive's ZIP structure, on the raw bytes, before any ZIP reader
//! sees them. One archive has one reading: the entries are local file records laid end to end
//! from the first byte, followed by the central directory and an end record with no comment, and
//! every central record matches its local one exactly (name, flags, method, CRC and sizes). There
//! is nothing before, between or after them, so no reader can see a different set of files.
//!
//! Refused outright: duplicate names; directory entries; encryption; data descriptors (sizes must
//! be in the headers); compression; ZIP64; multi-disk archives; archive and file comments; and
//! the Unicode path and comment extra fields (they would replace the name that was checked).
use nodecraft_types::{Error, ErrorKind};
use std::collections::BTreeSet;

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
const LOCAL_LEN: usize = 30;
const CENTRAL_LEN: usize = 46;
const END_LEN: usize = 22;
/// Extra fields that change how an entry reads: ZIP64 sizes, Unicode path and comment.
const REFUSED_EXTRAS: [u16; 3] = [0x0001, 0x7075, 0x6375];

fn bad(detail: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, format!("the package archive is malformed: {detail}"))
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, Error> {
    let b: [u8; 2] = slice(bytes, at, 2)?.try_into().map_err(|_| bad("a header is cut short"))?;
    Ok(u16::from_le_bytes(b))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, Error> {
    let b: [u8; 4] = slice(bytes, at, 4)?.try_into().map_err(|_| bad("a header is cut short"))?;
    Ok(u32::from_le_bytes(b))
}

fn slice(bytes: &[u8], at: usize, len: usize) -> Result<&[u8], Error> {
    bytes.get(at..at.checked_add(len).ok_or_else(|| bad("an offset overflows"))?).ok_or_else(|| bad("a record runs past the end"))
}

fn check_extras(extra: &[u8]) -> Result<(), Error> {
    let mut at = 0;
    while at < extra.len() {
        let id = u16_at(extra, at)?;
        let len = usize::from(u16_at(extra, at + 2)?);
        if REFUSED_EXTRAS.contains(&id) {
            return Err(bad("an entry carries a ZIP64 or Unicode name field"));
        }
        at = at.checked_add(4 + len).ok_or_else(|| bad("an offset overflows"))?;
    }
    if at != extra.len() {
        return Err(bad("an extra field is cut short"));
    }
    Ok(())
}

/// One entry's name and bytes.
pub struct Entry<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
}

/// Checks `bytes`' structure; the entries in order. `max_entries` is checked from the end record,
/// before any entry is read.
pub fn check(bytes: &[u8], max_entries: usize) -> Result<Vec<Entry<'_>>, Error> {
    let end = bytes.len().checked_sub(END_LEN).ok_or_else(|| bad("it is too short to be a ZIP"))?;
    if u32_at(bytes, end)? != END {
        return Err(bad("it doesn't end with a ZIP end record (comments aren't allowed)"));
    }
    let (disk, cd_disk) = (u16_at(bytes, end + 4)?, u16_at(bytes, end + 6)?);
    let (on_disk, count) = (u16_at(bytes, end + 8)?, u16_at(bytes, end + 10)?);
    let (cd_size, cd_offset, comment) = (u32_at(bytes, end + 12)?, u32_at(bytes, end + 16)?, u16_at(bytes, end + 20)?);
    if disk != 0 || cd_disk != 0 || on_disk != count || comment != 0 || count == u16::MAX || cd_size == u32::MAX || cd_offset == u32::MAX {
        return Err(bad("multi-disk, ZIP64 and commented archives aren't allowed"));
    }
    let count = usize::from(count);
    if count > max_entries {
        return Err(Error::new(ErrorKind::InvalidInput, "the package has too many entries"));
    }
    let cd_start = usize::try_from(cd_offset).map_err(|_| bad("an offset overflows"))?;
    let cd_end = cd_start.checked_add(usize::try_from(cd_size).map_err(|_| bad("an offset overflows"))?).ok_or_else(|| bad("an offset overflows"))?;
    if cd_end != end {
        return Err(bad("the central directory doesn't end at the end record"));
    }
    let mut entries = Vec::with_capacity(count);
    let mut names = BTreeSet::new();
    let (mut central, mut local) = (cd_start, 0usize);
    for _ in 0..count {
        if u32_at(bytes, central)? != CENTRAL {
            return Err(bad("a central record is missing"));
        }
        let flags = u16_at(bytes, central + 8)?;
        let method = u16_at(bytes, central + 10)?;
        let crc = u32_at(bytes, central + 16)?;
        let (csize, usize_) = (u32_at(bytes, central + 20)?, u32_at(bytes, central + 24)?);
        let (nlen, xlen, clen) =
            (usize::from(u16_at(bytes, central + 28)?), usize::from(u16_at(bytes, central + 30)?), usize::from(u16_at(bytes, central + 32)?));
        let start_disk = u16_at(bytes, central + 34)?;
        let mode = u32_at(bytes, central + 38)? >> 16;
        let offset = u32_at(bytes, central + 42)?;
        // Unix file type bits, when present: only regular files (no links, devices or folders).
        if mode & 0o170000 != 0 && mode & 0o170000 != 0o100000 {
            return Err(bad("an entry isn't a regular file"));
        }
        if flags & 0x0001 != 0 || flags & 0x0008 != 0 || method != 0 || csize != usize_ || clen != 0 || start_disk != 0 {
            return Err(bad("entries are stored, unencrypted, with sizes in their headers and no comment"));
        }
        let name_bytes = slice(bytes, central + CENTRAL_LEN, nlen)?;
        check_extras(slice(bytes, central + CENTRAL_LEN + nlen, xlen)?)?;
        let name = std::str::from_utf8(name_bytes).map_err(|_| bad("an entry's name isn't UTF-8"))?;
        if name.is_empty() || name.contains('\0') || name.ends_with('/') {
            return Err(bad("an entry is a directory or has an empty name"));
        }
        if !names.insert(name) {
            return Err(bad("two entries have the same name"));
        }
        // Its local record is next in line, and says the same.
        if usize::try_from(offset).ok() != Some(local) || u32_at(bytes, local)? != LOCAL {
            return Err(bad("entries aren't laid out one after another from the start"));
        }
        let same = u16_at(bytes, local + 6)? == flags
            && u16_at(bytes, local + 8)? == method
            && u32_at(bytes, local + 14)? == crc
            && u32_at(bytes, local + 18)? == csize
            && u32_at(bytes, local + 22)? == usize_
            && usize::from(u16_at(bytes, local + 26)?) == nlen;
        let lxlen = usize::from(u16_at(bytes, local + 28)?);
        if !same || slice(bytes, local + LOCAL_LEN, nlen)? != name_bytes {
            return Err(bad("an entry's local header differs from its central record"));
        }
        check_extras(slice(bytes, local + LOCAL_LEN + nlen, lxlen)?)?;
        let size = usize::try_from(csize).map_err(|_| bad("an offset overflows"))?;
        let data_at = [LOCAL_LEN, nlen, lxlen].into_iter().try_fold(local, usize::checked_add).ok_or_else(|| bad("an offset overflows"))?;
        let data = slice(bytes, data_at, size)?;
        local = [LOCAL_LEN, nlen, lxlen, size].into_iter().try_fold(local, usize::checked_add).ok_or_else(|| bad("an offset overflows"))?;
        central = [CENTRAL_LEN, nlen, xlen].into_iter().try_fold(central, usize::checked_add).ok_or_else(|| bad("an offset overflows"))?;
        entries.push(Entry { name, data });
    }
    if local != cd_start || central != cd_end {
        return Err(bad("there are bytes between the entries and the central directory"));
    }
    Ok(entries)
}
