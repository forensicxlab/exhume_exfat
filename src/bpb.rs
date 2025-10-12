use log::debug;
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
}

impl BootSector {
    pub fn from_bytes(bs: &[u8]) -> Result<Self, String> {
        if bs.len() < 512 {
            return Err(format!("Boot sector too short: {} < 512 bytes", bs.len()));
        }

        let read_u16 = |o: usize| -> Result<u16, String> {
            bs.get(o..o + 2)
                .ok_or_else(|| format!("BS bounds error @0x{:X}+2", o))
                .and_then(|s| Ok(u16::from_le_bytes([s[0], s[1]])))
        };
        let read_u32 = |o: usize| -> Result<u32, String> {
            bs.get(o..o + 4)
                .ok_or_else(|| format!("BS bounds error @0x{:X}+4", o))
                .and_then(|s| Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]])))
        };
        let read_u64 = |o: usize| -> Result<u64, String> {
            bs.get(o..o + 8)
                .ok_or_else(|| format!("BS bounds error @0x{:X}+8", o))
                .and_then(|s| {
                    Ok(u64::from_le_bytes([
                        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
                    ]))
                })
        };

        let mut oem_name = [0u8; 8];
        oem_name.copy_from_slice(&bs[3..11]);

        // 0x55AA signature must be present
        let bs55aa = read_u16(510)?;
        if bs55aa != 0xAA55 {
            return Err("Invalid boot signature (0x55AA missing)".into());
        }

        let me = Self {
            oem_name,
            partition_offset: read_u64(0x40)?,
            volume_length: read_u64(0x48)?,
            fat_offset: read_u32(0x50)?,
            fat_length: read_u32(0x54)?,
            cluster_heap_offset: read_u32(0x58)?,
            cluster_count: read_u32(0x5C)?,
            root_dir_first_cluster: read_u32(0x60)?,
            volume_serial: read_u32(0x64)?,
            fs_revision: read_u16(0x68)?,
            volume_flags: read_u16(0x6A)?,
            bytes_per_sector_shift: bs[0x6C],
            sectors_per_cluster_shift: bs[0x6D],
            num_fats: bs[0x6E],
            drive_select: bs[0x6F],
        };

        if &me.oem_name != b"EXFAT   " {
            return Err(format!(
                "OEM name isn't EXFAT: {:?}",
                String::from_utf8_lossy(&me.oem_name)
            ));
        }

        if me.fs_revision != 0x0100 {
            return Err(format!(
                "Unexpected exFAT fs_revision=0x{:04X}",
                me.fs_revision
            ));
        }

        if me.num_fats != 1 {
            return Err(format!("num_fats={} (exFAT requires 1)", me.num_fats));
        }

        if me.volume_length == 0 {
            return Err("volume_length is zero".into());
        }
        if me.fat_offset == 0 || me.fat_length == 0 {
            return Err(format!(
                "Invalid FAT location/length: offset={}, length={}",
                me.fat_offset, me.fat_length
            ));
        }
        if me.cluster_heap_offset == 0 || me.cluster_count < 2 {
            return Err(format!(
                "Invalid cluster heap: offset={}, count={}",
                me.cluster_heap_offset, me.cluster_count
            ));
        }
        if me.root_dir_first_cluster < 2 {
            return Err(format!(
                "Invalid root_dir_first_cluster: {} (<2)",
                me.root_dir_first_cluster
            ));
        }

        if !(9..=12).contains(&me.bytes_per_sector_shift) {
            return Err(format!(
                "bytes_per_sector_shift={} not in [9..12]",
                me.bytes_per_sector_shift
            ));
        }
        if me.sectors_per_cluster_shift > 25 {
            return Err(format!(
                "sectors_per_cluster_shift={} too large",
                me.sectors_per_cluster_shift
            ));
        }
        let bps = me.bytes_per_sector();
        let spc = me.sectors_per_cluster();
        let bpc = me.bytes_per_cluster();

        if bpc < 4096 || bpc > 32 * 1024 * 1024 {
            return Err(format!("bytes_per_cluster={} outside [4KiB..32MiB]", bpc));
        }

        let fat_end_sector = me.fat_offset as u64 + me.fat_length as u64;
        if fat_end_sector > me.cluster_heap_offset as u64 {
            return Err(format!(
                "FAT overlaps cluster heap: FAT end={} >= heap offset={}",
                fat_end_sector, me.cluster_heap_offset
            ));
        }

        let first_data_cluster = 2u64;
        let last_cluster = me.cluster_count as u64 + first_data_cluster - 1;
        if (me.root_dir_first_cluster as u64) < first_data_cluster
            || (me.root_dir_first_cluster as u64) > last_cluster
        {
            return Err(format!(
                "root_dir_first_cluster={} out of range [2..{}]",
                me.root_dir_first_cluster, last_cluster
            ));
        }

        debug!("BPB OK: bps={} spc={} bpc={}", bps, spc, bpc);

        Ok(me)
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

    // Currently unused outside this module, keep for completeness.
    #[allow(dead_code)]
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
        t.to_string()
    }
}
