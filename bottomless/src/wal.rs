use anyhow::{anyhow, Result};
use std::io::SeekFrom;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWrite};

#[repr(transparent)]
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WalFrameHeader([u8; WalFrameHeader::SIZE]);

impl WalFrameHeader {
    pub const SIZE: usize = 24;

    /// In multi-page transactions, only the last page in the transaction contains
    /// the size_after_transaction field. If it's zero, it means it's an uncommited
    /// page.
    pub fn is_committed(&self) -> bool {
        self.size_after() != 0
    }

    /// Page number
    pub fn pgno(&self) -> u32 {
        u32::from_be_bytes([self.0[0], self.0[1], self.0[2], self.0[3]])
    }

    /// For commit records, the size of the database image in pages
    /// after the commit. For all other records, zero.
    pub fn size_after(&self) -> u32 {
        u32::from_be_bytes([self.0[4], self.0[5], self.0[6], self.0[7]])
    }

    #[allow(dead_code)]
    pub fn salt(&self) -> u64 {
        u64::from_be_bytes([
            self.0[8], self.0[9], self.0[10], self.0[11], self.0[12], self.0[13], self.0[14],
            self.0[15],
        ])
    }

    pub fn crc(&self) -> (u32, u32) {
        (
            u32::from_be_bytes([self.0[16], self.0[17], self.0[18], self.0[19]]),
            u32::from_be_bytes([self.0[20], self.0[21], self.0[22], self.0[23]]),
        )
    }

    /// Check the frame's checksum, chained from `init_crc`: the WAL header's
    /// checksum for the first frame, the previous frame's after that.
    /// `big_endian` is the WAL's checksum byte order (see
    /// [`WalHeader::big_endian_checksums`]).
    pub fn verify(
        &self,
        init_crc: (u32, u32),
        page_data: &[u8],
        big_endian: bool,
    ) -> Result<(u32, u32)> {
        let mut crc = init_crc;
        crc = checksum_step(crc, &self.0[0..8], big_endian);
        crc = checksum_step(crc, page_data, big_endian);
        let frame_crc = self.crc();
        if crc == frame_crc {
            Ok(crc)
        } else {
            Err(anyhow!(
                "Frame checksum verification failed for page no. {}. Expected: {:X}-{:X}. Got: {:X}-{:X}",
                self.pgno(),
                frame_crc.0,
                frame_crc.1,
                crc.0,
                crc.1,
            ))
        }
    }
}

impl From<[u8; WalFrameHeader::SIZE]> for WalFrameHeader {
    fn from(value: [u8; WalFrameHeader::SIZE]) -> Self {
        WalFrameHeader(value)
    }
}

impl From<WalFrameHeader> for [u8; WalFrameHeader::SIZE] {
    fn from(h: WalFrameHeader) -> [u8; WalFrameHeader::SIZE] {
        h.0
    }
}

impl AsRef<[u8]> for WalFrameHeader {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

#[repr(C, packed)]
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct WalHeader {
    /// Magic number. 0x377f0682 or 0x377f0683
    pub magic_no: u32,
    /// File format version. Currently 3007000
    pub version: u32,
    /// Database page size.
    pub page_size: u32,
    /// Checkpoint sequence number
    pub checkpoint_seq_no: u32,
    /// Random integer incremented with each checkpoint
    pub salt_1: u32,
    /// A different random integer changing with each checkpoint
    pub salt_2: u32,
    /// Checksum for first 24 bytes of header
    pub checksum_1: u32,
    pub checksum_2: u32,
}

impl WalHeader {
    pub const SIZE: u64 = 32;

