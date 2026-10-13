use std::fs;

use crate::exif;

// Diagnostic: dump raw IFD0 tags + parse result for one JPEG, then EXIF date
// coverage over up to 400 JPEGs in that file's folder tree.
// Run with: photoapp.exe exifcheck <path-to-a.jpg>

fn hexu(v: usize) -> String {
    format!("0x{:04X}", v)
}

pub fn check() {
    let args: Vec<String> = std::env::args().collect();
    let Some(sample) = args.iter().skip_while(|a| *a != "exifcheck").nth(1).cloned() else {
        println!("usage: photoapp.exe exifcheck <path-to-a.jpg>");
        return;
    };
    let sample = sample.as_str();
    let sample_dir = std::path::Path::new(sample)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".into());
    let data = match fs::read(sample) {
        Ok(d) => d,
        Err(e) => {
            println!("cannot read {sample}: {e}");
            return;
        }
    };

    println!("file: {sample}  bytes: {}", data.len());
    println!("parse: {:?}", exif::parse(&data));

    // Raw walk: locate APP1, print IFD0 tags, look for 0x8769.
    let mut pos = 2usize;
    let mut tiff: &[u8] = &[];
    while pos + 4 <= data.len() {
        if data[pos] != 0xFF {
            println!("walk broke at {pos}: byte {:02X}", data[pos]);
            break;
        }
        let marker = data[pos + 1];
        let len = ((data[pos + 2] as usize) << 8) | data[pos + 3] as usize;
        if marker == 0xE1 {
            let seg = &data[pos + 4..pos + 4 + len.saturating_sub(2).min(data.len() - pos - 4)];
            println!("APP1 at {pos} len {len} starts_exif={}", seg.starts_with(b"Exif\0\0"));
            if seg.starts_with(b"Exif\0\0") {
                tiff = &seg[6..];
            }
            break;
        }
        if marker == 0xDA {
            println!("SOS at {pos} before any APP1");
            break;
        }
        println!("segment marker {:02X} len {len}", marker);
        pos += 2 + len;
    }

    if tiff.len() < 8 {
        println!("no TIFF block");
        return;
    }
    let e = if tiff[0..4] == [0x49, 0x49, 0x2A, 0x00] { "<" } else { ">" };
    println!("endian {e} tiff_len {}", tiff.len());

    fn u16(t: &[u8], off: usize, e: &str) -> usize {
        if off + 2 > t.len() { return 0; }
        if e == "<" { (t[off] as usize) | ((t[off + 1] as usize) << 8) } else { (t[off + 1] as usize) | ((t[off] as usize) << 8) }
    }
    fn u32(t: &[u8], off: usize, e: &str) -> usize {
        if off + 4 > t.len() { return 0; }
        if e == "<" {
            (t[off] as usize) | ((t[off + 1] as usize) << 8) | ((t[off + 2] as usize) << 16) | ((t[off + 3] as usize) << 24)
        } else {
            (t[off + 3] as usize) | ((t[off + 2] as usize) << 8) | ((t[off + 1] as usize) << 16) | ((t[off] as usize) << 24)
        }
    }

    let ifd0_off = u32(tiff, 4, e);
    let n = u16(tiff, ifd0_off, e);
    println!("IFD0 at {ifd0_off}, {n} entries");
    let mut has8769 = false;
    for k in 0..n {
        let ent = ifd0_off + 2 + k * 12;
        if ent + 12 > tiff.len() {
            println!("entry {k} out of bounds at {ent}");
            break;
        }
        let tag = u16(tiff, ent, e);
        let typ = u16(tiff, ent + 2, e);
        let count = u32(tiff, ent + 4, e);
        let val = u32(tiff, ent + 8, e);
        if tag == 0x010F || tag == 0x8769 || tag == 0x8825 {
            println!("  tag {} type {typ} count {count} val/offset {val}", hexu(tag));
            if tag == 0x8769 {
                has8769 = true;
                let sub = u32(tiff, ent + 8, e);
                println!("  subIFD at {sub}, entries {}", u16(tiff, sub, e));
                let sn = u16(tiff, sub, e);
                println!("    all sub tags:");
                for j in 0..sn {
                    let se = sub + 2 + j * 12;
                    if se + 12 > tiff.len() { break; }
                    let st = u16(tiff, se, e);
                    let sc = u32(tiff, se + 4, e);
                    let sv = u32(tiff, se + 8, e);
                    println!("      {} type {} count {} val {}", hexu(st), u16(tiff, se + 2, e), sc, sv);
                    if st == 0x0132 && sc <= 200 && sv + sc <= tiff.len() {
                        println!("        DateTimeOriginal = {}", String::from_utf8_lossy(&tiff[sv..sv + sc]));
                    }
                }
            }
        }
    }
    if !has8769 {
        println!("IFD0 has NO 0x8769 entry");
    }

    // Coverage over a sample.
    let mut tot = 0usize;
    let mut got = 0usize;
    for (dp, fns) in walk(&sample_dir) {
        for f in fns {
            let p = dp.join(f);
            let ext = p.extension().unwrap_or_default().to_string_lossy().to_lowercase();
            if ext != "jpg" && ext != "jpeg" { continue; }
            let d = match fs::read(&p) { Ok(d) => d, Err(_) => continue };
            let i = exif::parse(&d);
            tot += 1;
            if i.taken_at.is_some() { got += 1; }
            if tot >= 400 { break; }
        }
        if tot >= 400 { break; }
    }
    println!("sample {tot} jpgs, taken_at {got}");
}

fn walk(root: &str) -> Vec<(std::path::PathBuf, Vec<String>)> {
    let mut out = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(root)];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&d) {
            let mut files = Vec::new();
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() { stack.push(p); } else { files.push(p.file_name().unwrap_or_default().to_string_lossy().to_string()); }
            }
            out.push((d, files));
        }
    }
    out
}
