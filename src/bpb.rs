use prettytable::{Cell, Row, Table};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootSector {
    pub oem_name: [u8; 8],        // 0x03 .. 0x0A (not critical for exFAT but kept)
    pub partition_offset: u64,    // 0x40
    pub volume_length: u64,       // 0x48 (in sectors)
    pub fat_offset: u32,          // 0x50 (in sectors from volume start)
    pub fat_length: u32,          // 0x54 (in sectors)
    pub cluster_heap_offset: u32, // 0x58 (in sectors)
    pub cluster_count: u32,       // 0x5C
    pub root_dir_first_cluster: u32, // 0x60
    pub volume_serial: u32,       // 0x64
    pub fs_revision: u16,         // 0x68
    pub volume_flags: u16,        // 0x6A
    pub bytes_per_sector_shift: u8, // 0x6C (2^n)
    pub sectors_per_cluster_shift: u8, // 0x6D (2^n)
    pub num_fats: u8,             // 0x6E
    pub drive_select: u8,         // 0x6F
    pub percent_in_use: u8,       // 0x70
}

impl BootSector {
    pub fn from_bytes(bs: &[u8]) -> Result<Self, String> {
        if bs.len() < 512 {
            return Err("Boot sector too short".into());
        }
        let mut oem_name = [0u8; 8];
        oem_name.copy_from_slice(&bs[3..11]);
        let le_u16 = |o: usize| -> u16 { u16::from_le_bytes(bs[o..o + 2].try_into().unwrap()) };
        let le_u32 = |o: usize| -> u32 { u32::from_le_bytes(bs[o..o + 4].try_into().unwrap()) };
        let le_u64 = |o: usize| -> u64 { u64::from_le_bytes(bs[o..o + 8].try_into().unwrap()) };

        // Simple sanity: FileSystemName should read "EXFAT   " at 0x03? (actually 0x03.. not normative here)
        // We accept any, but many images will have it.
        let bs55aa = u16::from_le_bytes(bs[510..512].try_into().unwrap());
        if bs55aa != 0xAA55 {
            return Err("Invalid boot signature (0x55AA missing)".into());
        }

        Ok(Self {
            oem_name,
            partition_offset: le_u64(0x40),
            volume_length: le_u64(0x48),
            fat_offset: le_u32(0x50),
            fat_length: le_u32(0x54),
            cluster_heap_offset: le_u32(0x58),
            cluster_count: le_u32(0x5C),
            root_dir_first_cluster: le_u32(0x60),
            volume_serial: le_u32(0x64),
            fs_revision: le_u16(0x68),
            volume_flags: le_u16(0x6A),
            bytes_per_sector_shift: bs[0x6C],
            sectors_per_cluster_shift: bs[0x6D],
            num_fats: bs[0x6E],
            drive_select: bs[0x6F],
            percent_in_use: bs[0x70],
        })
    }

    #[inline]
    pub fn bytes_per_sector(&self) -> u64 {
        1u64 << self.bytes_per_sector_shift
    }
    #[inline]
    pub fn sectors_per_cluster(&self) -> u64 {
        1u64 << self.sectors_per_cluster_shift
    }
    #[inline]
    pub fn bytes_per_cluster(&self) -> u64 {
        self.bytes_per_sector() * self.sectors_per_cluster()
    }

    #[inline]
    pub fn fat_start_byte(&self) -> u64 {
        self.fat_offset as u64 * self.bytes_per_sector()
    }
    #[inline]
    pub fn cluster_heap_start_byte(&self) -> u64 {
        self.cluster_heap_offset as u64 * self.bytes_per_sector()
    }

    #[inline]
    pub fn cluster_to_byte_offset(&self, clus: u32) -> u64 {
        // Cluster #2 is first data cluster
        let data_sector =
            self.cluster_heap_offset as u64 + (clus as u64 - 2) * self.sectors_per_cluster();
        data_sector * self.bytes_per_sector()
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }

    pub fn to_string(&self) -> String {
        let mut t = Table::new();
        t.add_row(Row::new(vec![
            Cell::new("Bytes/sector"),
            Cell::new(&self.bytes_per_sector().to_string()),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Sectors/cluster"),
            Cell::new(&self.sectors_per_cluster().to_string()),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Clusters"),
            Cell::new(&self.cluster_count.to_string()),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("FAT offset (sectors)"),
            Cell::new(&self.fat_offset.to_string()),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("FAT length (sectors)"),
            Cell::new(&self.fat_length.to_string()),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Cluster heap offset (sectors)"),
            Cell::new(&self.cluster_heap_offset.to_string()),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Root dir first cluster"),
            Cell::new(&format!("{}", self.root_dir_first_cluster)),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Volume flags"),
            Cell::new(&format!("0x{:04X}", self.volume_flags)),
        ]));
        t.add_row(Row::new(vec![
            Cell::new("Percent in use"),
            Cell::new(&format!("{}%", self.percent_in_use)),
        ]));
        t.to_string()
    }
}