    /// SQLite writes a WAL's checksums in the byte order of the machine that
    /// made it, and says which in the magic number's last bit: `0x377f0683`
    /// big-endian, `0x377f0682` little-endian.
    pub fn big_endian_checksums(&self) -> bool {
        self.magic_no & 1 == 1
    }
}

impl From<[u8; WalHeader::SIZE as usize]> for WalHeader {
    fn from(v: [u8; WalHeader::SIZE as usize]) -> Self {
        WalHeader {
            magic_no: u32::from_be_bytes([v[0], v[1], v[2], v[3]]),
            version: u32::from_be_bytes([v[4], v[5], v[6], v[7]]),
            page_size: u32::from_be_bytes([v[8], v[9], v[10], v[11]]),
            checkpoint_seq_no: u32::from_be_bytes([v[12], v[13], v[14], v[15]]),
            salt_1: u32::from_be_bytes([v[16], v[17], v[18], v[19]]),
            salt_2: u32::from_be_bytes([v[20], v[21], v[22], v[23]]),
            checksum_1: u32::from_be_bytes([v[24], v[25], v[26], v[27]]),
            checksum_2: u32::from_be_bytes([v[28], v[29], v[30], v[31]]),
        }
    }
}

#[derive(Debug)]
pub(crate) struct WalFileReader {
    file: File,
    header: WalHeader,
}

impl WalFileReader {
    pub async fn open<P: AsRef<Path>>(fpath: P) -> Result<Option<Self>> {
        let mut file = File::open(fpath).await?;
        let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
        if len < WalHeader::SIZE {
            return Ok(None);
        }
        let header = {
            let mut buf = [0u8; WalHeader::SIZE as usize];
            file.read_exact(buf.as_mut()).await?;
            WalHeader::from(buf)
        };
        Ok(Some(WalFileReader { file, header }))
    }

    /// Returns page size stored in WAL file header.
    pub fn page_size(&self) -> u32 {
        self.header.page_size
    }

    pub fn checksum(&self) -> (u32, u32) {
        (self.header.checksum_1, self.header.checksum_2)
    }

    /// The byte order of this WAL's checksums.
    pub fn big_endian_checksums(&self) -> bool {
        self.header.big_endian_checksums()
    }

    pub fn frame_size(&self) -> u64 {
        (WalFrameHeader::SIZE as u64) + (self.page_size() as u64)
    }

    /// Returns an offset in a WAL file, where the data of a frame with given number starts.
    pub fn offset(&self, frame_no: u32) -> u64 {
        WalHeader::SIZE + ((frame_no - 1) as u64) * self.frame_size()
    }

    /// Returns a number of pages stored in current WAL file.
    pub async fn frame_count(&self) -> u32 {
        let len = self.file.metadata().await.map(|m| m.len()).unwrap_or(0);
        if len < WalHeader::SIZE {
            0
        } else {
            ((len - WalHeader::SIZE) / self.frame_size()) as u32
        }
    }

    /// Sets a file cursor at the beginning of a frame with given number.
    pub async fn seek_frame(&mut self, frame_no: u32) -> Result<()> {
        let offset = self.offset(frame_no);
        self.file.seek(SeekFrom::Start(offset)).await?;
        Ok(())
    }

    /// Reads a header of a WAL frame, without reading the entire page that frame is
    /// responsible for.
    ///
    /// For reading specific frame use [WalFileReader::seek_frame] before calling this method.
    pub async fn read_frame_header(&mut self) -> Result<Option<WalFrameHeader>> {
        let mut header = [0u8; WalFrameHeader::SIZE];
        match self.file.read_exact(header.as_mut()).await {
            Ok(_) => Ok(Some(WalFrameHeader::from(header))),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn copy_frames<W>(&mut self, w: &mut W, frame_count: usize) -> Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        //TODO - specialize non-compressed file cloning:
        //   libc::copy_file_range(wal.as_mut(), wal.offset(frame), out, 0, len)
        let len = (frame_count as u64) * self.frame_size();
        let h = self.file.try_clone().await?;
        let mut range = h.take(len);
        tokio::io::copy(&mut range, w).await?;
        Ok(())
    }

    /// Reads a range of next consecutive frames, including headers, into given buffer.
    /// Returns a number of frames read this way.
    ///
    /// # Errors
    ///
    /// This function will propagate any WAL file I/O errors.
    /// It will return an error if provided `buf` length is not multiplication of an underlying
    /// WAL frame size.
    /// It will return an error if at least one frame was not fully read.
    #[allow(dead_code)]
    pub async fn read_frame_range(&mut self, buf: &mut [u8]) -> Result<usize> {
        let frame_size = self.frame_size() as usize;
        if buf.len() % frame_size != 0 {
            return Err(anyhow!("Provided buffer doesn't fit full frames"));
        }
        let read = self.file.read_exact(buf).await?;
        if read % frame_size != 0 {
            Err(anyhow!("Some of the read frames where not complete"))
        } else {
            Ok(read / frame_size)
        }
    }

    #[allow(dead_code)]
    pub async fn next_frame(&mut self, page: &mut [u8]) -> Result<Option<WalFrameHeader>> {
        debug_assert_eq!(page.len(), self.page_size() as usize);
        let header = self.read_frame_header().await?;
        if header.is_some() {
            self.file.read_exact(page).await?;
        }
        Ok(header)
    }
}

impl AsMut<File> for WalFileReader {
    fn as_mut(&mut self) -> &mut File {
        &mut self.file
    }
}

/// Generate or extend an 8 byte checksum based on the data in `page` and the
/// `init` value, as https://www.sqlite.org/fileformat.html#checksum_algorithm
/// describes. `page` size must be a multiple of 8. The words are read in the
/// WAL's byte order, `big_endian` or not, which the magic number in its header
/// gives: not the byte order of the machine doing the reading, which may be
/// restoring a backup another machine made.
pub fn checksum_step(init: (u32, u32), page: &[u8], big_endian: bool) -> (u32, u32) {
    debug_assert_eq!(page.len() % 8, 0);
    let word = |bytes: &[u8]| {
        let bytes = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        }
    };
    let (mut s0, mut s1) = init;
    for pair in page.chunks_exact(8) {
        s0 = s0.wrapping_add(word(&pair[0..4])).wrapping_add(s1);
        s1 = s1.wrapping_add(word(&pair[4..8])).wrapping_add(s0);
    }
    (s0, s1)
}

#[cfg(test)]
mod test {
    use crate::wal::{checksum_step, WalFrameHeader, WalHeader};

