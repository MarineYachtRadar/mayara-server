//! MRR (MaYaRa Radar Recording) file format implementation.
//!
//! Binary format for recording and playing back radar data.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::time::{SystemTime, UNIX_EPOCH};

/// Magic bytes for MRR file header
pub const MRR_MAGIC: [u8; 4] = *b"MRR1";

/// Magic bytes for MRR file footer
pub const MRR_FOOTER_MAGIC: [u8; 4] = *b"MRRF";

/// Current format version.
///
/// Version 2 stores the source radar's full identity (brand, legend-shaping
/// flags, controls) in the capabilities JSON so playback can reproduce the
/// exact `RadarInfo` shape live recording had. V1 files are rejected — they
/// rendered with a generic Brand::Playback legend and never matched the
/// source radar visually. Re-record old captures.
pub const MRR_VERSION: u16 = 2;

/// Header size in bytes (fixed)
pub const HEADER_SIZE: usize = 256;

/// Footer size in bytes (fixed)
pub const FOOTER_SIZE: usize = 32;

/// Index entry size in bytes
pub const INDEX_ENTRY_SIZE: usize = 16;

/// Frame flags
pub const FRAME_FLAG_HAS_STATE: u8 = 0x01;

/// File header (256 bytes fixed size)
#[derive(Debug, Clone)]
pub struct MrrHeader {
    pub version: u16,
    pub flags: u16,
    /// Radar brand (Brand enum numeric ID)
    pub radar_brand: u32,
    /// Spokes per revolution (e.g., 2048)
    pub spokes_per_rev: u32,
    /// Maximum spoke length in pixels
    pub max_spoke_len: u32,
    /// Number of pixel values
    pub pixel_values: u32,
    /// Recording start time (Unix timestamp in milliseconds)
    pub start_time_ms: u64,
    /// Offset to capabilities JSON in file
    pub capabilities_offset: u64,
    pub capabilities_len: u32,
    /// Offset to initial state JSON in file
    pub initial_state_offset: u64,
    pub initial_state_len: u32,
    /// Offset to first frame
    pub frames_offset: u64,
}

impl Default for MrrHeader {
    fn default() -> Self {
        Self {
            version: MRR_VERSION,
            flags: 0,
            radar_brand: 0,
            spokes_per_rev: 0,
            max_spoke_len: 0,
            pixel_values: 0,
            start_time_ms: 0,
            capabilities_offset: 0,
            capabilities_len: 0,
            initial_state_offset: 0,
            initial_state_len: 0,
            frames_offset: 0,
        }
    }
}

impl MrrHeader {
    pub fn write<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let mut buf = [0u8; HEADER_SIZE];

        buf[0..4].copy_from_slice(&MRR_MAGIC);
        buf[4..6].copy_from_slice(&self.version.to_le_bytes());
        buf[6..8].copy_from_slice(&self.flags.to_le_bytes());
        buf[8..12].copy_from_slice(&self.radar_brand.to_le_bytes());
        buf[12..16].copy_from_slice(&self.spokes_per_rev.to_le_bytes());
        buf[16..20].copy_from_slice(&self.max_spoke_len.to_le_bytes());
        buf[20..24].copy_from_slice(&self.pixel_values.to_le_bytes());
        buf[24..32].copy_from_slice(&self.start_time_ms.to_le_bytes());
        buf[32..40].copy_from_slice(&self.capabilities_offset.to_le_bytes());
        buf[40..44].copy_from_slice(&self.capabilities_len.to_le_bytes());
        buf[44..52].copy_from_slice(&self.initial_state_offset.to_le_bytes());
        buf[52..56].copy_from_slice(&self.initial_state_len.to_le_bytes());
        buf[56..64].copy_from_slice(&self.frames_offset.to_le_bytes());
        // Remaining 192 bytes are reserved (already zeroed)

