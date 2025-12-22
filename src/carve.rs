/// Main source : https://arxiv.org/pdf/1804.08653
use crate::direntry::{EntryType, FileDirectoryEntry, StreamExtensionEntry};
use crate::fat::Fat;
use crate::fs::{ExFatFS, FsError};
use log::{debug, info, warn};
use std::fs::{File, create_dir_all};
use std::io::Write;
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub enum Magic {
    Jpeg,
    Png,
    Pdf,
    Zip,
    Mp4,
}

impl Magic {
    fn all() -> &'static [Magic] {
        &[Magic::Jpeg, Magic::Png, Magic::Pdf, Magic::Zip, Magic::Mp4]
    }
    fn name(self) -> &'static str {
        match self {
            Magic::Jpeg => "jpeg",
            Magic::Png => "png",
            Magic::Pdf => "pdf",
            Magic::Zip => "zip",
            Magic::Mp4 => "mp4",
        }
    }
    fn matches(self, buf: &[u8]) -> bool {
        match self {
            Magic::Jpeg => buf.len() >= 4 && buf[0] == 0xFF && buf[1] == 0xD8 && buf[2] == 0xFF,
            Magic::Png => buf.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            Magic::Pdf => buf.starts_with(b"%PDF-"),
            Magic::Zip => buf.starts_with(b"PK\x03\x04"),
            Magic::Mp4 => buf.len() >= 12 && &buf[4..8] == b"ftyp",
        }
    }
}

/// Inactive file metadata reconstructed from directory entries (0x80 cleared).
#[derive(Clone, Debug)]
pub struct InactiveMeta {
    pub name: String,
    pub first_cluster: u32,
    pub size: u64,
    pub uses_fat: bool, // false => contiguous (bit set means "no FAT")
    pub create_time: u32,
    pub last_mod_time: u32,
    pub last_access_time: u32,
}

/// Find the Allocation Bitmap entry (0x81) in the root directory and read it into memory (bytes).
fn read_allocation_bitmap<T: std::io::Read + std::io::Seek>(
    fs: &mut ExFatFS<T>,
) -> Result<Vec<u8>, FsError> {
    // Read raw directory entries directly from the root
    let raw = fs.read_dir_entries_from_chain(fs.bpb.root_dir_first_cluster)?;
    let mut i = 0usize;

    while i < raw.len() {
        let e = &raw[i];
        match EntryType::from(e.entry_type) {
            EntryType::End => break,
            EntryType::AllocationBitmap => {
                // AllocationBitmap entry format:
                // byte[1]  = bitmap flags (bit0 = active bitmap)
                // byte[20] = first cluster (u32 LE)
                // byte[24] = data length (u32 LE)
                let b = &e.raw;
                let first_cluster = u32::from_le_bytes([b[20], b[21], b[22], b[23]]);
                let data_length = u32::from_le_bytes([b[24], b[25], b[26], b[27]]) as usize;

                // Follow FAT chain if necessary
                let mut fat = Fat::new(&fs.bpb, &mut fs.io);
                let chain = fat.walk_chain(first_cluster, 1_000_000)?;

                // Read all clusters into buffer
                let mut buf = Vec::with_capacity(data_length);
                for cl in chain {
                    let block = fs.read_cluster(cl)?;
                    buf.extend_from_slice(&block);
                    if buf.len() >= data_length {
                        break;
                    }
                }
                buf.truncate(data_length);
                return Ok(buf);
            }
            _ => {}
        }
        i += 1;
    }

    Err(FsError::NotFound(
        "Allocation Bitmap not found in root".into(),
    ))
}

/// Return **all inactive** file sets across the FS (https://arxiv.org/pdf/1804.08653)
fn collect_inactive_entries<T: std::io::Read + std::io::Seek>(
    fs: &mut ExFatFS<T>,
) -> Result<Vec<InactiveMeta>, FsError> {
    let mut out = Vec::new();
    let mut stack = vec![fs.bpb.root_dir_first_cluster];

    while let Some(dir_clus) = stack.pop() {
        let ents = fs.read_dir_entries_from_chain(dir_clus)?;
        let mut i = 0usize;

        // Walk entry sets: treat type with MSB forced to 1 while remembering original "active bit"
        while i < ents.len() {
            let e = &ents[i];
            if e.entry_type == 0x00 {
                break;
            }

            // Decode set whether active or inactive by normalizing type
            let normalized_kind = EntryType::from(e.entry_type | 0x80);
            if matches!(normalized_kind, EntryType::File) {
                let sec_cnt = ents[i].raw[1] as usize;
                let end = (i + 1 + sec_cnt).min(ents.len());
                let active = (ents[i].entry_type & 0x80) != 0;

                // Parse secondaries manually to extract flags & name even if inactive
                let mut fde: Option<FileDirectoryEntry> = None;
                let mut st: Option<StreamExtensionEntry> = None;
                let mut name = String::new();

                for e2 in &ents[i..end] {
                    match EntryType::from(e2.entry_type | 0x80) {
                        EntryType::File => {
                            fde = Some(FileDirectoryEntry::parse(e2));
                        }
                        EntryType::StreamExt => {
                            st = Some(StreamExtensionEntry::parse(e2));
                        }
                        EntryType::FileName => {
                            name.push_str(&crate::direntry::FileNameEntry::parse(e2).name_fragment);
                        }
                        _ => {}
                    }
                }

                if let (Some(fd), Some(st)) = (fde, st) {
                    if !active {
                        let uses_fat = (st.general_flags & 0x02) == 0; // bit set => no FAT (contiguous); so uses_fat = !bit
                        out.push(InactiveMeta {
                            name,
                            first_cluster: st.first_cluster,
                            size: st.data_length,
                            uses_fat,
                            create_time: fd.create_time,
                            last_mod_time: fd.last_mod_time,
                            last_access_time: fd.last_access_time,
                        });
                    } else if (fd.attributes & 0x0010) != 0 && st.first_cluster >= 2 {
                        // active subdir: recurse
                        stack.push(st.first_cluster);
                    }
                }
                i = end;
                continue;
            }
            i += 1;
        }
    }

    Ok(out)
}

