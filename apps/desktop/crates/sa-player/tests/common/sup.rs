//! A minimal HDMV PGS (`.sup`) writer for test media: FFmpeg has a PGS decoder and a `sup` demuxer but no PGS
//! encoder, so the bitmap subtitle tests build their PGS tracks here, with known pixels, palette and timings, and mux
//! them with the FFmpeg CLI (which also transcodes them to VobSub, DVB and XSUB, bitmap to bitmap).
//!
//! A display set is PCS (presentation composition), WDS (windows), PDS (palette), ODS (objects) and END; a set that
//! composes no object clears the screen. [`display_set`] writes the plain case (an epoch start that carries its
//! palette and every object again, each object in a window of its own); [`DisplaySet`] writes anything else: normal
//! and acquisition-point sets, a palette-only update (`palette_update_flag`, how fades are done), compositions that
//! reuse an object decoded earlier, and objects of any size, split over as many ODS segments as their data needs (a
//! segment holds at most 65535 bytes). Objects are run-length coded the PGS way (`rle_line`). Every length field is
//! checked: a value that does not fit panics instead of wrapping.

/// One object: `rows` are palette indices (0 = transparent), shown at `(x, y)` on the canvas by [`display_set`].
pub struct Object {
    pub x: u16,
    pub y: u16,
    pub rows: Vec<Vec<u8>>,
}

impl Object {
    pub fn filled(x: u16, y: u16, w: usize, h: usize, index: u8) -> Self {
        Self { x, y, rows: vec![vec![index; w]; h] }
    }

    /// A `w` × `h` checkerboard of indices `a` and `b` (`a` at the top left): one byte of RLE per pixel when both are
    /// non-zero, so large objects need several ODS segments.
    pub fn checkerboard(x: u16, y: u16, w: usize, h: usize, a: u8, b: u8) -> Self {
        Self { x, y, rows: (0..h).map(|r| (0..w).map(|c| if (r + c) % 2 == 0 { a } else { b }).collect()).collect() }
    }

    fn width(&self) -> u16 {
        u16::try_from(self.rows[0].len()).expect("object width fits 16 bits")
    }

    fn height(&self) -> u16 {
        u16::try_from(self.rows.len()).expect("object height fits 16 bits")
    }

    fn rle(&self) -> Vec<u8> {
        let mut rle = Vec::new();
        for row in &self.rows {
            rle_line(row, &mut rle);
        }
        rle
    }
}

/// A palette entry: index, Y, Cr, Cb (limited range), alpha.
pub type PaletteEntry = (u8, u8, u8, u8, u8);

/// The composition state of a display set.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Updates the current epoch: objects and palettes decoded earlier stay usable.
    Normal = 0x00,
    AcquisitionPoint = 0x40,
    /// A new epoch: everything decoded before is forgotten.
    EpochStart = 0x80,
}

/// A window: `(id, x, y, width, height)`.
pub type Window = (u8, u16, u16, u16, u16);

/// One composition: object `object` shown in window `window` at `(x, y)`.
pub struct Composition {
    pub object: u16,
    pub window: u8,
    pub x: u16,
    pub y: u16,
}

