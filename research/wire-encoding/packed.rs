//! Prototype hand-packed encodings of the dominant table: compact regions.
//!
//! V0 row:    per region [lsize][dx][dy][dz][cell] as varints, in input order.
//! V1 column: same values, grouped by column.
//! V2 morton: regions sorted along a Morton curve relative to the world's grid
//!            origin; positions become one gap per region in curve space
//!            (0 for the dense, face-to-face leaves an octree produces).
//!            Anything that is not an octree-aligned pristine cube goes to an
//!            exception list with explicit coordinates, so the codec is lossless
//!            for every valid CellFragment.
use sauerteig_core::{Cell, CellFragment, FineBox, Shape};
use std::collections::HashMap;

pub struct W(pub Vec<u8>);
impl W {
    pub fn new() -> Self {
        W(Vec::new())
    }
    #[inline]
    pub fn u8(&mut self, v: u8) {
        self.0.push(v)
    }
    #[inline]
    pub fn var(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.0.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.0.push(v as u8);
    }
    #[inline]
    pub fn zz(&mut self, v: i64) {
        self.var(((v << 1) ^ (v >> 63)) as u64)
    }
}
pub struct R<'a> {
    pub b: &'a [u8],
    pub i: usize,
}
impl<'a> R<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        R { b, i: 0 }
    }
    #[inline]
    pub fn u8(&mut self) -> u8 {
        let v = self.b[self.i];
        self.i += 1;
        v
    }
    #[inline]
    pub fn var(&mut self) -> u64 {
        let mut shift = 0;
        let mut v = 0u64;
        loop {
            let b = self.b[self.i];
            self.i += 1;
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return v;
            }
            shift += 7;
        }
    }
    #[inline]
    pub fn zz(&mut self) -> i64 {
        let v = self.var();
        ((v >> 1) as i64) ^ -((v & 1) as i64)
    }
}

fn write_cell(w: &mut W, c: &Cell) {
    let mut flags = 0u8;
    if c.face_materials.is_some() {
        flags |= 1;
    }
    if c.diagonal {
        flags |= 2;
    }
    if c.shape.is_some() {
        flags |= 4;
    }
    if c.heights == [8; 4] {
        flags |= 8;
    }
    w.u8(flags);
    w.u8(c.material);
    if flags & 8 == 0 {
        for h in c.heights {
            w.u8(h);
        }
    }
    if let Some(f) = c.face_materials {
        for m in f {
            w.u8(m);
        }
    }
    if let Some(s) = c.shape {
        for v in s.vertices {
            w.u8(v[0] | v[1] << 4);
            w.u8(v[2]);
        }
        w.u8(s.diagonals);
    }
}
fn read_cell(r: &mut R) -> Cell {
    let flags = r.u8();
    let material = r.u8();
    let heights = if flags & 8 == 0 { [r.u8(), r.u8(), r.u8(), r.u8()] } else { [8; 4] };
    let face_materials = (flags & 1 != 0).then(|| [r.u8(), r.u8(), r.u8(), r.u8(), r.u8(), r.u8()]);
    let shape = (flags & 4 != 0).then(|| {
        let mut vertices = [[0u8; 3]; 8];
        for v in &mut vertices {
            let a = r.u8();
            *v = [a & 15, a >> 4, r.u8()];
        }
        Shape { vertices, diagonals: r.u8() }
    });
    Cell { material, face_materials, heights, diagonal: flags & 2 != 0, shape }
}

/// Palette of distinct cells, in first-use order.
struct Palette {
    cells: Vec<Cell>,
    index: HashMap<[u8; 64], u32>,
}
fn cell_key(c: &Cell) -> [u8; 64] {
    let mut w = W::new();
    write_cell(&mut w, c);
    let mut k = [0u8; 64];
    k[..w.0.len()].copy_from_slice(&w.0);
    k
}
impl Palette {
    fn new() -> Self {
        Palette { cells: Vec::new(), index: HashMap::new() }
    }
    fn id(&mut self, c: &Cell) -> u32 {
        let k = cell_key(c);
        if let Some(&i) = self.index.get(&k) {
            return i;
        }
        let i = self.cells.len() as u32;
        self.cells.push(*c);
        self.index.insert(k, i);
        i
    }
}

