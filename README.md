# Exhume exFAT

The Exhume exFAT documentation is maintained at [forensicxlab.com](https://www.forensicxlab.com/docs/exhume_exfat/getting-started).

## 0.1.13

- Add `ExFatFS::read_file_range` and `read_inode_range` for bounded logical reads
  without loading a whole file or cluster. Sequential reads reuse FAT traversal
  state; backward reads restart traversal.
- Parse valid data length and return zeroes for the uninitialized tail of a file.
- Report invalid, truncated, and cyclic cluster chains as range-read errors.

Ranges use byte offsets within the logical file and are clipped to its size.
Zero-length requests and requests at or beyond EOF return an empty vector.
FAT cycle detection retains visited cluster identifiers for the current chain;
memory used by that metadata grows with the chain length.

`FileRecord` now exposes `valid_data_length: Option<u64>` and
`StreamExtensionEntry` exposes `valid_data_length: u64`. Code constructing these
public structs directly must supply the new field. Parsed file records use
`Some(valid_data_length)`; old serialized file records default to `None`, which
preserves their existing behavior by treating `size` as the initialized length.