        writer.write_all(&buf)
    }

    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; HEADER_SIZE];
        reader.read_exact(&mut buf)?;

        if &buf[0..4] != &MRR_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid MRR file: bad magic bytes",
            ));
        }

        let version = u16::from_le_bytes([buf[4], buf[5]]);
        if version != MRR_VERSION {
            let hint = if version < MRR_VERSION {
                " — re-record from live radar."
            } else {
                " — this server is older than the recording; upgrade Mayara."
            };
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Unsupported MRR version: {} (this build expects {}){}",
                    version, MRR_VERSION, hint
                ),
            ));
        }

        Ok(Self {
            version,
            flags: u16::from_le_bytes([buf[6], buf[7]]),
            radar_brand: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            spokes_per_rev: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
            max_spoke_len: u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]),
            pixel_values: u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]),
            start_time_ms: u64::from_le_bytes([
                buf[24], buf[25], buf[26], buf[27], buf[28], buf[29], buf[30], buf[31],
            ]),
            capabilities_offset: u64::from_le_bytes([
                buf[32], buf[33], buf[34], buf[35], buf[36], buf[37], buf[38], buf[39],
            ]),
            capabilities_len: u32::from_le_bytes([buf[40], buf[41], buf[42], buf[43]]),
            initial_state_offset: u64::from_le_bytes([
                buf[44], buf[45], buf[46], buf[47], buf[48], buf[49], buf[50], buf[51],
            ]),
            initial_state_len: u32::from_le_bytes([buf[52], buf[53], buf[54], buf[55]]),
            frames_offset: u64::from_le_bytes([
                buf[56], buf[57], buf[58], buf[59], buf[60], buf[61], buf[62], buf[63],
            ]),
        })
    }
}

/// File footer (32 bytes fixed size)
#[derive(Debug, Clone, Default)]
pub struct MrrFooter {
    pub index_offset: u64,
    pub index_count: u32,
    pub frame_count: u32,
    pub duration_ms: u64,
}

impl MrrFooter {
    pub fn write<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let mut buf = [0u8; FOOTER_SIZE];

        buf[0..4].copy_from_slice(&MRR_FOOTER_MAGIC);
        buf[4..12].copy_from_slice(&self.index_offset.to_le_bytes());
        buf[12..16].copy_from_slice(&self.index_count.to_le_bytes());
        buf[16..20].copy_from_slice(&self.frame_count.to_le_bytes());
        buf[20..28].copy_from_slice(&self.duration_ms.to_le_bytes());

        writer.write_all(&buf)
    }

    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; FOOTER_SIZE];
        reader.read_exact(&mut buf)?;

        if &buf[0..4] != &MRR_FOOTER_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid MRR footer: bad magic bytes",
            ));
        }

        Ok(Self {
            index_offset: u64::from_le_bytes([
                buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10], buf[11],
            ]),
            index_count: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
            frame_count: u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]),
            duration_ms: u64::from_le_bytes([
                buf[20], buf[21], buf[22], buf[23], buf[24], buf[25], buf[26], buf[27],
            ]),
        })
    }
}

/// Index entry for seeking (16 bytes)
#[derive(Debug, Clone, Default)]
pub struct MrrIndexEntry {
    pub timestamp_ms: u64,
    pub file_offset: u64,
}

impl MrrIndexEntry {
    pub fn write<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.timestamp_ms.to_le_bytes())?;
        writer.write_all(&self.file_offset.to_le_bytes())
    }

    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; INDEX_ENTRY_SIZE];
        reader.read_exact(&mut buf)?;

        Ok(Self {
            timestamp_ms: u64::from_le_bytes([
                buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
            ]),
            file_offset: u64::from_le_bytes([
                buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
            ]),
        })
    }
}

/// Frame data (variable size)
#[derive(Debug, Clone)]
pub struct MrrFrame {
    pub timestamp_ms: u64,
    pub flags: u8,
    /// Protobuf RadarMessage data
    pub data: Vec<u8>,
    /// Optional state delta (JSON)
    pub state_delta: Option<Vec<u8>>,
}

/// Maximum allowed frame data size (64 MB)
const MAX_FRAME_DATA_SIZE: usize = 64 * 1024 * 1024;

impl MrrFrame {
    pub fn new(timestamp_ms: u64, data: Vec<u8>) -> Self {
        Self {
            timestamp_ms,
            flags: 0,
            data,
            state_delta: None,
        }
    }

    pub fn write<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        // Derive flag from state_delta to ensure consistency
        let flags = if self.state_delta.is_some() {
            self.flags | FRAME_FLAG_HAS_STATE
        } else {
            self.flags & !FRAME_FLAG_HAS_STATE
        };

