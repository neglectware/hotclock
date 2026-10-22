//! A minimal Ogg page writer (RFC 3533) and the FLAC-in-Ogg header packets (the "Ogg FLAC"
//! mapping): enough to carry one logical stream of whole packets, one page at a time.

/// Ogg's CRC-32: polynomial 0x04C11DB7, MSB first, initial value 0, no final XOR.
fn crc_table() -> &'static [u32; 256] {
    static T: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut r = (i as u32) << 24;
            for _ in 0..8 {
                r = if r & 0x8000_0000 != 0 {
                    (r << 1) ^ 0x04C1_1DB7
                } else {
                    r << 1
                };
            }
            *e = r;
        }
        t
    })
}

pub fn crc32(data: &[u8]) -> u32 {
    let t = crc_table();
    let mut crc = 0u32;
    for &b in data {
        crc = (crc << 8) ^ t[((crc >> 24) as u8 ^ b) as usize];
    }
    crc
}

pub const FLAG_BOS: u8 = 0x02;
pub const FLAG_EOS: u8 = 0x04;

/// The most lacing segments one page can carry.
pub const MAX_SEGMENTS: usize = 255;

/// Lacing segments `packet` needs: one per 255 bytes, plus the terminating short (or zero) one.
pub fn segments_for(len: usize) -> usize {
    len / 255 + 1
}

pub struct OggWriter {
    serial: u32,
    seq: u32,
}

impl OggWriter {
    pub fn new(serial: u32) -> Self {
        Self { serial, seq: 0 }
    }

    pub fn serial(&self) -> u32 {
        self.serial
    }

    /// One page holding whole `packets` (none continued from or into another page). `granule` is the
    /// position at the end of the last packet (`-1` when no packet ends here).
    pub fn page(&mut self, flags: u8, granule: i64, packets: &[&[u8]]) -> Vec<u8> {
        let nseg: usize = packets.iter().map(|p| segments_for(p.len())).sum();
        assert!(nseg <= MAX_SEGMENTS, "page needs {nseg} segments");
        let body: usize = packets.iter().map(|p| p.len()).sum();
        let mut out = Vec::with_capacity(27 + nseg + body);
        out.extend_from_slice(b"OggS");
        out.push(0);
        out.push(flags);
        out.extend_from_slice(&granule.to_le_bytes());
        out.extend_from_slice(&self.serial.to_le_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]); // CRC, filled in below
        out.push(nseg as u8);
        for p in packets {
            let mut n = p.len();
            while n >= 255 {
                out.push(255);
                n -= 255;
            }
            out.push(n as u8);
        }
        for p in packets {
            out.extend_from_slice(p);
        }
        let crc = crc32(&out);
        out[22..26].copy_from_slice(&crc.to_le_bytes());
        self.seq = self.seq.wrapping_add(1);
        out
    }
}

/// The 34-byte STREAMINFO body for a stream of unknown length (total samples 0, MD5 zero), fixed
/// block size `block`, 24-bit stereo.
pub fn streaminfo(rate: u32, block: u16, channels: u8, bits: u8) -> [u8; 34] {
    let mut b = [0u8; 34];
    b[0..2].copy_from_slice(&block.to_be_bytes()); // min block size
    b[2..4].copy_from_slice(&block.to_be_bytes()); // max block size
                                                   // min/max frame size (3 bytes each): 0 = unknown
    let packed: u64 =
        (u64::from(rate) << 44) | (u64::from(channels - 1) << 41) | (u64::from(bits - 1) << 36); // low 36 bits: total samples = 0
    b[10..18].copy_from_slice(&packed.to_be_bytes());
    b
}

/// The first Ogg FLAC packet: mapping header, `fLaC` and the STREAMINFO metadata block. `extra_headers`
/// is the number of header packets that follow it (the Vorbis comment).
pub fn first_packet(info: &[u8; 34], extra_headers: u16) -> Vec<u8> {
    let mut p = Vec::with_capacity(51);
    p.push(0x7F);
    p.extend_from_slice(b"FLAC");
    p.push(1); // mapping version major
    p.push(0); // minor
    p.extend_from_slice(&extra_headers.to_be_bytes());
    p.extend_from_slice(b"fLaC");
    p.extend_from_slice(&[0x00, 0, 0, 34]); // STREAMINFO, not the last block, length 34
    p.extend_from_slice(info);
    p
}

/// A Vorbis comment metadata block (the last one), `comments` as `KEY=value`.
pub fn comment_packet(vendor: &str, comments: &[String]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    body.extend_from_slice(vendor.as_bytes());
    body.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for c in comments {
        body.extend_from_slice(&(c.len() as u32).to_le_bytes());
        body.extend_from_slice(c.as_bytes());
    }
    let mut p = Vec::with_capacity(4 + body.len());
    p.push(0x80 | 4); // last metadata block, VORBIS_COMMENT
    let n = body.len() as u32;
    p.extend_from_slice(&n.to_be_bytes()[1..]);
    p.extend_from_slice(&body);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_of_a_known_vector() {
        // The Ogg variant (initial value 0, no final XOR) of "123456789", checked against an independent
        // bitwise implementation.
        assert_eq!(crc32(b"123456789"), 0x89A1_897F);
    }

    #[test]
    fn a_page_has_a_valid_crc_and_lacing() {
        let mut w = OggWriter::new(7);
        let big = vec![9u8; 600];
        let pg = w.page(FLAG_BOS, 1234, &[&[1, 2, 3], &big, &[]]);
        assert_eq!(&pg[..4], b"OggS");
        assert_eq!(pg[5], FLAG_BOS);
        assert_eq!(i64::from_le_bytes(pg[6..14].try_into().unwrap()), 1234);
        // 3 bytes: 1 segment; 600: 255+255+90 = 3; empty: 1.
        assert_eq!(pg[26], 5);
        assert_eq!(&pg[27..32], &[3, 255, 255, 90, 0]);
        let stored = u32::from_le_bytes(pg[22..26].try_into().unwrap());
        let mut z = pg.clone();
        z[22..26].fill(0);
        assert_eq!(crc32(&z), stored);
        assert_eq!(pg.len(), 27 + 5 + 3 + 600);
        let next = w.page(0, 5, &[&[0]]);
        assert_eq!(u32::from_le_bytes(next[18..22].try_into().unwrap()), 1);
    }

    #[test]
    fn a_packet_that_is_a_multiple_of_255_ends_with_a_zero_segment() {
        let mut w = OggWriter::new(1);
        let p = vec![0u8; 510];
        let pg = w.page(0, 0, &[&p]);
        assert_eq!(&pg[26..30], &[3, 255, 255, 0]);
    }

    #[test]
    fn streaminfo_packs_rate_channels_and_depth() {
        let b = streaminfo(48_000, 1024, 2, 24);
        assert_eq!(&b[0..4], &[4, 0, 4, 0]);
        // 48000 = 0x0BB80 in 20 bits, channels-1 = 1 in 3 bits, bits-1 = 23 in 5 bits.
        assert_eq!(&b[10..14], &[0x0B, 0xB8, 0x03, 0x70]);
        assert_eq!(&b[14..], &[0u8; 20]);
    }
}
