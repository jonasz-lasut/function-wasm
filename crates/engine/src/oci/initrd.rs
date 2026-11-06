//! The initramfs the VM boots: a newc cpio archive around the init binary
//! alone, with the console device node the kernel opens for its first
//! process. Written by hand - the format is a fixed header of hexadecimal
//! fields, a name and the data, each padded to four bytes - so the engine
//! depends on no cpio tool at runtime.

use std::io::Write;

/// Packs the init binary as `/init` with `/dev/console`.
pub fn pack(init: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(init.len() + 1024);
    let mut ino = 1;
    let mut entry = |name: &str, mode: u32, rdev: (u32, u32), data: &[u8]| {
        let header = format!(
            "070701{ino:08x}{mode:08x}{uid:08x}{gid:08x}{nlink:08x}{mtime:08x}{size:08x}{devmajor:08x}{devminor:08x}{rdevmajor:08x}{rdevminor:08x}{namesize:08x}{check:08x}",
            uid = 0,
            gid = 0,
            nlink = 1,
            mtime = 0,
            size = data.len(),
            devmajor = 0,
            devminor = 0,
            rdevmajor = rdev.0,
            rdevminor = rdev.1,
            namesize = name.len() + 1,
            check = 0,
        );
        out.write_all(header.as_bytes()).expect("vec");
        out.write_all(name.as_bytes()).expect("vec");
        out.push(0);
        pad(&mut out);
        out.write_all(data).expect("vec");
        pad(&mut out);
        ino += 1;
    };
    entry("dev", 0o040755, (0, 0), &[]);
    entry("dev/console", 0o020600, (5, 1), &[]);
    entry("init", 0o100755, (0, 0), init);
    entry("TRAILER!!!", 0, (0, 0), &[]);
    out
}

fn pad(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The archive lists its three entries and the trailer in the newc
    /// layout: magic, a 110-byte header, the name, four-byte alignment.
    #[test]
    fn packs_a_newc_archive() {
        let archive = pack(b"#!/bin/sh\n");
        assert!(archive.starts_with(b"070701"));
        let text = String::from_utf8_lossy(&archive);
        assert!(text.contains("dev\0"));
        assert!(text.contains("dev/console\0"));
        assert!(text.contains("init\0"));
        assert!(text.contains("#!/bin/sh\n"));
        assert!(text.contains("TRAILER!!!\0"));
        assert!(archive.len().is_multiple_of(4));
        // The init's header states its size and mode.
        let at = archive.windows(4).position(|w| w == b"init").unwrap() - 110;
        let header = &text[at..at + 110];
        assert_eq!(&header[14..22], "000081ed");
        assert_eq!(&header[54..62], format!("{:08x}", 10));
    }
}