        writer.write_all(&self.timestamp_ms.to_le_bytes())?;
        writer.write_all(&[flags])?;
        writer.write_all(&(self.data.len() as u32).to_le_bytes())?;
        writer.write_all(&self.data)?;

        if let Some(ref state) = self.state_delta {
            writer.write_all(&(state.len() as u32).to_le_bytes())?;
            writer.write_all(state)?;
        }

        Ok(())
    }

    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut ts_buf = [0u8; 8];
        reader.read_exact(&mut ts_buf)?;
        let timestamp_ms = u64::from_le_bytes(ts_buf);

        let mut flags_buf = [0u8; 1];
        reader.read_exact(&mut flags_buf)?;
        let flags = flags_buf[0];

        let mut len_buf = [0u8; 4];
        reader.read_exact(&mut len_buf)?;
        let data_len = u32::from_le_bytes(len_buf) as usize;
        if data_len > MAX_FRAME_DATA_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Frame data too large: {} bytes", data_len),
            ));
        }

        let mut data = vec![0u8; data_len];
        reader.read_exact(&mut data)?;

        let state_delta = if flags & FRAME_FLAG_HAS_STATE != 0 {
            reader.read_exact(&mut len_buf)?;
            let state_len = u32::from_le_bytes(len_buf) as usize;
            if state_len > MAX_FRAME_DATA_SIZE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("State data too large: {} bytes", state_len),
                ));
            }
            let mut state = vec![0u8; state_len];
            reader.read_exact(&mut state)?;
            Some(state)
        } else {
            None
        };

        Ok(Self {
            timestamp_ms,
            flags,
            data,
            state_delta,
        })
    }

    pub fn size(&self) -> usize {
        let base = 8 + 1 + 4 + self.data.len();
        if let Some(ref state) = self.state_delta {
            base + 4 + state.len()
        } else {
            base
        }
    }
}

/// Writer for creating MRR files
pub struct MrrWriter<W: Write + Seek> {
    writer: W,
    header: MrrHeader,
    frame_count: u32,
    last_timestamp_ms: u64,
    index: Vec<MrrIndexEntry>,
    index_interval: u32,
    frames_since_index: u32,
}

impl<W: Write + Seek> MrrWriter<W> {
    pub fn new(
        mut writer: W,
        radar_brand: u32,
        spokes_per_rev: u32,
        max_spoke_len: u32,
        pixel_values: u32,
        capabilities_json: &[u8],
        initial_state_json: &[u8],
    ) -> io::Result<Self> {
        let start_time_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let capabilities_end = HEADER_SIZE + capabilities_json.len();
        let initial_state_offset = capabilities_end as u64;
        let frames_offset = (capabilities_end + initial_state_json.len()) as u64;

        let header = MrrHeader {
            version: MRR_VERSION,
            flags: 0,
            radar_brand,
            spokes_per_rev,
            max_spoke_len,
            pixel_values,
            start_time_ms,
            capabilities_offset: HEADER_SIZE as u64,
            capabilities_len: capabilities_json.len() as u32,
            initial_state_offset,
            initial_state_len: initial_state_json.len() as u32,
            frames_offset,
        };

        header.write(&mut writer)?;
        writer.write_all(capabilities_json)?;
        writer.write_all(initial_state_json)?;

        Ok(Self {
            writer,
            header,
            frame_count: 0,
            last_timestamp_ms: 0,
            index: Vec::new(),
            index_interval: 100,
            frames_since_index: 0,
        })
    }

    pub fn write_frame(&mut self, frame: &MrrFrame) -> io::Result<()> {
        if self.frames_since_index >= self.index_interval {
            let current_pos = self.writer.stream_position()?;
            self.index.push(MrrIndexEntry {
                timestamp_ms: frame.timestamp_ms,
                file_offset: current_pos,
            });
            self.frames_since_index = 0;
        }

        frame.write(&mut self.writer)?;
        self.frame_count += 1;
        self.last_timestamp_ms = frame.timestamp_ms;
        self.frames_since_index += 1;

        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        let index_offset = self.writer.stream_position()?;
        for entry in &self.index {
            entry.write(&mut self.writer)?;
        }

        let footer = MrrFooter {
            index_offset,
            index_count: self.index.len() as u32,
            frame_count: self.frame_count,
            duration_ms: self.last_timestamp_ms,
        };
        footer.write(&mut self.writer)?;

        self.writer.seek(SeekFrom::Start(0))?;
        self.header.write(&mut self.writer)?;

        self.writer.flush()
    }
}