/// A display set with every field chosen.
pub struct DisplaySet<'a> {
    /// The PCS's time (s): when the set is shown.
    pub pts: f64,
    /// The time of the set's other segments (WDS, PDS, ODS, END); `None`: `pts`.
    pub segments_pts: Option<f64>,
    pub canvas: (u16, u16),
    pub composition_number: u16,
    pub state: State,
    pub palette_update: bool,
    pub palette_id: u8,
    pub compositions: Vec<Composition>,
    /// `None`: no WDS.
    pub windows: Option<Vec<Window>>,
    /// `(palette id, version, entries)`; `None`: no PDS.
    pub palette: Option<(u8, u8, &'a [PaletteEntry])>,
    /// `(object id, version, object)` sent with this set (their `x`/`y` are not used: `compositions` place them).
    pub objects: Vec<(u16, u8, &'a Object)>,
}

fn segment(out: &mut Vec<u8>, pts: f64, kind: u8, data: &[u8]) {
    let t = u32::try_from((pts * 90_000.0).round() as i64).expect("pts fits 32 bits of 90 kHz");
    let len = u16::try_from(data.len()).unwrap_or_else(|_| panic!("a PGS segment holds at most 65535 bytes, not {}", data.len()));
    out.extend_from_slice(b"PG");
    out.extend_from_slice(&t.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(&len.to_be_bytes());
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

/// The ODS segments of one object: the first carries the 24-bit data length (RLE plus the 4 size bytes) and the
/// size, the others continue the RLE; first / last in sequence flags `0x80` / `0x40`.
fn object_segments(id: u16, version: u8, o: &Object) -> Vec<Vec<u8>> {
    let rle = o.rle();
    let n = rle.len() + 4;
    assert!(n < 1 << 24, "object data of {n} bytes does not fit the 24-bit length");
    const MAX: usize = u16::MAX as usize;
    // id (2) + version (1) + flags (1), then the length (3) and the size (4) in the first segment only.
    let (first_room, room) = (MAX - 11, MAX - 4);
    let mut chunks = vec![&rle[..rle.len().min(first_room)]];
    let mut rest = &rle[chunks[0].len()..];
    while !rest.is_empty() {
        let (c, r) = rest.split_at(rest.len().min(room));
        chunks.push(c);
        rest = r;
    }
    let last = chunks.len() - 1;
    chunks
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut ods = Vec::with_capacity(c.len() + 11);
            ods.extend_from_slice(&id.to_be_bytes());
            let flags = if i == 0 { 0x80 } else { 0 } | if i == last { 0x40 } else { 0 };
            ods.extend_from_slice(&[version, flags]);
            if i == 0 {
                ods.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
                ods.extend_from_slice(&o.width().to_be_bytes());
                ods.extend_from_slice(&o.height().to_be_bytes());
            }
            ods.extend_from_slice(c);
            ods
        })
        .collect()
}

/// The number of ODS segments `o` takes.
pub fn object_segment_count(o: &Object) -> usize {
    object_segments(0, 0, o).len()
}

/// Appends a display set.
pub fn write(out: &mut Vec<u8>, set: &DisplaySet) {
    let mut pcs = Vec::new();
    pcs.extend_from_slice(&set.canvas.0.to_be_bytes());
    pcs.extend_from_slice(&set.canvas.1.to_be_bytes());
    // Frame rate code, composition number, composition state, palette update flag, palette id, object count.
    pcs.push(0x10);
    pcs.extend_from_slice(&set.composition_number.to_be_bytes());
    let count = u8::try_from(set.compositions.len()).expect("at most 255 compositions");
    pcs.extend_from_slice(&[set.state as u8, if set.palette_update { 0x80 } else { 0 }, set.palette_id, count]);
    for c in &set.compositions {
        pcs.extend_from_slice(&c.object.to_be_bytes());
        // Window id, not cropped.
        pcs.extend_from_slice(&[c.window, 0]);
        pcs.extend_from_slice(&c.x.to_be_bytes());
        pcs.extend_from_slice(&c.y.to_be_bytes());
    }
    segment(out, set.pts, 0x16, &pcs);
    let at = set.segments_pts.unwrap_or(set.pts);
    if let Some(windows) = &set.windows {
        let mut wds = vec![u8::try_from(windows.len()).expect("at most 255 windows")];
        for &(id, x, y, w, h) in windows {
            wds.push(id);
            for v in [x, y, w, h] {
                wds.extend_from_slice(&v.to_be_bytes());
            }
        }
        segment(out, at, 0x17, &wds);
    }
    if let Some((id, version, entries)) = set.palette {
        let mut pds = vec![id, version];
        for &(i, y, cr, cb, a) in entries {
            pds.extend_from_slice(&[i, y, cr, cb, a]);
        }
        segment(out, at, 0x14, &pds);
    }
    for &(id, version, o) in &set.objects {
        for ods in object_segments(id, version, o) {
            segment(out, at, 0x15, &ods);
        }
    }
    segment(out, at, 0x80, &[]);
}

/// Appends an epoch start shown from `pts` (s) on a `width` × `height` canvas: every object in a window of its own
/// at its place, palette 0 (version 0) and the objects sent with it; without objects the set clears the screen.
pub fn display_set(out: &mut Vec<u8>, pts: f64, canvas: (u16, u16), objects: &[Object], palette: &[PaletteEntry]) {
    let id = |i: usize| u16::try_from(i).expect("object id");
    write(
        out,
        &DisplaySet {
            pts,
            segments_pts: None,
            canvas,
            composition_number: 0,
            state: State::EpochStart,
            palette_update: false,
            palette_id: 0,
            compositions: objects.iter().enumerate().map(|(i, o)| Composition { object: id(i), window: i as u8, x: o.x, y: o.y }).collect(),
            windows: Some(objects.iter().enumerate().map(|(i, o)| (i as u8, o.x, o.y, o.width(), o.height())).collect()),
            palette: (!objects.is_empty()).then_some((0, 0, palette)),
            objects: objects.iter().enumerate().map(|(i, o)| (id(i), 0, o)).collect(),
        },
    );
}
