use crate::bpb::BootSector;
use crate::compat::CompatDirEntry;
use crate::direntry::{EntryType, FileRecord, RawDirEnt, assemble_file};
use crate::exinode::ExInode;
use crate::fat::Fat;
use log::error;
use serde_json::{Value, json};
use std::collections::HashMap;
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
    io: T,
    // fake-inode index: inode -> (parent_dir_first_cluster, primary_entry_index, FileRecord)
    index_built: bool,
    inode_to_record: HashMap<u64, (u32, usize, FileRecord)>,
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

    fn read_cluster(&mut self, cluster: u32) -> Result<Vec<u8>, FsError> {
        let off = self.cluster_to_offset(cluster);
        let mut buf = vec![0u8; self.bpb.bytes_per_cluster() as usize];
        self.io.seek(SeekFrom::Start(off))?;
        self.io.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn read_dir_entries_from_chain(
        &mut self,
        first_cluster: u32,
    ) -> Result<Vec<RawDirEnt>, FsError> {
        let mut fat = Fat::new(&self.bpb, &mut self.io);
        let chain = fat.walk_chain(first_cluster, 1_000_000)?;
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
                    }
                    i = end;
                    continue;
                }
                _ => {
                    i += 1;
                }
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
                    }
                    i = end;
                    continue;
                }
                _ => {
                    i += 1;
                }
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
                        }
                        i = end;
                        continue;
                    }
                    _ => {
                        i += 1;
                    }
                }
            }
        }
        self.index_built = true;
        Ok(())
    }

    pub fn read_file(&mut self, fr: &FileRecord) -> Result<Vec<u8>, FsError> {
        let mut fat = Fat::new(&self.bpb, &mut self.io);
        let cluster_guess = (fr.size / self.bpb.bytes_per_cluster()) as usize + 4;
        let chain = fat.walk_chain(fr.first_cluster, cluster_guess)?;
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
                return Err(FsError::NotFound(comp.to_string()));
            }
        }
        Err(FsError::NotFound(path.to_string()))
    }

    pub fn super_info_json(&self) -> Value {
        json!({ "bpb": self.bpb.to_json() })
    }

    // ---------- ext-like façade ----------
    pub fn get_inode(&mut self, inode_num: u64) -> Result<ExInode, FsError> {
        self.ensure_index()?;
        let (_p, _idx, fr) = self
            .inode_to_record
            .get(&inode_num)
            .ok_or_else(|| FsError::NotFound(format!("inode {}", inode_num)))?;
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

        // Take an owned copy of the FileRecord, then drop the map borrow
        let fr = self
            .inode_to_record
            .get(&inode.i_num)
            .map(|(_, _, fr)| fr.clone())
            .ok_or_else(|| FsError::NotFound(format!("inode {}", inode.i_num)))?;

        if (fr.attributes & 0x0010) != 0 {
            return Err(FsError::NotAFile(fr.name));
        }

        // Now we can mutably borrow `self`
        self.read_file(&fr)
    }
}
