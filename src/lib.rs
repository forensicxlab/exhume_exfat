pub mod bpb;
pub mod direntry;
pub mod fat;
pub mod fs;

// add:
pub mod compat;
pub mod exinode; // fake-inode compatibility // ext-like DirEntry wrapper

pub use crate::bpb::BootSector;
pub use crate::fs::ExFatFS;
