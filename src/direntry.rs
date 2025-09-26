use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    AllocationBitmap, // 0x81
    UpCaseTable,      // 0x82
    VolumeLabel,      // 0x83
    File,             // 0x85 (file directory entry – 1st in a set)
    StreamExt,        // 0xC0 (stream extension)
    FileName,         // 0xC1 (file name 15 UTF-16 chars)
    Unknown(u8),
    End, // 0x00 empty marks end of directory
}

impl From<u8> for EntryType {
    fn from(v: u8) -> Self {
        match v {
            0x00 => EntryType::End,
            0x81 => EntryType::AllocationBitmap,
            0x82 => EntryType::UpCaseTable,
            0x83 => EntryType::VolumeLabel,
            0x85 => EntryType::File,
            0xC0 => EntryType::StreamExt,
            0xC1 => EntryType::FileName,
            x => EntryType::Unknown(x),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawDirEnt {
    pub entry_type: u8,
    pub raw: [u8; 32],
}

impl RawDirEnt {
    pub fn from_bytes(b: &[u8]) -> Self {
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&b[0..32]);
        Self {
            entry_type: raw[0],
            raw,
        }
    }
    pub fn kind(&self) -> EntryType {
        EntryType::from(self.entry_type)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDirectoryEntry {
    // 0x85
    pub secondary_count: u8, // number of following secondary entries
    pub set_checksum: u16,
    pub attributes: u16,
    pub create_time: u32,
    pub last_mod_time: u32,
    pub last_access_time: u32,
}

impl FileDirectoryEntry {
    pub fn parse(raw: &RawDirEnt) -> Self {
        let b = &raw.raw;
        let le_u16 = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap());
        let le_u32 = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        Self {
            secondary_count: b[1],
            set_checksum: le_u16(2),
            attributes: le_u16(4),
            create_time: le_u32(8),
            last_mod_time: le_u32(12),
            last_access_time: le_u32(16),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamExtensionEntry {
    // 0xC0
    pub general_flags: u8,
    pub first_cluster: u32,
    pub data_length: u64,
}

impl StreamExtensionEntry {
    pub fn parse(raw: &RawDirEnt) -> Self {
        let b = &raw.raw;
        let le_u32 = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let le_u64 = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        Self {
            general_flags: b[1],
            first_cluster: le_u32(20),
            data_length: le_u64(24),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileNameEntry {
    // 0xC1
    pub name_fragment: String, // up to 15 UTF-16LE chars
}

impl FileNameEntry {
    pub fn parse(raw: &RawDirEnt) -> Self {
        let b = &raw.raw;
        // bytes 2..32 hold 15 UTF-16LE code units (30 bytes)
        let mut u16s = [0u16; 15];
        for i in 0..15 {
            u16s[i] = u16::from_le_bytes([b[2 + i * 2], b[3 + i * 2]]);
        }
        let iter = u16s.into_iter().take_while(|&c| c != 0); // stop at NUL
        let name = String::from_utf16_lossy(&iter.collect::<Vec<_>>());
        Self {
            name_fragment: name,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationBitmapEntry {
    // 0x81
    pub first_cluster: u32,
    pub data_length: u32, // bytes
}

impl AllocationBitmapEntry {
    pub fn parse(raw: &RawDirEnt) -> Self {
        let b = &raw.raw;
        let le_u32 = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        Self {
            first_cluster: le_u32(20),
            data_length: le_u32(24),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpcaseTableEntry {
    // 0x82
    pub first_cluster: u32,
    pub data_length: u32,
}

impl UpcaseTableEntry {
    pub fn parse(raw: &RawDirEnt) -> Self {
        let b = &raw.raw;
        let le_u32 = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        Self {
            first_cluster: le_u32(20),
            data_length: le_u32(24),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeLabelEntry {
    // 0x83
    pub label: String,
}

impl VolumeLabelEntry {
    pub fn parse(raw: &RawDirEnt) -> Self {
        let b = &raw.raw;
        let len = b[1] as usize; // number of UTF-16 chars in label
        let mut out = String::new();
        for i in 0..len.min(11) {
            let ch = u16::from_le_bytes([b[2 + i * 2], b[3 + i * 2]]);
            out.push(char::from_u32(ch as u32).unwrap_or('\u{FFFD}'));
        }
        Self { label: out }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub name: String,
    pub attributes: u16,
    pub first_cluster: u32,
    pub size: u64,
}

impl FileRecord {
    pub fn is_dir(&self) -> bool {
        (self.attributes & 0x0010) != 0
    }
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

// Helper to assemble a set of 0x85 + 0xC0 + 0xC1... into a FileRecord
pub fn assemble_file<'a>(set: &'a [RawDirEnt]) -> Option<FileRecord> {
    if set.is_empty() {
        return None;
    }
    let mut fde: Option<FileDirectoryEntry> = None;
    let mut stream: Option<StreamExtensionEntry> = None;
    let mut name = String::new();

    for e in set {
        match e.kind() {
            EntryType::File => {
                fde = Some(FileDirectoryEntry::parse(e));
            }
            EntryType::StreamExt => {
                stream = Some(StreamExtensionEntry::parse(e));
            }
            EntryType::FileName => {
                name.push_str(&FileNameEntry::parse(e).name_fragment);
            }
            _ => {}
        }
    }
    if let (Some(fd), Some(st)) = (fde, stream) {
        return Some(FileRecord {
            name,
            attributes: fd.attributes,
            first_cluster: st.first_cluster,
            size: st.data_length,
        });
    }
    None
}
