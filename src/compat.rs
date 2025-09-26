use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// ext-like directory entry for CLI/JSON parity with exhume_extfs
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CompatDirEntry {
    pub inode: u64,
    pub rec_len: u16,
    pub file_type: u8,
    pub name: String,
}

impl CompatDirEntry {
    pub fn from_name_inode(name: &str, inode: u64, is_dir: bool) -> Self {
        // ext's file_type values don't map 1:1; we use a simple mapping:
        // 2 = directory, 1 = regular (only used for display)
        let file_type = if is_dir { 2 } else { 1 };
        Self {
            inode,
            rec_len: 32,
            file_type,
            name: name.to_string(),
        }
    }
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
    pub fn to_string(&self) -> String {
        if !self.name.is_empty() {
            format!("{} :  {} : 0x{:x}", self.inode, self.name, self.file_type)
        } else {
            format!("{} :  ? : 0x{:x}", self.inode, self.file_type)
        }
    }
}
