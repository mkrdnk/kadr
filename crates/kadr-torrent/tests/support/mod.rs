use std::path::Path;

use bytes::Bytes;
use sha1::{Digest, Sha1};

pub const PIECE: usize = 16 * 1024;
pub const PREFIX: usize = 777;
pub const SELECTED: usize = 1024 * 1024 + 117;
pub const MISSING: usize = 7 * PIECE - PREFIX;

/// No downloaded fixtures: canonical bencode and SHA-1 over deterministic bytes.
/// The selected file starts and ends inside pieces shared with other files.
pub struct Fixture {
    pub torrent: Bytes,
    pub selected: Vec<u8>,
    all: Vec<u8>,
}

fn string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(format!("{}:", bytes.len()).as_bytes());
    out.extend_from_slice(bytes);
}

fn integer(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(format!("i{value}e").as_bytes());
}

impl Fixture {
    pub fn new(private: bool) -> Self {
        let total = PREFIX + SELECTED + 3 * PIECE;
        let all: Vec<u8> = (0..total)
            .map(|n| ((n * 31 + n / 251 + 19) % 256) as u8)
            .collect();
        let hashes: Vec<u8> = all
            .chunks(PIECE)
            .flat_map(|chunk| Sha1::digest(chunk).to_vec())
            .collect();
        let mut torrent = Vec::new();
        // A tracker URL is present but disabled by the wrapper. This is not an
        // observing listener or a syscall-level no-egress assertion.
        torrent.extend_from_slice(b"d8:announce");
        string(&mut torrent, b"http://127.0.0.1:1/disabled-tracker");
        torrent.extend_from_slice(b"4:infod5:filesl");
        for (name, length) in [
            ("prefix.bin", PREFIX),
            ("selected.bin", SELECTED),
            ("tail.bin", 3 * PIECE),
        ] {
            torrent.extend_from_slice(b"d6:length");
            integer(&mut torrent, length);
            torrent.extend_from_slice(b"4:pathl");
            string(&mut torrent, name.as_bytes());
            torrent.extend_from_slice(b"ee");
        }
        torrent.extend_from_slice(b"e4:name");
        string(&mut torrent, b"kadr-fixture");
        torrent.extend_from_slice(b"12:piece length");
        integer(&mut torrent, PIECE);
        torrent.extend_from_slice(b"6:pieces");
        string(&mut torrent, &hashes);
        if private {
            torrent.extend_from_slice(b"7:privatei1e");
        }
        torrent.extend_from_slice(b"ee");
        Self {
            torrent: torrent.into(),
            selected: all[PREFIX..PREFIX + SELECTED].to_vec(),
            all,
        }
    }

    pub fn write_partial_seed(&self, output: &Path) {
        let dir = output.join("kadr-fixture");
        std::fs::create_dir_all(&dir).unwrap();
        let mut selected = self.selected.clone();
        // Whole interior pieces are intentionally bad: upstream must rehash and
        // refuse to expose them. Beginning/middle/end remain available.
        selected[MISSING..MISSING + 2 * PIECE].fill(0);
        std::fs::write(dir.join("prefix.bin"), &self.all[..PREFIX]).unwrap();
        std::fs::write(dir.join("selected.bin"), selected).unwrap();
        std::fs::write(dir.join("tail.bin"), &self.all[PREFIX + SELECTED..]).unwrap();
    }
}
