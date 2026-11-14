//! SipHash-2-4 with a 128-bit output and fixed keys: a stable, well-mixed
//! hash for comparing trees. It is not a cryptographic commitment.

const K0: u64 = 0x5361_7565_7274_6569; // "Sauerteig"
const K1: u64 = 0x6f63_7472_6565_2031; // "octree 1"

pub(crate) struct Hasher {
    v: [u64; 4],
    tail: u64,
    ntail: usize,
    length: usize,
}

impl Hasher {
    pub(crate) fn new() -> Self {
        Self {
            v: [
                K0 ^ 0x736f_6d65_7073_6575,
                K1 ^ 0x646f_7261_6e64_6f6d ^ 0xee,
                K0 ^ 0x6c79_6765_6e65_7261,
                K1 ^ 0x7465_6462_7974_6573,
            ],
            tail: 0,
            ntail: 0,
            length: 0,
        }
    }

    fn round(&mut self) {
        let v = &mut self.v;
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[2] = v[2].rotate_left(32);
    }

    fn block(&mut self, m: u64) {
        self.v[3] ^= m;
        self.round();
        self.round();
        self.v[0] ^= m;
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) {
        self.length += bytes.len();
        for &byte in bytes {
            self.tail |= u64::from(byte) << (8 * self.ntail);
            self.ntail += 1;
            if self.ntail == 8 {
                let m = self.tail;
                self.block(m);
                self.tail = 0;
                self.ntail = 0;
            }
        }
    }

    pub(crate) fn finish(mut self) -> [u8; 16] {
        let last = self.tail | ((self.length as u64 & 0xff) << 56);
        self.block(last);
        self.v[2] ^= 0xee;
        for _ in 0..4 {
            self.round();
        }
        let a = self.v[0] ^ self.v[1] ^ self.v[2] ^ self.v[3];
        self.v[1] ^= 0xdd;
        for _ in 0..4 {
            self.round();
        }
        let b = self.v[0] ^ self.v[1] ^ self.v[2] ^ self.v[3];
        let mut out = [0; 16];
        out[..8].copy_from_slice(&a.to_le_bytes());
        out[8..].copy_from_slice(&b.to_le_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference vectors of SipHash-2-4-128 use key 00..0f; check the
    /// rounds against them with that key.
    #[test]
    fn matches_the_reference_vectors() {
        let k0 = u64::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7]);
        let k1 = u64::from_le_bytes([8, 9, 10, 11, 12, 13, 14, 15]);
        let hash = |len: usize| {
            let mut h = Hasher::new();
            h.v = [
                k0 ^ 0x736f_6d65_7073_6575,
                k1 ^ 0x646f_7261_6e64_6f6d ^ 0xee,
                k0 ^ 0x6c79_6765_6e65_7261,
                k1 ^ 0x7465_6462_7974_6573,
            ];
            let message: Vec<u8> = (0..len as u8).collect();
            h.write(&message);
            h.finish()
        };
        assert_eq!(
            hash(0),
            [
                0xa3, 0x81, 0x7f, 0x04, 0xba, 0x25, 0xa8, 0xe6, 0x6d, 0xf6, 0x72, 0x14, 0xc7, 0x55,
                0x02, 0x93
            ]
        );
        assert_eq!(
            hash(1),
            [
                0xda, 0x87, 0xc1, 0xd8, 0x6b, 0x99, 0xaf, 0x44, 0x34, 0x76, 0x59, 0x11, 0x9b, 0x22,
                0xfc, 0x45
            ]
        );
    }
}