pub fn spread(v: u32) -> u64 {
    let mut x = v as u64 & 0x1f_ffff;
    x = (x | x << 32) & 0x1f00000000ffff;
    x = (x | x << 16) & 0x1f0000ff0000ff;
    x = (x | x << 8) & 0x100f00f00f00f00f;
    x = (x | x << 4) & 0x10c30c30c30c30c3;
    x = (x | x << 2) & 0x1249249249249249;
    x
}
#[inline]
pub fn compact(mut x: u64) -> u32 {
    x &= 0x1249249249249249;
    x = (x ^ (x >> 2)) & 0x10c30c30c30c30c3;
    x = (x ^ (x >> 4)) & 0x100f00f00f00f00f;
    x = (x ^ (x >> 8)) & 0x1f0000ff0000ff;
    x = (x ^ (x >> 16)) & 0x1f00000000ffff;
    x = (x ^ (x >> 32)) & 0x1f_ffff;
    x as u32
}
#[inline]
pub fn morton(c: [u32; 3]) -> u64 {
    spread(c[0]) | spread(c[1]) << 1 | spread(c[2]) << 2
}
#[inline]
pub fn unmorton(m: u64) -> [u32; 3] {
    [compact(m), compact(m >> 1), compact(m >> 2)]
}

fn pristine(f: &CellFragment) -> bool {
    f.bounds == FineBox { min: f.origin, max: f.origin.map(|v| v + f.size as i32) }
}

/// Grid-aligned pristine cube relative to `g`, as (morton code, log2 size).
fn aligned_key(f: &CellFragment, g: [i32; 3]) -> Option<(u64, u8)> {
    if !f.size.is_power_of_two() || !pristine(f) {
        return None;
    }
    let mut c = [0u32; 3];
    for a in 0..3 {
        let rel = f.origin[a].checked_sub(g[a])?;
        if rel < 0 || rel >= (1 << 21) || rel % f.size as i32 != 0 {
            return None;
        }
        c[a] = rel as u32;
    }
    Some((morton(c), f.size.trailing_zeros() as u8))
}

pub fn sort_morton(regions: &[CellFragment], g: [i32; 3]) -> Vec<CellFragment> {
    let mut keyed: Vec<(u64, u8, usize)> = Vec::new();
    let mut rest: Vec<usize> = Vec::new();
    for (i, f) in regions.iter().enumerate() {
        match aligned_key(f, g) {
            Some((m, k)) => keyed.push((m, k, i)),
            None => rest.push(i),
        }
    }
    keyed.sort_unstable();
    keyed.into_iter().map(|(_, _, i)| i).chain(rest).map(|i| regions[i].clone()).collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    Rows,
    Columns,
}

fn write_palette(w: &mut W, p: &Palette) {
    w.var(p.cells.len() as u64);
    for c in &p.cells {
        write_cell(w, c);
    }
}

fn write_exception(w: &mut W, f: &CellFragment, g: [i32; 3], pal: &mut Palette) {
    for a in 0..3 {
        w.zz(f.origin[a] as i64 - g[a] as i64);
    }
    w.var(f.size.trailing_zeros() as u64);
    let p = pristine(f);
    w.u8(!p as u8);
    if !p {
        for a in 0..3 {
            w.zz(f.bounds.min[a] as i64 - f.origin[a] as i64);
        }
        for a in 0..3 {
            w.zz(f.bounds.max[a] as i64 - f.origin[a] as i64);
        }
    }
    let id = pal.id(&f.cell);
    w.var(id as u64);
}

/// V2: `regions` need not be sorted; this sorts a copy.
pub fn pack_v2(regions: &[CellFragment], g: [i32; 3], layout: Layout) -> Vec<u8> {
    let sorted = sort_morton(regions, g);
    let mut pal = Palette::new();
    let mut main: Vec<(u64, u8, u32)> = Vec::new();
    let mut exc: Vec<&CellFragment> = Vec::new();
    for f in &sorted {
        match aligned_key(f, g) {
            Some((m, k)) => main.push((m, k, pal.id(&f.cell))),
            None => exc.push(f),
        }
    }
    let mut ex = W::new();
    for f in &exc {
        write_exception(&mut ex, f, g, &mut pal);
    }
    let mut out = W::new();
    out.var(main.len() as u64);
    out.var(exc.len() as u64);
    write_palette(&mut out, &pal);
    let mut prev_end = 0u64;
    match layout {
        Layout::Columns => {
            for &(_, k, _) in &main {
                out.u8(k);
            }
            for &(m, k, _) in &main {
                out.var(m - prev_end);
                prev_end = m + (1u64 << (3 * k as u32));
            }
            for &(_, _, c) in &main {
                out.var(c as u64);
            }
        }
        Layout::Rows => {
            for &(m, k, c) in &main {
                out.u8(k);
                out.var(m - prev_end);
                out.var(c as u64);
                prev_end = m + (1u64 << (3 * k as u32));
            }
        }
    }
    out.0.extend_from_slice(&ex.0);
    out.0
}