/// Maximum allowed metadata size (16 MB)
const MAX_METADATA_SIZE: usize = 16 * 1024 * 1024;

/// Reader for MRR files
pub struct MrrReader<R: Read + Seek> {
    reader: R,
    header: MrrHeader,
    footer: MrrFooter,
    capabilities: Vec<u8>,
    initial_state: Vec<u8>,
    /// File offset where frames end (= index start)
    frames_end_offset: u64,
}

impl<R: Read + Seek> MrrReader<R> {
    pub fn open(mut reader: R) -> io::Result<Self> {
        let header = MrrHeader::read(&mut reader)?;

        let cap_len = header.capabilities_len as usize;
        if cap_len > MAX_METADATA_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Capabilities too large: {} bytes", cap_len),
            ));
        }
        reader.seek(SeekFrom::Start(header.capabilities_offset))?;
        let mut capabilities = vec![0u8; cap_len];
        reader.read_exact(&mut capabilities)?;

        let state_len = header.initial_state_len as usize;
        if state_len > MAX_METADATA_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Initial state too large: {} bytes", state_len),
            ));
        }
        reader.seek(SeekFrom::Start(header.initial_state_offset))?;
        let mut initial_state = vec![0u8; state_len];
        reader.read_exact(&mut initial_state)?;

        reader.seek(SeekFrom::End(-(FOOTER_SIZE as i64)))?;
        let footer = MrrFooter::read(&mut reader)?;

        let frames_end_offset = footer.index_offset;

        reader.seek(SeekFrom::Start(header.frames_offset))?;

        Ok(Self {
            reader,
            header,
            footer,
            capabilities,
            initial_state,
            frames_end_offset,
        })
    }

    pub fn header(&self) -> &MrrHeader {
        &self.header
    }

    pub fn footer(&self) -> &MrrFooter {
        &self.footer
    }

    pub fn capabilities(&self) -> &[u8] {
        &self.capabilities
    }

    pub fn initial_state(&self) -> &[u8] {
        &self.initial_state
    }

    pub fn read_frame(&mut self) -> io::Result<Option<MrrFrame>> {
        if self.reader.stream_position()? >= self.frames_end_offset {
            return Ok(None);
        }

        let frame = MrrFrame::read(&mut self.reader)?;
        Ok(Some(frame))
    }

    pub fn seek_to_timestamp(&mut self, target_ms: u64) -> io::Result<()> {
        self.reader
            .seek(SeekFrom::Start(self.footer.index_offset))?;

        let mut best_entry: Option<MrrIndexEntry> = None;
        for _ in 0..self.footer.index_count {
            let entry = MrrIndexEntry::read(&mut self.reader)?;
            if entry.timestamp_ms <= target_ms {
                best_entry = Some(entry);
            } else {
                break;
            }
        }

        let seek_pos = best_entry
            .map(|e| e.file_offset)
            .unwrap_or(self.header.frames_offset);
        self.reader.seek(SeekFrom::Start(seek_pos))?;

        loop {
            let pos_before = self.reader.stream_position()?;
            let frame = self.read_frame()?;
            match frame {
                Some(f) if f.timestamp_ms >= target_ms => {
                    self.reader.seek(SeekFrom::Start(pos_before))?;
                    break;
                }
                Some(_) => continue,
                None => break,
            }
        }

        Ok(())
    }

    pub fn rewind(&mut self) -> io::Result<()> {
        self.reader
            .seek(SeekFrom::Start(self.header.frames_offset))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_header_roundtrip() {
        let header = MrrHeader {
            version: MRR_VERSION,
            flags: 0,
            radar_brand: 42,
            spokes_per_rev: 2048,
            max_spoke_len: 1024,
            pixel_values: 64,
            start_time_ms: 1234567890123,
            capabilities_offset: 256,
            capabilities_len: 100,
            initial_state_offset: 356,
            initial_state_len: 50,
            frames_offset: 406,
        };

        let mut buf = Vec::new();
        header.write(&mut buf).unwrap();
        assert_eq!(buf.len(), HEADER_SIZE);

        let mut cursor = Cursor::new(buf);
        let read_header = MrrHeader::read(&mut cursor).unwrap();

        assert_eq!(read_header.version, header.version);
        assert_eq!(read_header.radar_brand, header.radar_brand);
        assert_eq!(read_header.spokes_per_rev, header.spokes_per_rev);
        assert_eq!(read_header.start_time_ms, header.start_time_ms);
    }

    #[test]
    fn test_footer_roundtrip() {
        let footer = MrrFooter {
            index_offset: 12345678,
            index_count: 100,
            frame_count: 10000,
            duration_ms: 60000,
        };

        let mut buf = Vec::new();
        footer.write(&mut buf).unwrap();
        assert_eq!(buf.len(), FOOTER_SIZE);

        let mut cursor = Cursor::new(buf);
        let read_footer = MrrFooter::read(&mut cursor).unwrap();

        assert_eq!(read_footer.index_offset, footer.index_offset);
        assert_eq!(read_footer.index_count, footer.index_count);
        assert_eq!(read_footer.frame_count, footer.frame_count);
        assert_eq!(read_footer.duration_ms, footer.duration_ms);
    }

    #[test]
    fn test_frame_roundtrip() {
        let frame = MrrFrame::new(1000, vec![1, 2, 3, 4, 5]);

        let mut buf = Vec::new();
        frame.write(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf);
        let read_frame = MrrFrame::read(&mut cursor).unwrap();

        assert_eq!(read_frame.timestamp_ms, frame.timestamp_ms);
        assert_eq!(read_frame.data, frame.data);
        assert!(read_frame.state_delta.is_none());
    }

    #[test]
    fn test_frame_with_state_roundtrip() {
        let frame = MrrFrame {
            timestamp_ms: 2000,
            flags: FRAME_FLAG_HAS_STATE,
            data: vec![1, 2, 3],
            state_delta: Some(vec![b'{', b'}']),
        };

        let mut buf = Vec::new();
        frame.write(&mut buf).unwrap();

        let mut cursor = Cursor::new(buf);
        let read_frame = MrrFrame::read(&mut cursor).unwrap();

        assert_eq!(read_frame.timestamp_ms, frame.timestamp_ms);
        assert_eq!(read_frame.data, frame.data);
        assert_eq!(read_frame.state_delta, Some(vec![b'{', b'}']));
    }

    #[test]
    fn test_writer_reader_roundtrip() {
        let capabilities = br#"{"controls":["range","gain"]}"#;
        let initial_state = br#"{"range":1000,"gain":50}"#;

        let mut raw = Cursor::new(Vec::new());
        {
            let mut writer =
                MrrWriter::new(&mut raw, 1, 2048, 1024, 64, capabilities, initial_state).unwrap();

            for i in 0..250u64 {
                let frame = MrrFrame::new(i * 100, vec![i as u8; 10]);
                writer.write_frame(&frame).unwrap();
            }
            writer.finish().unwrap();
        }

        // Read it back
        raw.set_position(0);
        let mut reader = MrrReader::open(raw).unwrap();

        assert_eq!(reader.header().radar_brand, 1);
        assert_eq!(reader.header().spokes_per_rev, 2048);
        assert_eq!(reader.header().max_spoke_len, 1024);
        assert_eq!(reader.footer().frame_count, 250);
        assert_eq!(reader.capabilities(), capabilities);
        assert_eq!(reader.initial_state(), initial_state);

        // Verify first frame
        let first = reader.read_frame().unwrap().unwrap();
        assert_eq!(first.timestamp_ms, 0);
        assert_eq!(first.data, vec![0u8; 10]);

        // Verify second frame
        let second = reader.read_frame().unwrap().unwrap();
        assert_eq!(second.timestamp_ms, 100);
        assert_eq!(second.data, vec![1u8; 10]);
    }

    #[test]
    fn test_rejects_v1_recordings() {
        let mut buf = vec![0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(&MRR_MAGIC);
        buf[4..6].copy_from_slice(&1u16.to_le_bytes()); // V1
        let err = MrrHeader::read(&mut Cursor::new(buf)).unwrap_err();
        assert!(
            err.to_string().contains("Unsupported MRR version: 1"),
            "expected v1 rejection, got: {}",
            err
        );
    }
}
