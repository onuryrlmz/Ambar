//! Writes a small 7z for trying the app by hand, since no 7z tool is assumed on the machine.
//! Usage: cargo run --example make_7z -- <out.7z> [password]

use sevenz_rust2::encoder_options::AesEncoderOptions;
use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderMethod, NtTime, Password};
use std::fs::File;

/// 2023-11-14 22:13:20 UTC.
const MODIFIED: u64 = 1_700_000_000;
const NT_TO_UNIX_SECS: u64 = 11_644_473_600;

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().expect("usage: make_7z <out.7z> [password]");
    let mut w = ArchiveWriter::new(File::create(out).unwrap()).unwrap();
    let mut methods = vec![];
    if let Some(p) = args.next() {
        methods.push(AesEncoderOptions::new(Password::new(&p)).into());
    }
    methods.push(EncoderMethod::LZMA2.into());
    w.set_content_methods(methods);
    w.push_archive_entry::<&[u8]>(ArchiveEntry::new_directory("belgeler"), None).unwrap();
    for (name, data) in [
        ("belgeler/öğrenci.txt", &b"merhaba 7z\n"[..]),
        ("kök.txt", &b"kok dosyasi\n"[..]),
    ] {
        let mut e = ArchiveEntry::new_file(name);
        e.has_last_modified_date = true;
        e.last_modified_date = NtTime::from((MODIFIED + NT_TO_UNIX_SECS) * 10_000_000);
        w.push_archive_entry(e, Some(data)).unwrap();
    }
    w.finish().unwrap();
}
