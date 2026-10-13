// Minimal EXIF parser: JPEG APP1 segment -> TIFF header -> IFD0 + Exif subIFD + GPS IFD.
// Returns (taken_at, camera, width, height, gps_lat, gps_lon).

#[derive(Debug, Default)]
pub struct ExifInfo {
    pub taken_at: Option<String>,
    pub camera: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
}

pub fn parse(data: &[u8]) -> ExifInfo {
    let mut info = ExifInfo::default();
    if data.len() < 8 || data[0..2] != [0xFF, 0xD8] { return info; }

    // Walk JPEG segments to find APP1 with "Exif\0\0"
    let mut pos = 2usize;
    let mut tiff: &[u8] = &[];
    while pos + 4 <= data.len() {
        if data[pos] != 0xFF { break; }
        let marker = data[pos + 1];
        if marker == 0xE1 {
            let len = ((data[pos + 2] as usize) << 8) | data[pos + 3] as usize;
            let seg = &data[pos + 4..pos + 4 + len.saturating_sub(2).min(data.len() - pos - 4)];
            if seg.starts_with(b"Exif\0\0") { tiff = &seg[6..]; break; }
            pos += 2 + len;
        } else if marker == 0xDA { break; } // start of scan
        else {
            let len = ((data[pos + 2] as usize) << 8) | data[pos + 3] as usize;
            pos += 2 + len;
        }
    }
    if tiff.len() < 8 { return info; }

    let endian = if tiff[0..4] == [0x49, 0x49, 0x2A, 0x00] { "<" } else if tiff[0..4] == [0x4D, 0x4D, 0x00, 0x2A] { ">" } else { return info };

    fn u16(t: &[u8], off: usize, e: &str) -> usize {
        if off + 2 > t.len() { return 0; }
        let b = [t[off], t[off + 1]];
        if e == "<" { (b[0] as usize) | ((b[1] as usize) << 8) } else { (b[1] as usize) | ((b[0] as usize) << 8) }
    }
    fn u32(t: &[u8], off: usize, e: &str) -> usize {
        if off + 4 > t.len() { return 0; }
        let b = [t[off], t[off + 1], t[off + 2], t[off + 3]];
        if e == "<" { (b[0] as usize) | ((b[1] as usize) << 8) | ((b[2] as usize) << 16) | ((b[3] as usize) << 24) }
        else { (b[3] as usize) | ((b[2] as usize) << 8) | ((b[1] as usize) << 16) | ((b[0] as usize) << 24) }
    }

    // Read an IFD into a map of tag -> entry offset (entry base needed for inline values)
    fn read_ifd(t: &[u8], off: usize, e: &str) -> std::collections::HashMap<usize, usize> {
        let mut out = std::collections::HashMap::new();
        if off + 2 > t.len() { return out; }
        let n = u16(t, off, e);
        for k in 0..n {
            let ent = off + 2 + k * 12;
            if ent + 12 > t.len() { break; }
            out.insert(u16(t, ent, e), ent);
        }
        out
    }

    fn entry_str(t: &[u8], ent: usize, e: &str) -> Option<String> {
        let count = u32(t, ent + 4, e);
        if count == 0 || count > 200 { return None; }
        // count <= 4: bytes inline at ent+8; otherwise ent+8 is an offset
        let start = if count <= 4 { ent + 8 } else { u32(t, ent + 8, e) };
        if start + count > t.len() { return None; }
        let s = String::from_utf8_lossy(&t[start..start + count]).trim_end_matches('\0').trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }

    // Value field for single-value entries, read with the field's actual type.
    // A SHORT (type 3) occupies only the first 2 bytes of the 4-byte value field,
    // so reading it as u32 gives a wrong number. LONG (type 4) is the full 4 bytes.
    fn entry_val(t: &[u8], ent: usize, e: &str) -> usize {
        let typ = u16(t, ent + 2, e);
        match typ {
            3 => u16(t, ent + 8, e),
            4 => u32(t, ent + 8, e),
            _ => u32(t, ent + 8, e),
        }
    }


    let ifd0 = read_ifd(tiff, u32(tiff, 4, endian), endian);

    // Model (0x010F) lives in IFD0
    if let Some(&ent) = ifd0.get(&0x010F) { info.camera = entry_str(tiff, ent, endian); }

    // Exif subIFD (0x8769): DateTimeOriginal is 0x9003 (0x0132 is IFD0 DateTime),
    // ExifImageWidth/Height are 0x1000/0x1001 (SHORT, must use entry_val).
    if let Some(&ent) = ifd0.get(&0x8769) {
        let exif = read_ifd(tiff, u32(tiff, ent + 8, endian), endian);
        if let Some(&e) = exif.get(&0x9003) { info.taken_at = entry_str(tiff, e, endian); }
        // 0x1000/0x1001 are only trustworthy when typed SHORT/LONG; some writers
        // store them as UNDEFINED(7) holding ASCII text, which reads as garbage.
        if let Some(&e) = exif.get(&0x1000) {
            let typ = u16(tiff, e + 2, endian);
            if typ == 3 || typ == 4 { info.width = Some(entry_val(tiff, e, endian) as i64); }
        }
        if let Some(&e) = exif.get(&0x1001) {
            let typ = u16(tiff, e + 2, endian);
            if typ == 3 || typ == 4 { info.height = Some(entry_val(tiff, e, endian) as i64); }
        }
        // PixelXDimension/PixelYDimension (0xA002/0xA003) — many cameras write only these.
        if info.width.is_none() {
            if let Some(&e) = exif.get(&0xA002) {
                let typ = u16(tiff, e + 2, endian);
                if typ == 3 || typ == 4 { info.width = Some(entry_val(tiff, e, endian) as i64); }
            }
        }
        if info.height.is_none() {
            if let Some(&e) = exif.get(&0xA003) {
                let typ = u16(tiff, e + 2, endian);
                if typ == 3 || typ == 4 { info.height = Some(entry_val(tiff, e, endian) as i64); }
            }
        }
    }
    // Fallback: IFD0 DateTime (0x0132) when the subIFD has no DateTimeOriginal.
    if info.taken_at.is_none() {
        if let Some(&ent) = ifd0.get(&0x0132) { info.taken_at = entry_str(tiff, ent, endian); }
    }

    // GPS IFD (0x8825): lat tag 1, lon tag 2 (three rationals each);
    // ref tags 2/4 are ASCII count<=4, inline at ent+8 ('N'/'S', 'E'/'W').
    if let Some(&ent) = ifd0.get(&0x8825) {
        let gps = read_ifd(tiff, u32(tiff, ent + 8, endian), endian);
        fn dms(t: &[u8], ent: usize, e: &str) -> Option<f64> {
            let off = u32(t, ent + 8, e);
            if off + 24 > t.len() { return None; }
            let d = u32(t, off, e) as f64; let dn = u32(t, off + 4, e) as f64;
            let m = u32(t, off + 8, e) as f64; let mn = u32(t, off + 12, e) as f64;
            let s = u32(t, off + 16, e) as f64; let sn = u32(t, off + 20, e) as f64;
            if dn == 0.0 || mn == 0.0 || sn == 0.0 { return None; }
            Some(d / dn + m / mn / 60.0 + s / sn / 3600.0)
        }
        if let Some(&e) = gps.get(&1) {
            if let Some(v) = dms(tiff, e, endian) {
                let sign = match gps.get(&2).and_then(|&re| tiff.get(re + 8)) { Some(b'S') => -1.0, _ => 1.0 };
                info.gps_lat = Some(v * sign);
            }
        }
        if let Some(&e) = gps.get(&2) {
            if let Some(v) = dms(tiff, e, endian) {
                let sign = match gps.get(&4).and_then(|&re| tiff.get(re + 8)) { Some(b'W') => -1.0, _ => 1.0 };
                info.gps_lon = Some(v * sign);
            }
        }
    }

    info
}
