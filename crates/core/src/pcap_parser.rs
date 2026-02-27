use std::{
    fs::File,
    io::{self, BufReader, ErrorKind, Read, Result},
    path::Path,
};

// --- Constants ---
// Magic Numbers (Literal Byte Arrays)
// These represent the exact byte sequence expected at the beginning of the file.
const MAGIC_BYTES_LE: [u8; 4] = [0xd4, 0xc3, 0xb2, 0xa1]; // For 0xd4c3b2a1 (Little Endian)
const MAGIC_BYTES_NANO_LE: [u8; 4] = [0x4d, 0x3c, 0xb2, 0xa1]; // For 0x4d3cb2a1 (LE, Nano)
const MAGIC_BYTES_BE: [u8; 4] = [0xa1, 0xb2, 0xc3, 0xd4]; // For 0xa1b2c3d4 (Big Endian)
const MAGIC_BYTES_NANO_BE: [u8; 4] = [0xa1, 0xb2, 0x3c, 0x4d]; // For 0xa1b23c4d (BE, Nano)

// Header Sizes
const GLOBAL_HEADER_SIZE: usize = 24;
const PACKET_HEADER_SIZE: usize = 16;

/// An extremely simple iterator over raw data blocks (assumed Ethernet frames) from a PCAP file.
///
/// Uses only `std` library. Reads the minimum required to extract packet data blobs based on their length.
/// Correctly handles both Little-Endian and Big-Endian PCAP files based on magic number bytes.
pub struct MinimalPcapIterator {
    reader: BufReader<File>,
    is_little_endian: bool, // Simple flag for byte order
}

impl MinimalPcapIterator {
    /// Creates the iterator. Reads only the magic number bytes to determine byte order,
    /// then skips the rest of the global header.
    pub fn new<P: AsRef<Path>>(filepath: P) -> Result<Self> {
        let file = File::open(filepath)?;
        let mut reader = BufReader::new(file);

        // 1. Read Magic Number (first 4 bytes)
        let mut magic_buf = [0u8; 4];
        reader.read_exact(&mut magic_buf)?; // Reads the first 4 bytes from file

        tracing::info!("Magic number bytes: {magic_buf:02x?}");

        // 2. Determine endianness by comparing the actual bytes read to known patterns
        let is_little_endian = match magic_buf {
            // Compare against the literal byte patterns
            MAGIC_BYTES_LE | MAGIC_BYTES_NANO_LE => true,
            MAGIC_BYTES_BE | MAGIC_BYTES_NANO_BE => false,
            _ => {
                // Unrecognized byte pattern for the magic number
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("Invalid or unsupported PCAP magic number bytes: {magic_buf:02x?}"),
                ));
            }
        };

        tracing::info!(
            "Endianness: {}",
            if is_little_endian { "Little" } else { "Big" },
        );

        // 3. Skip the rest of the global header (24 total - 4 read = 20 bytes)
        let mut discard_buf = [0u8; GLOBAL_HEADER_SIZE - 4];
        reader.read_exact(&mut discard_buf)?;

        tracing::info!("Discarded global header bytes: {discard_buf:02x?}");

        Ok(Self {
            reader,
            is_little_endian,
        })
    }
}

impl Iterator for MinimalPcapIterator {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut packet_header_buf = [0u8; PACKET_HEADER_SIZE];

        // 1. Read the 16-byte packet header.
        // Check for EOF specifically, as it indicates the end of the file/iteration.
        if let Err(e) = self.reader.read_exact(&mut packet_header_buf) {
            if e.kind() == ErrorKind::UnexpectedEof {
                // Reached the end of the file while trying to read a header,
                // this is the normal termination condition for the iterator.
                return None;
            }
            panic!("Error reading packet header: {e}");
        }

        let incl_len_bytes: [u8; 4] = packet_header_buf[8..12].try_into().unwrap();
        let incl_len = if self.is_little_endian {
            u32::from_le_bytes(incl_len_bytes)
        } else {
            u32::from_be_bytes(incl_len_bytes)
        };

        // 3. Read exactly `incl_len` bytes of packet data.
        let mut packet_data = vec![0u8; incl_len as usize];
        if incl_len > 0 {
            self.reader.read_exact(&mut packet_data).unwrap();
        }

        Some(packet_data)
    }
}