pub fn unpack_v2(b: &[u8], g: [i32; 3], layout: Layout) -> Vec<CellFragment> {
    let mut r = R::new(b);
    let n = r.var() as usize;
    let ne = r.var() as usize;
    let np = r.var() as usize;
    let pal: Vec<Cell> = (0..np).map(|_| read_cell(&mut r)).collect();
    let mut out = Vec::with_capacity(n + ne);
    let mut push = |m: u64, k: u8, c: u32, out: &mut Vec<CellFragment>| {
        let rel = unmorton(m);
        let size = 1i32 << k;
        let origin = [g[0] + rel[0] as i32, g[1] + rel[1] as i32, g[2] + rel[2] as i32];
        out.push(CellFragment {
            bounds: FineBox { min: origin, max: [origin[0] + size, origin[1] + size, origin[2] + size] },
            origin,
            size: size as u32,
            cell: pal[c as usize],
        });
    };
    match layout {
        Layout::Columns => {
            let ks = &b[r.i..r.i + n];
            r.i += n;
            let mut ms = Vec::with_capacity(n);
            let mut prev_end = 0u64;
            for &k in ks {
                let m = prev_end + r.var();
                ms.push(m);
                prev_end = m + (1u64 << (3 * k as u32));
            }
            for i in 0..n {
                let c = r.var() as u32;
                push(ms[i], ks[i], c, &mut out);
            }
        }
        Layout::Rows => {
            let mut prev_end = 0u64;
            for _ in 0..n {
                let k = r.u8();
                let m = prev_end + r.var();
                let c = r.var() as u32;
                prev_end = m + (1u64 << (3 * k as u32));
                push(m, k, c, &mut out);
            }
        }
    }
    for _ in 0..ne {
        let origin = [0, 1, 2].map(|a| (r.zz() + g[a] as i64) as i32);
        let size = 1u32 << r.var();
        let bounds = if r.u8() == 0 {
            FineBox { min: origin, max: origin.map(|v| v + size as i32) }
        } else {
            let mn = [0, 1, 2].map(|a| (r.zz() + origin[a] as i64) as i32);
            let mx = [0, 1, 2].map(|a| (r.zz() + origin[a] as i64) as i32);
            FineBox { min: mn, max: mx }
        };
        let c = r.var() as usize;
        out.push(CellFragment { bounds, origin, size, cell: pal[c] });
    }
    out
}

/// V0/V1: input order preserved, per-axis deltas of the origin.
pub fn pack_delta(regions: &[CellFragment], g: [i32; 3], layout: Layout) -> Vec<u8> {
    let mut pal = Palette::new();
    let mut out = W::new();
    let mut exc = W::new();
    let mut n_exc = 0u64;
    let mut rows: Vec<(u8, [i64; 3], u32)> = Vec::new();
    let mut prev = [0i64; 3];
    let mut kept: Vec<&CellFragment> = Vec::new();
    for f in regions {
        if !f.size.is_power_of_two() || !pristine(f) {
            write_exception(&mut exc, f, g, &mut pal);
            n_exc += 1;
            continue;
        }
        let o = [0, 1, 2].map(|a| f.origin[a] as i64 - g[a] as i64);
        let d = [o[0] - prev[0], o[1] - prev[1], o[2] - prev[2]];
        prev = o;
        rows.push((f.size.trailing_zeros() as u8, d, pal.id(&f.cell)));
        kept.push(f);
    }
    out.var(rows.len() as u64);
    out.var(n_exc);
    write_palette(&mut out, &pal);
    match layout {
        Layout::Rows => {
            for (k, d, c) in &rows {
                out.u8(*k);
                out.zz(d[0]);
                out.zz(d[1]);
                out.zz(d[2]);
                out.var(*c as u64);
            }
        }
        Layout::Columns => {
            for (k, _, _) in &rows {
                out.u8(*k);
            }
            for a in 0..3 {
                for (_, d, _) in &rows {
                    out.zz(d[a]);
                }
            }
            for (_, _, c) in &rows {
                out.var(*c as u64);
            }
        }
    }
    out.0.extend_from_slice(&exc.0);
    out.0
}

