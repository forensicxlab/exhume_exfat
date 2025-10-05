use crate::bpb::BootSector;
use log::{debug, warn};
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};

/// exFAT uses 32-bit FAT entries. High 4 bits are reserved in FAT32; in exFAT full 32 are used.
/// End-of-chain markers are >= 0xFFFFFFF8.
#[inline]
pub fn is_eoc(v: u32) -> bool {
    v >= 0xFFFFFFF8
}

pub struct Fat<'a, T: Read + Seek> {
    pub bs: &'a BootSector,
    pub io: &'a mut T,
}

impl<'a, T: Read + Seek> Fat<'a, T> {
    pub fn new(bs: &'a BootSector, io: &'a mut T) -> Self {
        Self { bs, io }
    }

    pub fn read_entry(&mut self, cluster: u32) -> std::io::Result<u32> {
        let fat_byte = self.bs.fat_start_byte() + (cluster as u64 * 4);
        self.io.seek(SeekFrom::Start(fat_byte))?;
        let mut b = [0u8; 4];
        self.io.read_exact(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// Follow the chain starting at `first_cluster` and return the ordered list of clusters.
    pub fn walk_chain(&mut self, first_cluster: u32, max: usize) -> std::io::Result<Vec<u32>> {
        let mut out = Vec::new();
        let mut cur = first_cluster;
        let mut steps = 0usize;
        let mut seen = HashSet::new();

        if cur < 2 {
            warn!("walk_chain: invalid start cluster {}", cur);
            return Ok(out);
        }

        while cur >= 2 && cur < 0xFFFFFFF0 && steps < max {
            if !seen.insert(cur) {
                warn!("walk_chain: detected cycle at cluster {}", cur);
                break;
            }
            out.push(cur);
            let next = self.read_entry(cur)?;
            debug!("FAT[{}] -> {}", cur, next);
            if is_eoc(next) {
                break;
            }
            if next == 0 {
                debug!("walk_chain: cluster {} points to 0 (free?)", cur);
                break;
            }
            if next == cur {
                warn!("walk_chain: self-loop at cluster {}", cur);
                break;
            }
            cur = next;
            steps += 1;
        }
        Ok(out)
    }
}