    #[test]
    fn checksums_read_words_in_the_wal_byte_order() {
        // Two words, 1 and 2 when read big-endian.
        let data = [0, 0, 0, 1, 0, 0, 0, 2];
        // s0 = 0 + 1 + 0, s1 = 0 + 2 + s0
        assert_eq!(checksum_step((0, 0), &data, true), (1, 3));
        assert_eq!(
            checksum_step((0, 0), &data, false),
            (0x0100_0000, 0x0300_0000)
        );
        // The same bytes at an odd address give the same sum.
        let mut shifted = [0u8; 9];
        shifted[1..].copy_from_slice(&data);
        assert_eq!(checksum_step((0, 0), &shifted[1..], true), (1, 3));
    }

    /// A WAL as SQLite writes it on this machine: the header's checksum and
    /// every frame's verify in the byte order its magic number gives, and not
    /// in the other.
    #[test]
    fn a_wal_written_by_sqlite_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "wal").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute_batch("CREATE TABLE t (x);").unwrap();
        for _ in 0..50 {
            conn.execute("INSERT INTO t VALUES (randomblob(300))", [])
                .unwrap();
        }
        let wal = std::fs::read(dir.path().join("db-wal")).unwrap();
        drop(conn);

        let header = WalHeader::from(<[u8; 32]>::try_from(&wal[..32]).unwrap());
        let big_endian = header.big_endian_checksums();
        assert_eq!(big_endian, cfg!(target_endian = "big"));
        assert_eq!(
            checksum_step((0, 0), &wal[..24], big_endian),
            (header.checksum_1, header.checksum_2)
        );

        let page_size = header.page_size as usize;
        let frame_size = WalFrameHeader::SIZE + page_size;
        let frames: Vec<_> = wal[32..].chunks_exact(frame_size).collect();
        assert!(frames.len() > 2, "{} frames", frames.len());
        let mut crc = (header.checksum_1, header.checksum_2);
        for frame in &frames {
            let (head, page) = frame.split_at(WalFrameHeader::SIZE);
            let head = WalFrameHeader::from(<[u8; 24]>::try_from(head).unwrap());
            assert!(head.verify(crc, page, !big_endian).is_err());
            crc = head.verify(crc, page, big_endian).unwrap();
        }
    }

    #[test]
    fn wal_header_mem_mapping() {
        // copied from actual SQLite WAL file
        let source = [
            55, 127, 6, 130, 0, 45, 226, 24, 0, 0, 16, 0, 0, 0, 0, 0, 190, 6, 47, 124, 39, 191, 98,
            92, 81, 22, 9, 209, 101, 96, 160, 157,
        ];
        let expected = WalHeader {
            magic_no: 0x377f0682,
            version: 3007000,
            page_size: 4096,
            checkpoint_seq_no: 0,
            salt_1: 3188076412,
            salt_2: 666853980,
            checksum_1: 1360398801,
            checksum_2: 1700831389,
        };
        let actual = WalHeader::from(source);
        assert_eq!(actual, expected);
    }
}