// ---------------------------------------------------------------------------
// Override layer: coarse cells, refined roots, empty masks, volumes.
// ---------------------------------------------------------------------------
use sauerteig_core::{CellEdit, MaterialVolume, RootEdit, VolumeMaterial};

fn cell_morton(pos: [i32; 3], g: [i32; 3]) -> Option<u64> {
    let mut c = [0u32; 3];
    for a in 0..3 {
        let rel = pos[a].checked_sub(g[a] / 8)?;
        if !(0..(1 << 21)).contains(&rel) {
            return None;
        }
        c[a] = rel as u32;
    }
    Some(morton(c))
}

/// Coarse one-metre overrides. `g` is the world's eighth-metre grid origin.
/// Palette index 0 means "no cell" (a deletion inside a patch).
pub fn pack_cells(cells: &[CellEdit], g: [i32; 3]) -> Vec<u8> {
    let mut keyed: Vec<(u64, &CellEdit)> = cells.iter().map(|c| (cell_morton(c.pos, g).expect("in range"), c)).collect();
    keyed.sort_unstable_by_key(|k| k.0);
    let mut pal = Palette::new();
    let mut ids = Vec::with_capacity(keyed.len());
    for (_, c) in &keyed {
        ids.push(c.cell.map_or(0, |cell| pal.id(&cell) + 1));
    }
    let mut out = W::new();
    out.var(keyed.len() as u64);
    write_palette(&mut out, &pal);
    let mut prev = 0u64;
    for (i, (m, _)) in keyed.iter().enumerate() {
        out.var(*m - prev);
        out.var(ids[i] as u64);
        prev = *m + 1;
    }
    out.0
}
pub fn unpack_cells(b: &[u8], g: [i32; 3]) -> Vec<CellEdit> {
    let mut r = R::new(b);
    let n = r.var() as usize;
    let np = r.var() as usize;
    let pal: Vec<Cell> = (0..np).map(|_| read_cell(&mut r)).collect();
    let mut out = Vec::with_capacity(n);
    let mut prev = 0u64;
    for _ in 0..n {
        let m = prev + r.var();
        let id = r.var() as usize;
        let c = unmorton(m);
        out.push(CellEdit {
            pos: [g[0] / 8 + c[0] as i32, g[1] / 8 + c[1] as i32, g[2] / 8 + c[2] as i32],
            cell: (id != 0).then(|| pal[id - 1]),
        });
        prev = m + 1;
    }
    out
}