fn bitmap_is_allocated(bitmap: &[u8], cluster: u32) -> bool {
    if cluster < 2 {
        return true; // treat as allocated / not for carving
    }
    let idx = (cluster - 2) as usize;
    let byte = idx / 8;
    let bit = idx % 8;
    if byte >= bitmap.len() {
        return true; // be safe
    }
    (bitmap[byte] & (1u8 << bit)) != 0
}

/// Carve unallocated clusters following the methodology (cluster-start signatures, metadata from inactive entries).
pub fn carve<T: std::io::Read + std::io::Seek>(
    fs: &mut ExFatFS<T>,
    out_dir: &str,
    limit: Option<usize>,
) -> Result<usize, FsError> {
    let bitmap = read_allocation_bitmap(fs)?;
    let inact = collect_inactive_entries(fs)?;
    debug!("carve: inactive sets indexed = {}", inact.len());

    create_dir_all(out_dir).map_err(|e| FsError::Io(e))?;

    let mut found = 0usize;
    let mut cl = 2u32; // first data cluster

    // Walk through all clusters; look only at unallocated ones (https://arxiv.org/pdf/1804.08653)
    while (cl as u64) < fs.bpb.cluster_count as u64 + 2 {
        if !bitmap_is_allocated(&bitmap, cl) {
            // scan header at cluster start
            let buf = fs.read_cluster(cl)?;
            for m in Magic::all() {
                if m.matches(&buf) {
                    // Try to match with the most recent inactive entry with same first_cluster
                    let mut candidates: Vec<&InactiveMeta> =
                        inact.iter().filter(|im| im.first_cluster == cl).collect();

                    // If multiple, prefer the one with the most recent last_mod_time (heuristic).
                    candidates.sort_by_key(|im| im.last_mod_time);
                    candidates.reverse();

                    let meta_opt = candidates.first().cloned();

                    // Decide size and chain method
                    let (size, uses_fat, _name_guess) = if let Some(meta) = meta_opt {
                        (meta.size, meta.uses_fat, meta.name.clone())
                    } else {
                        // Unknown metadata: fall back to contiguous recovery until next allocated cluster or limit to a few MB
                        warn!(
                            "carve: header {:?} at cluster {} but no inactive entry matched; falling back to contiguous scan",
                            m, cl
                        );
                        // Simple fallback: read contiguous unallocated clusters up to 16MB
                        let fallback = 16 * 1024 * 1024usize;
                        (fallback as u64, false, format!("{}_0x{:08x}", m.name(), cl))
                    };

                    // Read data
                    let data = if uses_fat {
                        let mut fat = Fat::new(&fs.bpb, &mut fs.io);
                        let chain = fat.walk_chain(cl, 1_000_000)?;
                        let mut out = Vec::with_capacity(size as usize);
                        for c in chain {
                            let blk = fs.read_cluster(c)?;
                            out.extend_from_slice(&blk);
                            if out.len() >= size as usize {
                                break;
                            }
                        }
                        let mut v = out;
                        v.truncate(size as usize);
                        v
                    } else {
                        // contiguous from first_cluster
                        let mut remaining = size as usize;
                        let mut cur = cl;
                        let mut out = Vec::with_capacity(remaining);
                        while remaining > 0 && !bitmap_is_allocated(&bitmap, cur) {
                            let blk = fs.read_cluster(cur)?;
                            let take = remaining.min(blk.len());
                            out.extend_from_slice(&blk[..take]);
                            remaining -= take;
                            cur += 1;
                        }
                        out
                    };

                    // Output file
                    let fname = if let Some(meta) = meta_opt {
                        // Try to keep original extension; else use magic name
                        let ext = Path::new(&meta.name)
                            .extension()
                            .and_then(|s| s.to_str())
                            .unwrap_or(m.name());
                        format!("{}/carved_0x{:08x}_{}.{}", out_dir, cl, m.name(), ext)
                    } else {
                        format!("{}/carved_0x{:08x}_{}.bin", out_dir, cl, m.name())
                    };
                    let mut f = File::create(&fname)?;
                    f.write_all(&data)?;
                    info!("carved {} bytes -> {}", data.len(), fname);

                    found += 1;
                    if let Some(max) = limit {
                        if found >= max {
                            return Ok(found);
                        }
                    }
                    break;
                }
            }
        }
        cl += 1;
    }
    Ok(found)
}
