use crate::direntry::FileRecord;
use prettytable::{Cell, Row, Table};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A fake-inode wrapper to offer an ext-like API on exFAT.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExInode {
    pub i_num: u64,
    pub attributes: u16,
    pub first_cluster: u32,
    pub size: u64,
    pub name: String,
}

impl ExInode {
    pub fn from_record(i_num: u64, fr: &FileRecord) -> Self {
        Self {
            i_num,
            attributes: fr.attributes,
            first_cluster: fr.first_cluster,
            size: fr.size,
            name: fr.name.clone(),
        }
    }

    #[inline]
    pub fn size(&self) -> u64 {
        self.size
    }
    #[inline]
    pub fn is_dir(&self) -> bool {
        (self.attributes & 0x0010) != 0
    }
    #[inline]
    pub fn is_regular_file(&self) -> bool {
        !self.is_dir()
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }

    pub fn to_string(&self) -> String {
        let mut t = Table::new();
        t.add_row(Row::new(vec![
            Cell::new("Identifier"),
            Cell::new(&format!("0x{:x}", self.i_num)),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Attributes"),
            Cell::new(&format!("0x{:04x}", self.attributes)),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("First cluster"),
            Cell::new(&format!("{}", self.first_cluster)),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Size"),
            Cell::new(&format!("{}", self.size)),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Dir?"),
            Cell::new(&format!("{}", self.is_dir())),
        ]));
        t.add_row(Row::new(vec![Cell::new("Name"), Cell::new(&self.name)]));
        t.to_string()
    }
}