/// Refined roots and empty masks. A fragment inside its root is 3 bytes of
/// nibble-packed bounds plus a palette index; sources other than the root's
/// own cube are spelled out.
pub fn pack_roots(roots: &[RootEdit], g: [i32; 3]) -> Vec<u8> {
    let mut keyed: Vec<(u64, &RootEdit)> = roots.iter().map(|r| (cell_morton(r.pos, g).expect("in range"), r)).collect();
    keyed.sort_unstable_by_key(|k| k.0);
    let mut pal = Palette::new();
    let mut body = W::new();
    let mut prev = 0u64;
    for (m, r) in &keyed {
        body.var(*m - prev);
        prev = *m + 1;
        body.var(r.fragments.len() as u64);
        let root_min = r.pos.map(|v| v * 8);
        for f in &r.fragments {
            let rel_min = [0, 1, 2].map(|a| (f.bounds.min[a] - root_min[a]) as u8);
            let rel_max = [0, 1, 2].map(|a| (f.bounds.max[a] - root_min[a]) as u8);
            let own = f.origin == root_min && f.size == 8;
            body.u8(rel_min[0] | rel_min[1] << 4);
            body.u8(rel_min[2] | rel_max[0] << 4);
            body.u8(rel_max[1] | rel_max[2] << 4);
            body.var(((pal.id(&f.cell) as u64) << 1) | (!own) as u64);
            if !own {
                for a in 0..3 {
                    body.zz(f.origin[a] as i64 - root_min[a] as i64);
                }
                body.var(f.size.trailing_zeros() as u64);
            }
        }
    }
    let mut out = W::new();
    out.var(keyed.len() as u64);
    write_palette(&mut out, &pal);
    out.0.extend_from_slice(&body.0);
    out.0
}
pub fn unpack_roots(b: &[u8], g: [i32; 3]) -> Vec<RootEdit> {
    let mut r = R::new(b);
    let n = r.var() as usize;
    let np = r.var() as usize;
    let pal: Vec<Cell> = (0..np).map(|_| read_cell(&mut r)).collect();
    let mut out = Vec::with_capacity(n);
    let mut prev = 0u64;
    for _ in 0..n {
        let m = prev + r.var();
        prev = m + 1;
        let c = unmorton(m);
        let pos = [g[0] / 8 + c[0] as i32, g[1] / 8 + c[1] as i32, g[2] / 8 + c[2] as i32];
        let root_min = pos.map(|v| v * 8);
        let nf = r.var() as usize;
        let mut fragments = Vec::with_capacity(nf);
        for _ in 0..nf {
            let (a, b2, c2) = (r.u8(), r.u8(), r.u8());
            let rel_min = [a & 15, b2 & 15, a >> 4];
            let rel_min = [rel_min[0], rel_min[2], b2 & 15];
            let rel_max = [b2 >> 4, c2 & 15, c2 >> 4];
            let bounds = FineBox {
                min: [0, 1, 2].map(|i| root_min[i] + rel_min[i] as i32),
                max: [0, 1, 2].map(|i| root_min[i] + rel_max[i] as i32),
            };
            let tagged = r.var();
            let cell = pal[(tagged >> 1) as usize];
            let (origin, size) = if tagged & 1 == 0 {
                (root_min, 8)
            } else {
                let o = [0, 1, 2].map(|i| (r.zz() + root_min[i] as i64) as i32);
                (o, 1u32 << r.var())
            };
            fragments.push(CellFragment { bounds, origin, size, cell });
        }
        out.push(RootEdit { pos, fragments });
    }
    out
}

pub fn pack_volumes(vols: &[MaterialVolume], g: [i32; 3]) -> Vec<u8> {
    let mut v: Vec<&MaterialVolume> = vols.iter().collect();
    v.sort_by_key(|x| {
        let c = [0, 1, 2].map(|a| (x.bounds.min[a] - g[a]).max(0) as u32);
        morton(c)
    });
    let mut out = W::new();
    out.var(v.len() as u64);
    let mut prev = [0i64; 3];
    for x in v {
        let mn = [0, 1, 2].map(|a| x.bounds.min[a] as i64 - g[a] as i64);
        for a in 0..3 {
            out.zz(mn[a] - prev[a]);
        }
        prev = mn;
        for a in 0..3 {
            out.var((x.bounds.max[a] - x.bounds.min[a]) as u64);
        }
        out.u8(match x.material {
            VolumeMaterial::Water => 0,
            VolumeMaterial::Glass => 1,
        });
    }
    out.0
}
pub fn unpack_volumes(b: &[u8], g: [i32; 3]) -> Vec<MaterialVolume> {
    let mut r = R::new(b);
    let n = r.var() as usize;
    let mut out = Vec::with_capacity(n);
    let mut prev = [0i64; 3];
    for _ in 0..n {
        let mut mn = [0i64; 3];
        for a in 0..3 {
            mn[a] = prev[a] + r.zz();
        }
        prev = mn;
        let sz = [r.var() as i64, r.var() as i64, r.var() as i64];
        let min = [0, 1, 2].map(|a| (mn[a] + g[a] as i64) as i32);
        let max = [0, 1, 2].map(|a| (mn[a] + sz[a] + g[a] as i64) as i32);
        let material = if r.u8() == 0 { VolumeMaterial::Water } else { VolumeMaterial::Glass };
        out.push(MaterialVolume { bounds: FineBox { min, max }, material });
    }
    out
}
