// Copyright 2025 PackDB Contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! A deterministic tar container for the support bundle.
//!
//! Only what the bundle needs: regular files with short relative names, in
//! the order given, with fixed mode, owner and modification time, so the same
//! contents always produce the same bytes. [`read`] parses exactly this form
//! back, which is how a written bundle is verified.

const BLOCK: usize = 512;

/// The longest file name the USTAR `name` field holds.
const MAX_NAME: usize = 100;

/// Where the checksum field sits, and its width.
const CHECKSUM: (usize, usize) = (148, 8);

fn put(block: &mut [u8; BLOCK], at: usize, bytes: &[u8]) {
    for (slot, byte) in block.iter_mut().skip(at).zip(bytes) {
        *slot = *byte;
    }
}

fn field(block: &[u8], at: usize, width: usize) -> Vec<u8> {
    block.iter().skip(at).take(width).copied().collect()
}

/// A numeric header field: `width - 1` octal digits, then NUL.
fn octal(width: usize, value: u64) -> Result<Vec<u8>, String> {
    let digits = width - 1;
    let mut text = format!("{value:0digits$o}").into_bytes();
    if text.len() > digits {
        return Err(format!("{value} does not fit a tar header field"));
    }
    text.push(0);
    Ok(text)
}

fn checksum(block: &[u8; BLOCK]) -> u64 {
    let mut copy = *block;
    put(&mut copy, CHECKSUM.0, &[b' '; CHECKSUM.1]);
    copy.iter().map(|byte| u64::from(*byte)).sum()
}

fn header(name: &str, size: usize) -> Result<[u8; BLOCK], String> {
    if name.is_empty() || name.len() > MAX_NAME || name.starts_with('/') || name.contains("..") {
        return Err(format!("{name:?} is not a valid bundle entry name"));
    }
    let size = u64::try_from(size).map_err(|_| format!("{name} is too large"))?;
    let mut block = [0_u8; BLOCK];
    put(&mut block, 0, name.as_bytes());
    put(&mut block, 100, &octal(8, 0o644)?);
    put(&mut block, 108, &octal(8, 0)?);
    put(&mut block, 116, &octal(8, 0)?);
    put(&mut block, 124, &octal(12, size)?);
    put(&mut block, 136, &octal(12, 0)?);
    put(&mut block, 156, b"0");
    put(&mut block, 257, b"ustar\0");
    put(&mut block, 263, b"00");
    let sum = format!("{:06o}\0 ", checksum(&block));
    put(&mut block, CHECKSUM.0, sum.as_bytes());
    Ok(block)
}

/// A tar archive of `entries`, in order.
pub(crate) fn write(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut archive = Vec::new();
    for (name, data) in entries {
        archive.extend_from_slice(&header(name, data.len())?);
        archive.extend_from_slice(data);
        let padding = (BLOCK - data.len() % BLOCK) % BLOCK;
        archive.resize(archive.len() + padding, 0);
    }
    archive.resize(archive.len() + 2 * BLOCK, 0);
    Ok(archive)
}

fn parse_octal(bytes: &[u8]) -> Result<u64, String> {
    let text: String = bytes
        .iter()
        .take_while(|byte| **byte != 0 && **byte != b' ')
        .map(|byte| char::from(*byte))
        .collect();
    u64::from_str_radix(&text, 8).map_err(|_| format!("{text:?} is not an octal header field"))
}

/// The entries of an archive [`write`] produced.
pub(crate) fn read(archive: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut entries = Vec::new();
    let mut offset = 0;
    loop {
        let mut block = [0_u8; BLOCK];
        let bytes = archive
            .get(offset..offset + BLOCK)
            .ok_or("the archive ends without its end marker")?;
        block.copy_from_slice(bytes);
        if block.iter().all(|byte| *byte == 0) {
            return Ok(entries);
        }
        let recorded = parse_octal(&field(&block, CHECKSUM.0, CHECKSUM.1))?;
        if recorded != checksum(&block)
            || field(&block, 257, 6) != b"ustar\0"
            || field(&block, 156, 1) != b"0"
        {
            return Err(format!("the header at byte {offset} is not a bundle entry"));
        }
        let name: String = block
            .iter()
            .take(MAX_NAME)
            .take_while(|byte| **byte != 0)
            .map(|byte| char::from(*byte))
            .collect();
        let size = usize::try_from(parse_octal(&field(&block, 124, 12))?)
            .map_err(|_| "an entry is larger than this platform can hold")?;
        let start = offset + BLOCK;
        let data = archive
            .get(start..start + size)
            .ok_or_else(|| format!("{name} is truncated"))?;
        entries.push((name, data.to_vec()));
        offset = start + size + (BLOCK - size % BLOCK) % BLOCK;
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn entries() -> Vec<(String, Vec<u8>)> {
        vec![
            ("manifest.json".into(), b"{}".to_vec()),
            ("server/info.json".into(), vec![b'x'; 513]),
            ("empty".into(), Vec::new()),
        ]
    }

    #[test]
    fn what_is_written_reads_back_identically() {
        let archive = write(&entries()).expect("write");
        assert_eq!(archive.len() % BLOCK, 0);
        assert_eq!(read(&archive).expect("read"), entries());
        assert_eq!(write(&entries()).expect("again"), archive, "deterministic");
    }

    #[test]
    fn the_system_tar_lists_the_entries() {
        let archive = write(&entries()).expect("write");
        let path = std::env::temp_dir().join(format!("packdb-tarball-{}.tar", std::process::id()));
        std::fs::write(&path, &archive).expect("write file");
        let listed = std::process::Command::new("tar")
            .arg("-tf")
            .arg(&path)
            .output();
        std::fs::remove_file(&path).expect("remove");
        let Ok(listed) = listed else {
            return; // No `tar` on this machine.
        };
        assert!(listed.status.success(), "{listed:?}");
        let names = String::from_utf8(listed.stdout).expect("utf-8");
        assert_eq!(
            names.lines().collect::<Vec<_>>(),
            ["manifest.json", "server/info.json", "empty"]
        );
    }

    #[test]
    fn unsafe_names_and_damaged_archives_are_refused() {
        for name in ["", "/etc/passwd", "../x", &"a".repeat(101)] {
            assert!(write(&[(name.to_string(), Vec::new())]).is_err(), "{name}");
        }
        let mut archive = write(&entries()).expect("write");
        archive[10] ^= 1;
        assert!(read(&archive).is_err(), "checksum");
        let archive = write(&entries()).expect("write");
        assert!(
            read(&archive[..archive.len() - 2 * BLOCK]).is_err(),
            "no end marker"
        );
        assert!(read(&archive[..600]).is_err(), "truncated");
    }
}
