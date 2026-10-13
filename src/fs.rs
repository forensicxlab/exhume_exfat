use crate::bpb::BootSector;
use crate::compat::CompatDirEntry;
use crate::direntry::{EntryType, FileRecord, RawDirEnt, assemble_file};
use crate::exinode::ExInode;
use crate::fat::Fat;
use log::{debug, warn};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FsError {
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parse: {0}")]
    Parse(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Not a file: {0}")]
    NotAFile(String),
}

pub struct ExFatFS<T: Read + Seek> {
    pub bpb: BootSector,
    pub io: T,
    // fake-inode index: inode -> (parent_dir_first_cluster, primary_entry_index, FileRecord)
    index_built: bool,
    inode_to_record: HashMap<u64, (u32, usize, FileRecord)>,
    // Only the currently read chain is retained. Sequential ranges never walk
    // from the beginning again; the visited metadata detects corrupt cycles.
    read_cursor: Option<ReadCursor>,
}

struct ReadCursor {
    first: u32,
    index: u64,
    cluster: u32,
    visited: HashSet<u32>,
}

impl<T: Read + Seek> ExFatFS<T> {
    pub fn new(mut io: T) -> Result<Self, FsError> {
        io.seek(SeekFrom::Start(0))?;
        let mut b = [0u8; 512];
        io.read_exact(&mut b)?;
        let bpb = BootSector::from_bytes(&b).map_err(FsError::Parse)?;
        Ok(Self {
            bpb,
            io,
            index_built: false,
            inode_to_record: HashMap::new(),
            read_cursor: None,
        })
    }

    #[inline]
    pub fn bytes_per_sector(&self) -> u64 {
        self.bpb.bytes_per_sector()
    }
    #[inline]
    pub fn cluster_to_offset(&self, clus: u32) -> u64 {
        self.bpb.cluster_to_byte_offset(clus)
    }

    pub fn read_cluster(&mut self, cluster: u32) -> Result<Vec<u8>, FsError> {
        if cluster < 2 {
            return Err(FsError::Parse(format!("invalid data cluster {}", cluster)));
        }
        let off = self.cluster_to_offset(cluster);
        let mut buf = vec![0u8; self.bpb.bytes_per_cluster() as usize];
        self.io.seek(SeekFrom::Start(off))?;
        self.io.read_exact(&mut buf)?;
        Ok(buf)
    }

    pub fn read_dir_entries_from_chain(
        &mut self,
        first_cluster: u32,
    ) -> Result<Vec<RawDirEnt>, FsError> {
        let mut fat = Fat::new(&self.bpb, &mut self.io);
        let chain = fat.walk_chain(first_cluster, 1_000_000)?;
        debug!(
            "read_dir_entries_from_chain: first_cluster={} chain_len={}",
            first_cluster,
            chain.len()
        );
        let mut out = Vec::new();
        for cl in chain {
            let buf = self.read_cluster(cl)?;
            for chunk in buf.chunks(32) {
                if chunk.len() < 32 {
                    break;
                }
                out.push(RawDirEnt::from_bytes(chunk));
            }
        }
        Ok(out)
    }

    pub fn list_dir_with_inodes(
        &mut self,
        first_cluster: u32,
    ) -> Result<Vec<(u64, FileRecord)>, FsError> {
        let ents = self.read_dir_entries_from_chain(first_cluster)?;
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < ents.len() {
            match ents[i].kind() {
                EntryType::End => break,
                EntryType::File => {
                    let sec_cnt = ents[i].raw[1] as usize;
                    let end = (i + 1 + sec_cnt).min(ents.len());
                    if let Some(fr) = assemble_file(&ents[i..end]) {
                        let ino = ((first_cluster as u64) << 32) | (i as u64);
                        out.push((ino, fr));
                    } else {
                        warn!("assemble_file failed at entry index {}", i);
                    }
                    i = end;
                    continue;
                }
                _ => i += 1,
            }
        }
        Ok(out)
    }

    /// Convenience for the root directory.
    pub fn list_root_with_inodes(&mut self) -> Result<Vec<(u64, FileRecord)>, FsError> {
        self.list_dir_with_inodes(self.bpb.root_dir_first_cluster)
    }

    pub fn list_dir(&mut self, first_cluster: u32) -> Result<Vec<FileRecord>, FsError> {
        let ents = self.read_dir_entries_from_chain(first_cluster)?;
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < ents.len() {
            match ents[i].kind() {
                EntryType::End => break,
                EntryType::File => {
                    let sec_cnt = ents[i].raw[1] as usize;
                    let end = (i + 1 + sec_cnt).min(ents.len());
                    if let Some(fr) = assemble_file(&ents[i..end]) {
                        out.push(fr);
                    } else {
                        warn!("assemble_file failed at entry index {}", i);
                    }
                    i = end;
                    continue;
                }
                _ => i += 1,
            }
        }
        Ok(out)
    }

    /// Build (or rebuild) the fake-inode index by walking from root.
    fn ensure_index(&mut self) -> Result<(), FsError> {
        if self.index_built {
            return Ok(());
        }
        self.inode_to_record.clear();
        let mut stack: Vec<u32> = vec![self.bpb.root_dir_first_cluster];

        while let Some(dir_clus) = stack.pop() {
            let ents = self.read_dir_entries_from_chain(dir_clus)?;
            let mut i = 0usize;
            while i < ents.len() {
                match ents[i].kind() {
                    EntryType::End => break,
                    EntryType::File => {
                        let sec_cnt = ents[i].raw[1] as usize;
                        let end = (i + 1 + sec_cnt).min(ents.len());
                        if let Some(fr) = assemble_file(&ents[i..end]) {
                            let ino = ((dir_clus as u64) << 32) | (i as u64);
                            self.inode_to_record.insert(ino, (dir_clus, i, fr.clone()));
                            if fr.is_dir() && fr.first_cluster >= 2 {
                                stack.push(fr.first_cluster);
                            }
                        } else {
                            warn!(
                                "assemble_file failed while indexing (dir_cluster={}, i={})",
                                dir_clus, i
                            );
                        }
                        i = end;
                        continue;
                    }
                    _ => i += 1,
                }
            }
        }
        self.index_built = true;
        debug!(
            "ensure_index: indexed {} inodes",
            self.inode_to_record.len()
        );
        Ok(())
    }

    // exhume_exfat/src/fs.rs

    pub fn read_file(&mut self, fr: &FileRecord) -> Result<Vec<u8>, FsError> {
        if fr.first_cluster < 2 {
            return Err(FsError::Parse(format!(
                "invalid first_cluster {} for '{}'",
                fr.first_cluster, fr.name
            )));
        }

        // exFAT Stream Extension general_flags:
        // bit 1 == 1  -> NoFatChain (contiguous allocation)
        let no_fat_chain = (fr.general_flags & 0x02) != 0;

        if no_fat_chain {
            // Read contiguous clusters directly
            let mut remaining = fr.size as usize;
            let mut cur = fr.first_cluster;
            let mut out = Vec::with_capacity(remaining);

            while remaining > 0 {
                let blk = self.read_cluster(cur)?;
                let take = remaining.min(blk.len());
                out.extend_from_slice(&blk[..take]);
                remaining -= take;
                cur += 1; // contiguous
            }
            return Ok(out);
        }

        // FAT-chained allocation
        let clusters_needed = (fr.size).div_ceil(self.bpb.bytes_per_cluster()) as usize;
        let mut fat = Fat::new(&self.bpb, &mut self.io);
        let chain = fat.walk_chain(fr.first_cluster, clusters_needed + 4)?;
        if chain.is_empty() && fr.size > 0 {
            warn!(
                "read_file: empty chain but size={} for '{}'",
                fr.size, fr.name
            );
        }

        let mut out = Vec::with_capacity(fr.size as usize);
        for cl in chain {
            let buf = self.read_cluster(cl)?;
            out.extend_from_slice(&buf);
            if out.len() >= fr.size as usize {
                break;
            }
        }
        out.truncate(fr.size as usize);
        Ok(out)
    }

    pub fn read_path(&mut self, path: &str) -> Result<Vec<u8>, FsError> {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let mut cur_dir = self.bpb.root_dir_first_cluster;
        if parts.is_empty() {
            return Err(FsError::NotAFile("/".into()));
        }

        for (idx, comp) in parts.iter().enumerate() {
            let entries = self.list_dir(cur_dir)?;
            let mut next: Option<FileRecord> = None;
            for e in entries {
                if e.name.eq_ignore_ascii_case(comp) {
                    next = Some(e);
                    break;
                }
            }
            if let Some(fr) = next {
                if idx == parts.len() - 1 {
                    if fr.is_dir() {
                        return Err(FsError::NotAFile(fr.name));
                    }
                    return self.read_file(&fr);
                } else {
                    cur_dir = fr.first_cluster;
                }
            } else {
                return Err(FsError::NotFound(format!(
                    "{} (at '/{}')",
                    comp,
                    parts[..=idx].join("/")
                )));
            }
        }
        Err(FsError::NotFound(path.to_string()))
    }

    pub fn super_info_json(&self) -> Value {
        json!({ "bpb": self.bpb.to_json() })
    }

    pub fn get_inode(&mut self, inode_num: u64) -> Result<ExInode, FsError> {
        self.ensure_index()?;
        let (_p, _idx, fr) = self
            .inode_to_record
            .get(&inode_num)
            .ok_or_else(|| FsError::NotFound(format!("inode 0x{:016x}", inode_num)))?;
        Ok(ExInode::from_record(inode_num, fr))
    }

    pub fn resolve_path_to_inode_num(&mut self, path: &str) -> Result<(u64, ExInode), FsError> {
        self.ensure_index()?;
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return Err(FsError::NotFound("/".into()));
        }

        let mut cur_dir = self.bpb.root_dir_first_cluster;
        let mut current_inode: Option<u64> = None;

        for (pos, comp) in parts.iter().enumerate() {
            let mut found: Option<(u64, FileRecord)> = None;
            for (ino, (parent, _idx, fr)) in self.inode_to_record.iter() {
                if *parent == cur_dir && fr.name.eq_ignore_ascii_case(comp) {
                    found = Some((*ino, fr.clone()));
                    break;
                }
            }
            if let Some((ino, fr)) = found {
                current_inode = Some(ino);
                if pos < parts.len() - 1 {
                    cur_dir = fr.first_cluster;
                }
            } else {
                return Err(FsError::NotFound((*comp).to_string()));
            }
        }

        let ino = current_inode.unwrap();
        let inode = self.get_inode(ino)?;
        Ok((ino, inode))
    }

    pub fn list_dir_inode(&mut self, inode: &ExInode) -> Result<Vec<CompatDirEntry>, FsError> {
        self.ensure_index()?;
        if !inode.is_dir() {
            return Err(FsError::NotFound("not a directory".into()));
        }
        let mut out = Vec::new();
        for (ino, (parent, _idx, fr)) in self.inode_to_record.iter() {
            if *parent == inode.first_cluster {
                out.push(CompatDirEntry::from_name_inode(&fr.name, *ino, fr.is_dir()));
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn read_inode(&mut self, inode: &ExInode) -> Result<Vec<u8>, FsError> {
        self.ensure_index()?;

        let fr = self
            .inode_to_record
            .get(&inode.i_num)
            .map(|(_, _, fr)| fr.clone())
            .ok_or_else(|| FsError::NotFound(format!("inode 0x{:016x}", inode.i_num)))?;

        if (fr.attributes & 0x0010) != 0 {
            return Err(FsError::NotAFile(fr.name));
        }

        self.read_file(&fr)
    }

    /// Read logical bytes without allocating the file or a whole cluster.
    pub fn read_inode_range(
        &mut self,
        inode: &ExInode,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, FsError> {
        self.ensure_index()?;
        let record = self
            .inode_to_record
            .get(&inode.i_num)
            .map(|(_, _, record)| record.clone())
            .ok_or_else(|| FsError::NotFound(format!("inode {}", inode.i_num)))?;
        self.read_file_range(&record, offset, length)
    }

    /// FAT traversal state is reused between increasing range requests.
    pub fn read_file_range(
        &mut self,
        file: &FileRecord,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, FsError> {
        if file.is_dir() {
            return Err(FsError::NotAFile(file.name.clone()));
        }
        if offset >= file.size || length == 0 {
            return Ok(Vec::new());
        }
        let count = (file.size - offset).min(length as u64) as usize;
        let valid = file.valid_data_length.unwrap_or(file.size);
        if valid > file.size {
            return Err(FsError::Parse("valid data length exceeds file size".into()));
        }
        let initialized = valid.saturating_sub(offset).min(count as u64) as usize;
        let cluster_bytes = self.bpb.bytes_per_cluster();
        let mut output = vec![0; count];
        let mut done = 0;
        while done < initialized {
            let logical = offset + done as u64;
            let index = logical / cluster_bytes;
            let cluster = if file.general_flags & 2 != 0 {
                u64::from(file.first_cluster)
                    .checked_add(index)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or_else(|| FsError::Parse("contiguous cluster overflow".into()))?
            } else {
                if self
                    .read_cursor
                    .as_ref()
                    .is_none_or(|c| c.first != file.first_cluster || c.index > index)
                {
                    self.validate_data_cluster(file.first_cluster)?;
                    self.read_cursor = Some(ReadCursor {
                        first: file.first_cluster,
                        index: 0,
                        cluster: file.first_cluster,
                        visited: HashSet::from([file.first_cluster]),
                    });
                }
                while self.read_cursor.as_ref().unwrap().index < index {
                    let current = self.read_cursor.as_ref().unwrap().cluster;
                    let next = Fat::new(&self.bpb, &mut self.io).read_entry(current)?;
                    self.validate_data_cluster(next)?;
                    let cursor = self.read_cursor.as_mut().unwrap();
                    if !cursor.visited.insert(next) {
                        return Err(FsError::Parse("cycle in file cluster chain".into()));
                    }
                    cursor.cluster = next;
                    cursor.index += 1;
                }
                self.read_cursor.as_ref().unwrap().cluster
            };
            self.validate_data_cluster(cluster)?;
            let within = logical % cluster_bytes;
            let take = (initialized - done).min((cluster_bytes - within) as usize);
            let physical = self
                .cluster_to_offset(cluster)
                .checked_add(within)
                .ok_or_else(|| FsError::Parse("cluster byte offset overflow".into()))?;
            self.io.seek(SeekFrom::Start(physical))?;
            self.io.read_exact(&mut output[done..done + take])?;
            done += take;
        }
        Ok(output)
    }

    fn validate_data_cluster(&self, cluster: u32) -> Result<(), FsError> {
        if cluster < 2
            || u64::from(cluster) >= u64::from(self.bpb.cluster_count) + 2
            || cluster >= 0xffff_fff0
        {
            return Err(FsError::Parse(format!(
                "invalid or prematurely terminated cluster chain at {cluster:#x}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod range_tests {
    use super::*;
    use std::io::Cursor;
    fn fixture(chained: bool) -> (ExFatFS<Cursor<Vec<u8>>>, FileRecord) {
        let bpb = BootSector {
            oem_name: *b"EXFAT   ",
            partition_offset: 0,
            volume_length: 64,
            fat_offset: 1,
            fat_length: 1,
            cluster_heap_offset: 4,
            cluster_count: 32,
            root_dir_first_cluster: 2,
            volume_serial: 0,
            fs_revision: 0x100,
            volume_flags: 0,
            bytes_per_sector_shift: 9,
            sectors_per_cluster_shift: 0,
            num_fats: 1,
            drive_select: 0,
        };
        let mut bytes = vec![0; 64 * 512];
        let clusters = if chained { [3, 7, 4] } else { [3, 4, 5] };
        for (i, cluster) in clusters.iter().enumerate() {
            let offset = bpb.cluster_to_byte_offset(*cluster) as usize;
            bytes[offset..offset + 512].fill(b'a' + i as u8);
            let next = clusters.get(i + 1).copied().unwrap_or(u32::MAX);
            bytes[512 + *cluster as usize * 4..516 + *cluster as usize * 4]
                .copy_from_slice(&next.to_le_bytes());
        }
        let fs = ExFatFS {
            bpb,
            io: Cursor::new(bytes),
            index_built: true,
            inode_to_record: HashMap::new(),
            read_cursor: None,
        };
        let record = FileRecord {
            name: "test".into(),
            attributes: 0,
            first_cluster: 3,
            size: 1500,
            valid_data_length: Some(1500),
            general_flags: if chained { 0 } else { 2 },
            create_time: 0,
            last_mod_time: 0,
            last_access_time: 0,
        };
        (fs, record)
    }
    #[test]
    fn contiguous_and_fragmented_ranges_match_expected_bytes() {
        for chained in [false, true] {
            let (mut fs, record) = fixture(chained);
            let expected = [vec![b'a'; 512], vec![b'b'; 512], vec![b'c'; 476]].concat();
            for (offset, count) in [
                (0, 1),
                (500, 50),
                (549, 1),
                (1020, 700),
                (1500, 10),
                (u64::MAX, 5),
                (17, 40),
            ] {
                let result = fs.read_file_range(&record, offset, count).unwrap();
                let start = (offset.min(expected.len() as u64)) as usize;
                assert_eq!(result, expected[start..(start + count).min(expected.len())]);
            }
        }
    }
    #[test]
    fn sequential_chain_keeps_its_cursor_and_rejects_cycles_and_short_chains() {
        let (mut fs, record) = fixture(true);
        fs.read_file_range(&record, 512, 1).unwrap();
        assert_eq!(fs.read_cursor.as_ref().unwrap().index, 1);
        fs.read_file_range(&record, 600, 1).unwrap();
        assert_eq!(fs.read_cursor.as_ref().unwrap().visited.len(), 2);
        fs.io.get_mut()[512 + 7 * 4..516 + 7 * 4].copy_from_slice(&3u32.to_le_bytes());
        assert!(
            fs.read_file_range(&record, 1024, 1)
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );
        let (mut fs, record) = fixture(true);
        fs.io.get_mut()[512 + 3 * 4..516 + 3 * 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(fs.read_file_range(&record, 512, 1).is_err());
    }
    #[test]
    fn uninitialized_tail_is_zero_filled_and_empty_files_need_no_cluster() {
        let (mut fs, mut record) = fixture(false);
        record.valid_data_length = Some(510);
        assert_eq!(
            fs.read_file_range(&record, 508, 8).unwrap(),
            [b'a', b'a', 0, 0, 0, 0, 0, 0]
        );
        record.size = 0;
        record.valid_data_length = Some(0);
        record.first_cluster = 0;
        assert!(fs.read_file_range(&record, 0, 10).unwrap().is_empty());
        record.size = 1;
        record.valid_data_length = Some(2);
        assert!(fs.read_file_range(&record, 0, 1).is_err());
    }
    #[test]
    fn a_small_read_never_materializes_the_advertised_file() {
        let (mut fs, mut record) = fixture(false);
        record.size = 1024 * 1024 * 1024;
        record.valid_data_length = Some(record.size);
        assert_eq!(fs.read_file_range(&record, 3, 12).unwrap(), vec![b'a'; 12]);
        assert_eq!(fs.io.position(), fs.cluster_to_offset(3) + 15);
    }
}
