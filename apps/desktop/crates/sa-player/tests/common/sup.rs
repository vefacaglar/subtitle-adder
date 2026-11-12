//! A minimal HDMV PGS (`.sup`) writer for test media: FFmpeg has a PGS decoder and a `sup` demuxer but no PGS
//! encoder, so the bitmap subtitle tests build their PGS track here, with known pixels, palette and timings, and mux
//! it with the FFmpeg CLI (which also transcodes it to VobSub and DVB, bitmap to bitmap).
//!
//! A display set is PCS (presentation composition), WDS (windows), PDS (palette), one ODS (object) per object and
//! END; a set without objects clears the screen. Every set here is an epoch start that carries its palette and
//! objects again. Objects are run-length coded the PGS way (`rle_line`).

/// One object of a display set: shown at `(x, y)` on the canvas; `rows` are palette indices (0 = transparent).
pub struct Object {
    pub x: u16,
    pub y: u16,
    pub rows: Vec<Vec<u8>>,
}

impl Object {
    pub fn filled(x: u16, y: u16, w: usize, h: usize, index: u8) -> Self {
        Self { x, y, rows: vec![vec![index; w]; h] }
    }
}

/// A palette entry: index, Y, Cr, Cb (limited range), alpha.
pub type PaletteEntry = (u8, u8, u8, u8, u8);

fn segment(out: &mut Vec<u8>, pts: f64, kind: u8, data: &[u8]) {
    let t = (pts * 90_000.0).round() as u32;
    out.extend_from_slice(b"PG");
    out.extend_from_slice(&t.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
}

/// One row: a non-zero colour alone (`C`, runs of one or two), runs `00 0L` / `00 4L LL` of colour 0, `00 8L C` /
/// `00 CL LL C` of colour C, and the end of line `00 00`.
pub fn rle_line(pixels: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < pixels.len() {
        let c = pixels[i];
        let mut n = 1;
        while i + n < pixels.len() && pixels[i + n] == c && n < 16383 {
            n += 1;
        }
        match (c, n) {
            (c, 1 | 2) if c != 0 => out.extend(std::iter::repeat_n(c, n)),
            (0, n) if n < 64 => out.extend_from_slice(&[0, n as u8]),
            (0, n) => out.extend_from_slice(&[0, 0x40 | (n >> 8) as u8, n as u8]),
            (c, n) if n < 64 => out.extend_from_slice(&[0, 0x80 | n as u8, c]),
            (c, n) => out.extend_from_slice(&[0, 0xC0 | (n >> 8) as u8, n as u8, c]),
        }
        i += n;
    }
    out.extend_from_slice(&[0, 0]);
}

/// Appends a display set shown from `pts` (s) on a `width` × `height` canvas.
pub fn display_set(out: &mut Vec<u8>, pts: f64, (width, height): (u16, u16), objects: &[Object], palette: &[PaletteEntry]) {
    let mut pcs = Vec::new();
    pcs.extend_from_slice(&width.to_be_bytes());
    pcs.extend_from_slice(&height.to_be_bytes());
    // Frame rate code, composition number, composition state (epoch start), palette update flag, palette id.
    pcs.extend_from_slice(&[0x10, 0, 0, 0x80, 0, 0, objects.len() as u8]);
    for (id, o) in objects.iter().enumerate() {
        pcs.extend_from_slice(&(id as u16).to_be_bytes());
        // Window id, not cropped.
        pcs.extend_from_slice(&[id as u8, 0]);
        pcs.extend_from_slice(&o.x.to_be_bytes());
        pcs.extend_from_slice(&o.y.to_be_bytes());
    }
    segment(out, pts, 0x16, &pcs);
    let mut wds = vec![objects.len() as u8];
    for (id, o) in objects.iter().enumerate() {
        wds.push(id as u8);
        for v in [o.x, o.y, o.rows[0].len() as u16, o.rows.len() as u16] {
            wds.extend_from_slice(&v.to_be_bytes());
        }
    }
    segment(out, pts, 0x17, &wds);
    if !objects.is_empty() {
        let mut pds = vec![0, 0];
        for &(i, y, cr, cb, a) in palette {
            pds.extend_from_slice(&[i, y, cr, cb, a]);
        }
        segment(out, pts, 0x14, &pds);
        for (id, o) in objects.iter().enumerate() {
            let mut rle = Vec::new();
            for row in &o.rows {
                rle_line(row, &mut rle);
            }
            let n = rle.len() + 4;
            let mut ods = Vec::new();
            ods.extend_from_slice(&(id as u16).to_be_bytes());
            // Object version, first and last in sequence, data length (24 bits, with the size).
            ods.extend_from_slice(&[0, 0xC0, (n >> 16) as u8, (n >> 8) as u8, n as u8]);
            ods.extend_from_slice(&(o.rows[0].len() as u16).to_be_bytes());
            ods.extend_from_slice(&(o.rows.len() as u16).to_be_bytes());
            ods.extend_from_slice(&rle);
            segment(out, pts, 0x15, &ods);
        }
    }
    segment(out, pts, 0x80, &[]);
}
